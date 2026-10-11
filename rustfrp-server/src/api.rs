//! 管理 REST API（`/api/v1`）与面板静态托管。
//!
//! 安全要点：HttpOnly + SameSite=Strict 会话 Cookie；非安全方法必须携带 `X-CSRF-Token` 并通过同源检查；
//! 登录要求自定义请求头并限流；所有写操作要求非空 `reason` 并与业务写入同事务落审计；
//! 权限不足返回 403 且写入审计。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{self, token_hash};
use crate::config::Config;
use crate::db::{Db, StoreError};
use crate::hub::Hub;
use crate::models::{Admin, Role};
use crate::store::{self, NewTunnel, TunnelPatch};

/// 会话 Cookie 名。
pub const COOKIE: &str = "rfrp_session";
const MAX_REASON: usize = 200;

/// 共享状态。
#[derive(Clone)]
pub struct AppState {
    /// 数据库。
    pub db: Db,
    /// Hub。
    pub hub: Arc<Hub>,
    /// 配置。
    pub cfg: Arc<Config>,
    /// 登录限流。
    pub limiter: Arc<LoginLimiter>,
}

// ───────────── 错误 ─────────────

/// API 错误：统一渲染为 `{"error":{"code","message"}}`。
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "login required")
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found"),
            StoreError::Conflict(m) => ApiError::new(StatusCode::CONFLICT, "conflict", m),
            StoreError::Invalid(m) => ApiError::bad(m),
            // 数据库内部错误不对外暴露细节。
            StoreError::Sqlite(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": {"code": self.code, "message": self.message}})),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

// ───────────── 登录限流 ─────────────

/// 登录失败限流：同一来源在窗口内失败次数过多则暂时拒绝。
pub struct LoginLimiter {
    max: u32,
    window_ms: i64,
    map: Mutex<HashMap<String, (u32, i64)>>,
}

impl LoginLimiter {
    /// 创建限流器。
    pub fn new(max: u32, window_ms: i64) -> Self {
        Self {
            max,
            window_ms,
            map: Mutex::default(),
        }
    }

    /// 当前是否允许尝试登录。
    pub fn allowed(&self, ip: &str, now: i64) -> bool {
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        match m.get(ip) {
            Some((n, start)) if now - start < self.window_ms => *n < self.max,
            Some(_) => {
                m.remove(ip);
                true
            }
            None => true,
        }
    }

    /// 记录一次失败。
    pub fn fail(&self, ip: &str, now: i64) {
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        let e = m.entry(ip.to_string()).or_insert((0, now));
        if now - e.1 >= self.window_ms {
            *e = (0, now);
        }
        e.0 += 1;
        // 条目数封顶，防止被海量来源撑大内存。
        if m.len() > 10_000 {
            m.retain(|_, (_, start)| now - *start < self.window_ms);
        }
    }

    /// 登录成功后清除记录。
    pub fn clear(&self, ip: &str) {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(ip);
    }
}

// ───────────── 提取器 ─────────────

fn client_ip(parts: &Parts) -> String {
    // 只信任 TCP 对端地址；不读取 X-Forwarded-For / X-Real-IP（可被客户端伪造以绕过限流）。
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_string(), |c| c.0.ip().to_string())
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then_some(v)
        })
}

/// 同源检查：带 Origin 头时，其主机必须等于 Host。
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    origin
        .split_once("://")
        .is_some_and(|(_, rest)| rest.trim_end_matches('/') == host)
}

fn is_safe(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// 已登录的管理员（已通过会话、CSRF、同源校验）。
pub struct Auth {
    /// 管理员。
    pub admin: Admin,
    /// 来源 IP。
    pub ip: String,
    token_hash: String,
    csrf: String,
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = cookie_value(&parts.headers, COOKIE).ok_or_else(ApiError::unauthorized)?;
        let th = token_hash(token);
        let h2 = th.clone();
        let (admin, csrf) = state
            .db
            .call(move |c| store::find_session(c, &h2, store::now_ms()))
            .await
            .map_err(|e| {
                if e == StoreError::NotFound {
                    ApiError::unauthorized()
                } else {
                    e.into()
                }
            })?;
        if !is_safe(&parts.method) {
            if !same_origin(&parts.headers) {
                return Err(ApiError::forbidden("cross-origin request rejected"));
            }
            let sent = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if sent.is_empty() || sent != csrf {
                return Err(ApiError::forbidden("missing or invalid CSRF token"));
            }
        }
        Ok(Auth {
            admin,
            ip: client_ip(parts),
            token_hash: th,
            csrf,
        })
    }
}

impl Auth {
    /// 要求至少某个角色；不足时记审计并返回 403。
    async fn require(&self, st: &AppState, min: Role) -> Result<(), ApiError> {
        if self.admin.role >= min {
            return Ok(());
        }
        let (actor, ip) = (self.admin.name.clone(), self.ip.clone());
        let _ = st
            .db
            .call(move |c| {
                store::audit(
                    c,
                    &actor,
                    "permission.denied",
                    "api",
                    "",
                    "insufficient role",
                    "denied",
                    &ip,
                    store::now_ms(),
                )
            })
            .await;
        Err(ApiError::forbidden("insufficient permissions"))
    }
}

fn reason(s: &str) -> Result<String, ApiError> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > MAX_REASON {
        return Err(ApiError::bad(format!(
            "reason is required (1-{MAX_REASON} characters)"
        )));
    }
    Ok(t.to_string())
}

#[derive(Deserialize, Default)]
struct ReasonQuery {
    #[serde(default)]
    reason: String,
}

// ───────────── 认证 ─────────────

#[derive(Deserialize)]
struct LoginBody {
    name: String,
    password: String,
}

fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| auth::hash_password("rustfrp-dummy-password").unwrap_or_default())
}

async fn login(
    State(st): State<AppState>,
    parts: Parts,
    Json(body): Json<LoginBody>,
) -> Result<Response, ApiError> {
    if parts.headers.get("x-rustfrp-admin").is_none() || !same_origin(&parts.headers) {
        return Err(ApiError::forbidden(
            "login must come from the panel (same origin)",
        ));
    }
    let ip = client_ip(&parts);
    let now = store::now_ms();
    if !st.limiter.allowed(&ip, now) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many failed attempts, try again later",
        ));
    }
    let name = body.name.trim().to_string();
    let n2 = name.clone();
    let found = st
        .db
        .call(move |c| store::find_admin_with_hash(c, &n2))
        .await;
    // 账号不存在时也做一次等耗时的哈希校验，避免通过响应时间枚举用户名。
    let (admin, ok) = match found {
        Ok((a, h)) => {
            let ok = auth::verify_password(&body.password, &h) && a.disabled_at.is_none();
            (Some(a), ok)
        }
        Err(_) => {
            let _ = auth::verify_password(&body.password, dummy_hash());
            (None, false)
        }
    };
    let (actor, ip2) = (name.clone(), ip.clone());
    let result = if ok { "ok" } else { "denied" };
    let _ = st
        .db
        .call(move |c| {
            store::audit(
                c,
                &actor,
                "auth.login",
                "admin",
                &actor,
                "",
                result,
                &ip2,
                store::now_ms(),
            )
        })
        .await;
    let Some(admin) = admin.filter(|_| ok) else {
        st.limiter.fail(&ip, now);
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid user name or password",
        ));
    };
    st.limiter.clear(&ip);

    let token = auth::new_token()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e))?;
    let csrf = auth::new_token()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e))?;
    let (th, c2, aid, ttl) = (
        token_hash(&token),
        csrf.clone(),
        admin.id.clone(),
        st.cfg.session_ttl_ms,
    );
    st.db
        .call(move |c| store::create_session(c, &th, &aid, &c2, now, ttl))
        .await?;
    let secure = if st.cfg.secure_cookie { "; Secure" } else { "" };
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
        ttl / 1000
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({"admin": admin, "csrf": csrf})),
    )
        .into_response())
}

async fn logout(State(st): State<AppState>, a: Auth) -> Result<Response, ApiError> {
    let th = a.token_hash.clone();
    st.db.call(move |c| store::delete_session(c, &th)).await?;
    let secure = if st.cfg.secure_cookie { "; Secure" } else { "" };
    let cookie = format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}");
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"ok": true}))).into_response())
}

async fn me(a: Auth) -> Json<serde_json::Value> {
    Json(json!({"admin": a.admin, "csrf": a.csrf}))
}

// ───────────── 概览 / 节点 ─────────────

async fn overview(State(st): State<AppState>, _a: Auth) -> ApiResult<serde_json::Value> {
    let counts = st.db.call(|c| store::counts(c, store::now_ms())).await?;
    Ok(Json(json!({
        "counts": counts,
        "clients_online": st.hub.online_ids().len(),
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

async fn node(State(st): State<AppState>, _a: Auth) -> Json<serde_json::Value> {
    Json(json!({
        "name": "builtin",
        "public_host": st.cfg.public_host,
        "edge_port": st.cfg.edge_port(),
        "allow_ports": st.cfg.allow_ports,
        "version": env!("CARGO_PKG_VERSION"),
        "clients_online": st.hub.online_ids().len(),
    }))
}

// ───────────── 客户端 ─────────────

async fn list_clients(
    State(st): State<AppState>,
    _a: Auth,
) -> ApiResult<Vec<crate::models::Client>> {
    let mut v = st.db.call(|c| store::list_clients(c)).await?;
    let online = st.hub.online_ids();
    v.iter_mut().for_each(|c| c.online = online.contains(&c.id));
    Ok(Json(v))
}

async fn get_client(
    State(st): State<AppState>,
    _a: Auth,
    Path(id): Path<String>,
) -> ApiResult<crate::models::Client> {
    let mut c = st.db.call(move |c| store::get_client(c, &id)).await?;
    c.online = st.hub.is_online(&c.id);
    Ok(Json(c))
}

#[derive(Deserialize)]
struct PatchClient {
    name: Option<String>,
    disabled: Option<bool>,
    #[serde(default)]
    reason: String,
}

async fn patch_client(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<PatchClient>,
) -> ApiResult<crate::models::Client> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    let (actor, ip, id2) = (a.admin.name.clone(), a.ip.clone(), id.clone());
    let client = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let cl = store::update_client(&tx, &id2, b.name.as_deref(), b.disabled, now)?;
            store::audit(
                &tx,
                &actor,
                "client.update",
                "client",
                &id2,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(cl)
        })
        .await?;
    if client.disabled_at.is_some() {
        st.hub.kick(&id);
    }
    let mut c = client;
    c.online = st.hub.is_online(&c.id);
    Ok(Json(c))
}

async fn delete_client(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Query(q): Query<ReasonQuery>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&q.reason)?;
    let (actor, ip, id2) = (a.admin.name.clone(), a.ip.clone(), id.clone());
    st.db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            store::delete_client(&tx, &id2, now)?;
            store::audit(
                &tx,
                &actor,
                "client.delete",
                "client",
                &id2,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    st.hub.kick(&id);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ReasonBody {
    #[serde(default)]
    reason: String,
}

async fn kick_client(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<ReasonBody>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    let (actor, ip, id2) = (a.admin.name.clone(), a.ip.clone(), id.clone());
    st.db
        .call(move |c| {
            store::get_client(c, &id2)?;
            store::audit(
                c,
                &actor,
                "client.kick",
                "client",
                &id2,
                &why,
                "ok",
                &ip,
                store::now_ms(),
            )
        })
        .await?;
    st.hub.kick(&id);
    Ok(Json(json!({"ok": true})))
}

// ───────────── 隧道 ─────────────

#[derive(Deserialize)]
struct TunnelFilter {
    client_id: Option<String>,
}

async fn list_tunnels(
    State(st): State<AppState>,
    _a: Auth,
    Query(q): Query<TunnelFilter>,
) -> ApiResult<Vec<crate::models::Tunnel>> {
    Ok(Json(
        st.db
            .call(move |c| store::list_tunnels(c, q.client_id.as_deref()))
            .await?,
    ))
}

async fn get_tunnel(
    State(st): State<AppState>,
    _a: Auth,
    Path(id): Path<String>,
) -> ApiResult<crate::models::Tunnel> {
    Ok(Json(st.db.call(move |c| store::get_tunnel(c, &id)).await?))
}

fn default_kind() -> String {
    "tcp".into()
}
fn default_ip() -> String {
    "127.0.0.1".into()
}
fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct CreateTunnel {
    client_id: String,
    name: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default = "default_ip")]
    local_ip: String,
    local_port: i64,
    #[serde(default)]
    remote_port: i64,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    reason: String,
}

fn port_policy(cfg: &Config, remote_port: i64) -> Result<(), ApiError> {
    if remote_port == 0 || cfg.allow_ports.is_empty() {
        return Ok(());
    }
    if cfg
        .allow_ports
        .iter()
        .any(|(lo, hi)| (i64::from(*lo)..=i64::from(*hi)).contains(&remote_port))
    {
        Ok(())
    } else {
        Err(ApiError::bad(format!(
            "remote port {remote_port} is outside the ports this server allows"
        )))
    }
}

async fn create_tunnel(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<CreateTunnel>,
) -> Result<(StatusCode, Json<crate::models::Tunnel>), ApiError> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    port_policy(&st.cfg, b.remote_port)?;
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let new = NewTunnel {
        client_id: b.client_id,
        name: b.name,
        kind: b.kind,
        local_ip: b.local_ip,
        local_port: b.local_port,
        remote_port: b.remote_port,
        enabled: b.enabled,
    };
    let t = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let t = store::create_tunnel(&tx, &new, now)?;
            store::audit(
                &tx,
                &actor,
                "tunnel.create",
                "tunnel",
                &t.id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(t)
        })
        .await?;
    st.hub.refresh(&t.client_id).await?;
    Ok((StatusCode::CREATED, Json(t)))
}

#[derive(Deserialize)]
struct PatchTunnel {
    name: Option<String>,
    local_ip: Option<String>,
    local_port: Option<i64>,
    remote_port: Option<i64>,
    enabled: Option<bool>,
    #[serde(default)]
    reason: String,
}

async fn patch_tunnel(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<PatchTunnel>,
) -> ApiResult<crate::models::Tunnel> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    if let Some(p) = b.remote_port {
        port_policy(&st.cfg, p)?;
    }
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let patch = TunnelPatch {
        name: b.name,
        local_ip: b.local_ip,
        local_port: b.local_port,
        remote_port: b.remote_port,
        enabled: b.enabled,
    };
    let t = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let t = store::update_tunnel(&tx, &id, &patch, now)?;
            store::audit(
                &tx,
                &actor,
                "tunnel.update",
                "tunnel",
                &t.id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(t)
        })
        .await?;
    st.hub.refresh(&t.client_id).await?;
    Ok(Json(t))
}

async fn delete_tunnel(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Query(q): Query<ReasonQuery>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&q.reason)?;
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let client_id = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let t = store::get_tunnel(&tx, &id)?;
            store::delete_tunnel(&tx, &id, now)?;
            store::audit(
                &tx,
                &actor,
                "tunnel.delete",
                "tunnel",
                &id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(t.client_id)
        })
        .await?;
    st.hub.refresh(&client_id).await?;
    Ok(Json(json!({"ok": true})))
}

// ───────────── 接入令牌 ─────────────

#[derive(Deserialize)]
struct CreateToken {
    name: String,
    #[serde(default = "default_ttl")]
    ttl_minutes: i64,
    #[serde(default)]
    reason: String,
}

fn default_ttl() -> i64 {
    60
}

/// 渲染各平台的安装 / 接入命令。
fn commands(cfg: &Config, token: &str, name: &str) -> serde_json::Value {
    let controller = format!("{}:{}", cfg.public_host, cfg.edge_port());
    let quoted = name.replace('"', "");
    let direct = format!("rustfrp-agent --mode client --controller {controller} --enroll-token {token} --name \"{quoted}\"");
    let mut v = serde_json::Map::new();
    v.insert("controller".into(), json!(controller));
    v.insert("direct".into(), json!(direct));
    if !cfg.public_url.is_empty() {
        let args = format!("--controller {controller} --enroll-token {token} --name \"{quoted}\"");
        v.insert(
            "linux_macos".into(),
            json!(format!(
                "curl -fsSL {}/install.sh | sh -s -- {args}",
                cfg.public_url
            )),
        );
        v.insert(
            "windows".into(),
            json!(format!(
                "powershell -c \"iwr {}/install.ps1 -UseBasicParsing | iex; Install-RustFrp {args}\"",
                cfg.public_url
            )),
        );
    }
    serde_json::Value::Object(v)
}

async fn create_token(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<CreateToken>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    if !(1..=10_080).contains(&b.ttl_minutes) {
        return Err(ApiError::bad("ttl_minutes must be 1..10080"));
    }
    let token = auth::new_token()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e))?;
    let (th, name, actor, ip) = (
        token_hash(&token),
        b.name.clone(),
        a.admin.name.clone(),
        a.ip.clone(),
    );
    let ttl = b.ttl_minutes * 60_000;
    let rec = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let rec = store::create_enroll_token(&tx, &th, &name, &actor, now, ttl)?;
            store::audit(
                &tx,
                &actor,
                "enroll_token.create",
                "enroll_token",
                &rec.id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(rec)
        })
        .await?;
    // 明文令牌只在这一次响应里出现。
    let cmds = commands(&st.cfg, &token, &rec.name);
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": rec.id, "token": token, "expires_at": rec.expires_at, "commands": cmds})),
    ))
}

async fn list_tokens(
    State(st): State<AppState>,
    a: Auth,
) -> ApiResult<Vec<crate::models::EnrollToken>> {
    a.require(&st, Role::Admin).await?;
    Ok(Json(st.db.call(|c| store::list_enroll_tokens(c)).await?))
}

async fn revoke_token(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Query(q): Query<ReasonQuery>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&q.reason)?;
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    st.db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            store::revoke_enroll_token(&tx, &id, now)?;
            store::audit(
                &tx,
                &actor,
                "enroll_token.revoke",
                "enroll_token",
                &id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

// ───────────── 审计 / 管理员 ─────────────

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    before: Option<i64>,
}

fn default_limit() -> i64 {
    100
}

async fn list_audit(
    State(st): State<AppState>,
    a: Auth,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Vec<crate::models::AuditEntry>> {
    a.require(&st, Role::Admin).await?;
    Ok(Json(
        st.db
            .call(move |c| store::list_audit(c, q.limit, q.before))
            .await?,
    ))
}

async fn list_admins(State(st): State<AppState>, a: Auth) -> ApiResult<Vec<Admin>> {
    a.require(&st, Role::Owner).await?;
    Ok(Json(st.db.call(|c| store::list_admins(c)).await?))
}

// ───────────── 静态面板与安全头 ─────────────

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// 把请求路径映射为面板目录内的相对文件；拒绝路径穿越。
fn panel_relative(path: &str) -> Option<String> {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return Some("index.html".into());
    }
    let ok = rel.split('/').all(|seg| {
        !seg.is_empty() && seg != "." && seg != ".." && !seg.contains('\\') && !seg.contains('\0')
    });
    ok.then(|| rel.to_string())
}

async fn panel(State(st): State<AppState>, req: Request) -> Response {
    let not_found =
        || ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found").into_response();
    let Some(dir) = &st.cfg.panel_dir else {
        return not_found();
    };
    if req.uri().path().starts_with("/api/") {
        return not_found();
    }
    let Some(rel) = panel_relative(req.uri().path()) else {
        return not_found();
    };
    let try_read =
        |rel: String| async move { tokio::fs::read(dir.join(&rel)).await.map(|b| (rel, b)) };
    // 单页应用：不存在且不带扩展名的路径回落到 index.html。
    let found = match try_read(rel.clone()).await {
        Ok(v) => Some(v),
        Err(_) if !rel.contains('.') => try_read("index.html".into()).await.ok(),
        Err(_) => None,
    };
    let Some((rel, bytes)) = found else {
        return not_found();
    };
    let cache = if rel == "index.html" {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    (
        [
            (header::CONTENT_TYPE, content_type(&rel)),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response()
}

async fn security_headers(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(h, "content-security-policy", "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; font-src 'self' data:; frame-ancestors 'none'; base-uri 'self'");
    set(h, "x-content-type-options", "nosniff");
    set(h, "referrer-policy", "no-referrer");
    set(h, "x-frame-options", "DENY");
    if api {
        set(h, "cache-control", "no-store");
    }
    res
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

/// 构建路由。
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/overview", get(overview))
        .route("/node", get(node))
        .route("/clients", get(list_clients))
        .route(
            "/clients/{id}",
            get(get_client).patch(patch_client).delete(delete_client),
        )
        .route("/clients/{id}/kick", post(kick_client))
        .route("/tunnels", get(list_tunnels).post(create_tunnel))
        .route(
            "/tunnels/{id}",
            get(get_tunnel).patch(patch_tunnel).delete(delete_tunnel),
        )
        .route("/enroll-tokens", get(list_tokens).post(create_token))
        .route("/enroll-tokens/{id}", delete(revoke_token))
        .route("/audit", get(list_audit))
        .route("/admins", get(list_admins));
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/v1", api)
        .fallback(panel)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// 序列化为 JSON 响应体的统一入口（测试使用）。
#[derive(Serialize)]
pub struct Empty {}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn limiter_blocks_after_max_failures_and_recovers() {
        let l = LoginLimiter::new(3, 1000);
        assert!(l.allowed("1.1.1.1", 0));
        for _ in 0..3 {
            l.fail("1.1.1.1", 10);
        }
        assert!(!l.allowed("1.1.1.1", 500));
        assert!(l.allowed("2.2.2.2", 500), "other sources are unaffected");
        assert!(l.allowed("1.1.1.1", 1010), "window expired");
        l.fail("1.1.1.1", 2000);
        l.clear("1.1.1.1");
        assert!(l.allowed("1.1.1.1", 2001));
        // 过期窗口内再次失败会重新计数。
        l.fail("3.3.3.3", 0);
        l.fail("3.3.3.3", 5000);
        l.fail("3.3.3.3", 5001);
        l.fail("3.3.3.3", 5002);
        assert!(!l.allowed("3.3.3.3", 5003));
    }

    #[test]
    fn limiter_caps_memory() {
        let l = LoginLimiter::new(5, 1000);
        for i in 0..10_050 {
            l.fail(&format!("10.0.{}.{}", i / 256, i % 256), 0);
        }
        l.fail("late", 5000);
        assert!(l.map.lock().unwrap().len() <= 10_001);
    }

    #[test]
    fn cookie_and_origin_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; rfrp_session=tok; b=2"),
        );
        assert_eq!(cookie_value(&h, COOKIE), Some("tok"));
        assert_eq!(cookie_value(&h, "zzz"), None);
        assert_eq!(cookie_value(&HeaderMap::new(), COOKIE), None);

        assert!(
            same_origin(&h),
            "no Origin header → allowed (non-browser client)"
        );
        h.insert(header::HOST, HeaderValue::from_static("panel.example.com"));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://panel.example.com"),
        );
        assert!(same_origin(&h));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example.com"),
        );
        assert!(!same_origin(&h));
        h.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(!same_origin(&h));
    }

    #[test]
    fn panel_paths_cannot_escape() {
        assert_eq!(panel_relative("/"), Some("index.html".into()));
        assert_eq!(
            panel_relative("//x"),
            Some("x".into()),
            "leading slashes are stripped, never escape"
        );
        assert_eq!(panel_relative("/main.dart.js"), Some("main.dart.js".into()));
        assert_eq!(
            panel_relative("/assets/a/b.png"),
            Some("assets/a/b.png".into())
        );
        for bad in [
            "/../etc/passwd",
            "/a/../b",
            "/a//b",
            "/a\\b",
            "/./x",
            "/a/.",
        ] {
            assert_eq!(panel_relative(bad), None, "{bad}");
        }
        assert_eq!(content_type("a.wasm"), "application/wasm");
        assert_eq!(content_type("a.unknown"), "application/octet-stream");
        for ext in [
            "html", "js", "mjs", "css", "json", "png", "svg", "ico", "ttf", "otf", "woff2",
        ] {
            assert_ne!(
                content_type(&format!("x.{ext}")),
                "application/octet-stream",
                "{ext}"
            );
        }
    }

    #[test]
    fn reason_rules() {
        assert!(reason("  ok  ").is_ok());
        assert!(reason("").is_err() && reason("   ").is_err());
        assert!(reason(&"x".repeat(MAX_REASON + 1)).is_err());
        assert!(reason(&"x".repeat(MAX_REASON)).is_ok());
    }

    #[test]
    fn port_policy_rules() {
        let cfg = Config {
            allow_ports: vec![(6000, 6100)],
            ..Config::default()
        };
        assert!(port_policy(&cfg, 0).is_ok() && port_policy(&cfg, 6050).is_ok());
        assert!(port_policy(&cfg, 5999).is_err());
        assert!(port_policy(&Config::default(), 1).is_ok());
    }

    #[test]
    fn install_commands() {
        let mut cfg = Config {
            public_host: "frp.example.com".into(),
            edge_addr: "0.0.0.0:7000".into(),
            ..Config::default()
        };
        let v = commands(&cfg, "rfrp_tok", "my \"box\"");
        assert_eq!(v["controller"], "frp.example.com:7000");
        assert!(v["direct"]
            .as_str()
            .unwrap()
            .contains("--enroll-token rfrp_tok --name \"my box\""));
        assert!(v.get("linux_macos").is_none());
        cfg.public_url = "https://frp.example.com".into();
        let v = commands(&cfg, "t", "n");
        assert!(v["linux_macos"]
            .as_str()
            .unwrap()
            .starts_with("curl -fsSL https://frp.example.com/install.sh | sh -s --"));
        assert!(v["windows"].as_str().unwrap().contains("install.ps1"));
    }
}
