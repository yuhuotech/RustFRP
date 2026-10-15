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
use axum::routing::{delete, get, patch, post};
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
    /// 内置 Edge 实时计数。
    pub metrics: Arc<rustfrp_agent::metrics::Metrics>,
    /// 进程级有界脱敏日志。
    pub logs: Arc<rustfrp_agent::log_ring::LogRing>,
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
        if let Some(header) = parts.headers.get(header::AUTHORIZATION) {
            let bearer = header
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .filter(|v| v.len() == 69 && v.starts_with("rfrp_"))
                .ok_or_else(ApiError::unauthorized)?;
            if parts.headers.contains_key(header::ORIGIN) && !same_origin(&parts.headers) {
                return Err(ApiError::forbidden("cross-origin request rejected"));
            }
            let (hash, path, safe) = (
                token_hash(bearer),
                parts
                    .extensions
                    .get::<axum::extract::OriginalUri>()
                    .map_or_else(
                        || parts.uri.path().to_string(),
                        |uri| uri.0.path().to_string(),
                    ),
                is_safe(&parts.method),
            );
            let admin = state
                .db
                .call(move |c| {
                    crate::api_tokens::authenticate(c, &hash, store::now_ms(), &path, safe)
                })
                .await
                .map_err(|e| match e {
                    StoreError::NotFound => ApiError::unauthorized(),
                    StoreError::Invalid(_) => {
                        ApiError::forbidden("token scope denies this endpoint")
                    }
                    other => other.into(),
                })?;
            return Ok(Auth {
                admin,
                ip: client_ip(parts),
                token_hash: String::new(),
                csrf: String::new(),
            });
        }
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
        Json(json!({"admin": admin, "csrf": csrf, "api":"v1"})),
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
    Json(json!({"admin": a.admin, "csrf": a.csrf, "api":"v1"}))
}

// ───────────── 概览 / 节点 ─────────────

async fn overview(State(st): State<AppState>, _a: Auth) -> ApiResult<serde_json::Value> {
    let (counts, day) = st
        .db
        .call(|c| {
            let now = store::now_ms();
            Ok((
                store::counts(c, now)?,
                crate::traffic::last_day_totals(c, now)?,
            ))
        })
        .await?;
    let traffic = st.metrics.total().snapshot();
    Ok(Json(json!({
        "process": crate::process::sample(),
        "traffic_24h": {"bytes_in": day.0, "bytes_out": day.1},
        "traffic": {"bytes_in":traffic.bytes_in,"bytes_out":traffic.bytes_out,"connections":traffic.connections,"connections_total":traffic.connections_total},
        "counts": counts,
        "clients_online": st.hub.online_ids().len(),
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

#[derive(Deserialize)]
struct TrafficQuery {
    #[serde(default = "traffic_hours")]
    hours: i64,
    tunnel_id: Option<String>,
    client_id: Option<String>,
}
fn traffic_hours() -> i64 {
    24
}
async fn traffic_history(
    State(st): State<AppState>,
    _a: Auth,
    Query(q): Query<TrafficQuery>,
) -> ApiResult<serde_json::Value> {
    let hours = q.hours;
    let now = store::now_ms();
    let items = st
        .db
        .call(move |c| {
            if let Some(id) = &q.tunnel_id {
                store::get_tunnel(c, id)?;
            }
            if let Some(id) = &q.client_id {
                store::get_client(c, id)?;
            }
            crate::traffic::history_filtered(
                c,
                now,
                hours,
                q.tunnel_id.as_deref(),
                q.client_id.as_deref(),
            )
        })
        .await?;
    Ok(Json(
        json!({"range_hours":hours,"bucket_ms":3_600_000,"sampled_only":true,"window_end":now / 3_600_000 * 3_600_000,"items":items}),
    ))
}

async fn node(State(st): State<AppState>, _a: Auth) -> Json<serde_json::Value> {
    Json(json!({
        "tls_fingerprint":st.cfg.edge_tls_fingerprint,
        "name": "builtin",
        "enabled": st.cfg.builtin_edge,
        "public_host": st.cfg.public_host,
        "edge_port": st.cfg.edge_port(),
        "allow_ports": st.cfg.allow_ports,
        "vhost_http_port": st.cfg.vhost_http,
        "vhost_https_port": st.cfg.vhost_https,
        "tcpmux_http_connect_port": st.cfg.tcpmux_http_connect_port,
        "subdomain_host": st.cfg.subdomain_host,
        "version": env!("CARGO_PKG_VERSION"),
        "clients_online": st.hub.online_ids().len(),
    }))
}

// ───────────── Prometheus ─────────────

/// 文本暴露格式 0.0.4。默认不提供；设置 `RUSTFRP_METRICS_TOKEN` 后需 `Authorization: Bearer <token>`。
async fn metrics(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let token = st.cfg.metrics_token.as_str();
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if token.is_empty() {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    if !rustfrp_core::http1::ct_eq(presented, token) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized",
        )
            .into_response();
    }
    let counted = st
        .db
        .call(|c| {
            let now = store::now_ms();
            Ok((
                store::counts(c, now)?,
                crate::traffic::last_day_totals(c, now)?,
                store::list_nodes(c)?,
            ))
        })
        .await;
    let Ok((counts, day, nodes)) = counted else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
    };
    let live = st.metrics.total().snapshot();
    let process = crate::process::sample();
    let nodes_online = nodes.iter().filter(|n| st.hub.node_online(&n.id)).count();
    let mut out = String::new();
    let mut gauge = |name: &str, help: &str, kind: &str, value: String| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
        ));
    };
    gauge(
        "rustfrp_build_info",
        "Controller version (value is always 1).",
        "gauge",
        "1".into(),
    );
    gauge(
        "rustfrp_clients",
        "Registered clients.",
        "gauge",
        counts.clients.to_string(),
    );
    gauge(
        "rustfrp_clients_online",
        "Clients currently connected.",
        "gauge",
        st.hub.online_ids().len().to_string(),
    );
    gauge(
        "rustfrp_tunnels",
        "Configured tunnels.",
        "gauge",
        counts.tunnels.to_string(),
    );
    gauge(
        "rustfrp_tunnels_running",
        "Tunnels reported running.",
        "gauge",
        counts.tunnels_running.to_string(),
    );
    gauge(
        "rustfrp_nodes",
        "External nodes.",
        "gauge",
        nodes.len().to_string(),
    );
    gauge(
        "rustfrp_nodes_online",
        "External nodes with a live control link.",
        "gauge",
        nodes_online.to_string(),
    );
    gauge(
        "rustfrp_builtin_connections",
        "Connections currently relayed by the built-in node.",
        "gauge",
        live.connections.to_string(),
    );
    gauge(
        "rustfrp_builtin_bytes_in_total",
        "Bytes from visitors to services since start (built-in node).",
        "counter",
        live.bytes_in.to_string(),
    );
    gauge(
        "rustfrp_builtin_bytes_out_total",
        "Bytes from services to visitors since start (built-in node).",
        "counter",
        live.bytes_out.to_string(),
    );
    gauge(
        "rustfrp_traffic_bytes_in_24h",
        "Bytes in over the last 24 hours (all nodes).",
        "gauge",
        day.0.to_string(),
    );
    gauge(
        "rustfrp_traffic_bytes_out_24h",
        "Bytes out over the last 24 hours (all nodes).",
        "gauge",
        day.1.to_string(),
    );
    out.push_str(&process_metric_lines(&process));
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
        .into_response()
}

fn process_metric_lines(process: &crate::process::ProcessStats) -> String {
    let mut out = String::new();
    let mut gauge = |name: &str, help: &str, kind: &str, value: String| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
        ));
    };
    gauge(
        "rustfrp_process_uptime_seconds",
        "Controller uptime.",
        "gauge",
        process.uptime_seconds.to_string(),
    );
    if let Some(rss) = process.rss_bytes {
        gauge(
            "rustfrp_process_resident_memory_bytes",
            "Controller resident memory.",
            "gauge",
            rss.to_string(),
        );
    }
    if let Some(cpu) = process.cpu_seconds {
        gauge(
            "rustfrp_process_cpu_seconds_total",
            "Controller CPU time.",
            "counter",
            format!("{cpu:.2}"),
        );
    }
    out
}

// ───────────── 节点 ─────────────

/// 内置节点的展示条目。
fn builtin_node(cfg: &Config, clients: i64) -> crate::models::Node {
    crate::models::Node {
        id: String::new(),
        name: "builtin".into(),
        builtin: true,
        public_host: cfg.public_host.clone(),
        edge_port: cfg.edge_port(),
        tls_fingerprint: cfg.edge_tls_fingerprint.clone(),
        vhost_http: cfg.vhost_http,
        vhost_https: cfg.vhost_https,
        tcpmux_http_connect_port: cfg.tcpmux_http_connect_port,
        subdomain_host: cfg.subdomain_host.clone(),
        allow_ports: cfg.allow_ports.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        created_at: 0,
        last_seen_at: None,
        disabled_at: None,
        online: true,
        clients,
    }
}

async fn list_nodes(State(st): State<AppState>, _a: Auth) -> ApiResult<Vec<crate::models::Node>> {
    let (mut nodes, builtin_clients) = st
        .db
        .call(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM clients WHERE node_id = '' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            Ok((store::list_nodes(c)?, n))
        })
        .await?;
    nodes
        .iter_mut()
        .for_each(|n| n.online = st.hub.node_online(&n.id));
    if st.cfg.builtin_edge {
        nodes.insert(0, builtin_node(&st.cfg, builtin_clients));
    }
    Ok(Json(nodes))
}

#[derive(Deserialize)]
struct PatchNode {
    name: Option<String>,
    disabled: Option<bool>,
    #[serde(default)]
    reason: String,
}

async fn patch_node(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<PatchNode>,
) -> ApiResult<crate::models::Node> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    if b.name.is_none() && b.disabled.is_none() {
        return Err(ApiError::bad("nothing to change"));
    }
    let (actor, ip, id2) = (a.admin.name.clone(), a.ip.clone(), id.clone());
    let mut node = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let n = store::update_node(&tx, &id2, b.name.as_deref(), b.disabled, now)?;
            store::audit(
                &tx,
                &actor,
                "node.update",
                "node",
                &id2,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(n)
        })
        .await?;
    if node.disabled_at.is_some() {
        st.hub.node_drop(&id);
    }
    node.online = st.hub.node_online(&id);
    Ok(Json(node))
}

async fn delete_node(
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
            store::delete_node(&tx, &id2, now)?;
            store::audit(
                &tx,
                &actor,
                "node.delete",
                "node",
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
    st.hub.node_drop(&id);
    Ok(Json(json!({"ok": true})))
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
    tags: Option<Vec<String>>,
    max_connections: Option<usize>,
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
            let mut cl = store::update_client(&tx, &id2, b.name.as_deref(), b.disabled, now)?;
            if let Some(tags) = &b.tags {
                cl = store::set_client_tags(&tx, &id2, tags)?;
            }
            if let Some(max) = b.max_connections {
                cl = store::set_client_max_connections(&tx, &id2, max)?;
            }
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
    // 外置节点上的设备：停用 / 启用要推给节点，节点才会真正拒绝或放行它。
    st.hub.refresh(&id).await?;
    if client.disabled_at.is_some() {
        st.hub.kick(&id);
    }
    let mut c = client;
    c.online = st.hub.is_online(&c.id);
    Ok(Json(c))
}

#[derive(Deserialize)]
struct MoveClient {
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    reason: String,
}

async fn move_client(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<MoveClient>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    let (target, node_caps) = if b.node_id.is_empty() {
        if !st.cfg.builtin_edge {
            return Err(ApiError::bad("the built-in edge is disabled"));
        }
        (
            rustfrp_core::wire::EdgeRedirect {
                address: st.cfg.public_host.clone(),
                port: st.cfg.edge_port(),
                tls_fingerprint: st.cfg.edge_tls_fingerprint.clone(),
            },
            (
                st.cfg.vhost_http,
                st.cfg.vhost_https,
                st.cfg.tcpmux_http_connect_port,
                st.cfg.subdomain_host.clone(),
                st.cfg.allow_ports.clone(),
            ),
        )
    } else {
        let node_id = b.node_id.clone();
        let node = st.db.call(move |c| store::get_node(c, &node_id)).await?;
        if node.disabled_at.is_some() {
            return Err(ApiError::bad("destination node is disabled"));
        }
        (
            rustfrp_core::wire::EdgeRedirect {
                address: node.public_host,
                port: node.edge_port,
                tls_fingerprint: node.tls_fingerprint,
            },
            (
                node.vhost_http,
                node.vhost_https,
                node.tcpmux_http_connect_port,
                node.subdomain_host,
                node.allow_ports,
            ),
        )
    };
    let client_for_tunnels = id.clone();
    let tunnels = st
        .db
        .call(move |c| store::list_tunnels(c, Some(&client_for_tunnels)))
        .await?;
    for tunnel in &tunnels {
        if (tunnel.kind == "http" && node_caps.0.is_none())
            || (tunnel.kind == "https" && node_caps.1.is_none())
            || (tunnel.kind == "tcpmux" && node_caps.2.is_none())
            || (!tunnel.subdomain.is_empty() && node_caps.3.is_empty())
        {
            return Err(ApiError::bad(format!(
                "destination node does not support the existing {} tunnel",
                tunnel.kind
            )));
        }
        if tunnel.remote_port > 0
            && !node_caps.4.is_empty()
            && !node_caps
                .4
                .iter()
                .any(|(lo, hi)| (*lo..=*hi).contains(&tunnel.remote_port))
        {
            return Err(ApiError::bad(format!(
                "destination node does not allow existing port {}",
                tunnel.remote_port
            )));
        }
    }
    let (actor, ip, client_id, destination) = (
        a.admin.name.clone(),
        a.ip.clone(),
        id.clone(),
        b.node_id.clone(),
    );
    let target_for_db = target.clone();
    let old_node = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let old =
                store::move_client_to_node(&tx, &client_id, &destination, &target_for_db, now)?;
            store::audit(
                &tx,
                &actor,
                "client.move_node",
                "client",
                &client_id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(old)
        })
        .await?;
    st.hub.redirect_device(&id, &old_node, target.clone()).await;
    Ok(Json(json!({"ok":true,"node_id":b.node_id,"target":target})))
}

#[derive(Deserialize)]
struct UpdateClient {
    version: String,
    #[serde(default)]
    reason: String,
}

async fn update_client(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<UpdateClient>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    if !st.hub.is_online(&id) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "the device must be online to receive an update",
        ));
    }
    let (actor, ip, client_id, version) = (
        a.admin.name.clone(),
        a.ip.clone(),
        id.clone(),
        b.version.clone(),
    );
    let (seq, version) = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let command = store::request_client_update(&tx, &client_id, &version)?;
            store::audit(
                &tx,
                &actor,
                "client.update_agent",
                "client",
                &client_id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(command)
        })
        .await?;
    st.hub.refresh(&id).await?;
    Ok(Json(json!({"ok":true,"seq":seq,"version":version})))
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
    // 外置节点要收到 Remove，才会拒绝这台设备再次登录。
    st.hub.sync_device(&id).await;
    st.hub.kick(&id);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ReasonBody {
    #[serde(default)]
    reason: String,
}

async fn restart_client(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<ReasonBody>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let why = reason(&b.reason)?;
    if !st.hub.is_online(&id) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "the device is offline; a restart request only applies to a connected agent",
        ));
    }
    let (actor, ip, id2) = (a.admin.name.clone(), a.ip.clone(), id.clone());
    st.db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            store::request_client_restart(&tx, &id2)?;
            store::audit(
                &tx,
                &actor,
                "client.restart",
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
    st.hub.refresh(&id).await?;
    Ok(Json(json!({"ok": true})))
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

#[derive(Deserialize)]
struct ConfigUpload {
    text: String,
    digest: Option<String>,
    #[serde(default)]
    accept_warnings: bool,
    #[serde(default)]
    reason: String,
}
async fn preview_config(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<ConfigUpload>,
) -> ApiResult<crate::config_transfer::Preview> {
    a.require(&st, Role::Operator).await?;
    let cfg = st.cfg.clone();
    Ok(Json(
        st.db
            .call(move |c| crate::config_transfer::preview(c, &cfg, &id, &b.text))
            .await?,
    ))
}
async fn import_config(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<ConfigUpload>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    if b.digest.as_deref() != Some(token_hash(&b.text).as_str()) || !b.accept_warnings {
        return Err(ApiError::bad(
            "preview digest and explicit compatibility acknowledgement required",
        ));
    }
    let cfg = st.cfg.clone();
    let target = id.clone();
    let updated = st
        .db
        .call(move |c| {
            crate::config_transfer::import(c, &cfg, &target, &b.text, (&a.admin.name, &why, &a.ip))
        })
        .await?;
    st.hub.refresh(&id).await?;
    Ok(Json(json!({"updated":updated})))
}
async fn export_config(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
) -> ApiResult<serde_json::Value> {
    // 导出含 http 隧道的 Basic 认证密码，等同于凭据：仅管理员及以上。
    a.require(&st, Role::Admin).await?;
    let cfg = st.cfg.clone();
    let text = st
        .db
        .call(move |c| crate::config_transfer::export(c, &cfg, &id))
        .await?;
    Ok(Json(
        json!({"format":"toml","text":text,"notice":"device and server authentication credentials omitted; http Basic auth passwords are included"}),
    ))
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
    custom_domains: Vec<String>,
    #[serde(default)]
    subdomain: String,
    #[serde(default)]
    host_header_rewrite: String,
    #[serde(default)]
    locations: Vec<String>,
    #[serde(default)]
    http_user: String,
    #[serde(default)]
    http_password: String,
    #[serde(default)]
    route_by_http_user: String,
    #[serde(default)]
    request_headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    response_headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    proxy_protocol: u8,
    #[serde(default)]
    bandwidth_bps: u64,
    #[serde(default)]
    bandwidth_server: bool,
    #[serde(default)]
    max_connections: usize,
    #[serde(default)]
    multiplexer: String,
    #[serde(default)]
    use_encryption: bool,
    #[serde(default)]
    use_compression: bool,
    #[serde(default)]
    lb_group: String,
    #[serde(default)]
    lb_group_key: String,
    #[serde(default)]
    health_check: Option<rustfrp_core::wire::HealthSpec>,
    #[serde(default)]
    plugin: Option<rustfrp_core::wire::PluginOpts>,
    #[serde(default)]
    reason: String,
}

fn port_policy(cfg: &Config, remote_port: i64) -> Result<(), ApiError> {
    cfg.check_remote_port(remote_port).map_err(ApiError::bad)
}

/// 该服务器是否开启了对应的虚拟主机端口 / 子域名后缀。
async fn tunnel_node_policy(
    st: &AppState,
    client_id: &str,
    kind: &str,
    subdomain: &str,
) -> Result<(), ApiError> {
    let id = client_id.to_string();
    let node_id = st
        .db
        .call(move |c| {
            let client = store::get_client(c, &id)?;
            Ok(client.node_id)
        })
        .await?;
    if node_id.is_empty() {
        vhost_policy(&st.cfg, kind, subdomain)?;
    }
    let (vhost_http, vhost_https, tcpmux, suffix) = if node_id.is_empty() {
        if !st.cfg.builtin_edge {
            return Err(ApiError::bad(
                "the built-in edge is disabled; move the client to an external node",
            ));
        }
        (
            st.cfg.vhost_http,
            st.cfg.vhost_https,
            st.cfg.tcpmux_http_connect_port,
            st.cfg.subdomain_host.clone(),
        )
    } else {
        let id = node_id.clone();
        let node = st.db.call(move |c| store::get_node(c, &id)).await?;
        if node.disabled_at.is_some() {
            return Err(ApiError::bad("the client's node is disabled"));
        }
        (
            node.vhost_http,
            node.vhost_https,
            node.tcpmux_http_connect_port,
            node.subdomain_host,
        )
    };
    if kind == "http" && vhost_http.is_none() {
        return Err(ApiError::bad(
            "http tunnels need the vhost HTTP port (set RUSTFRP_VHOST_HTTP on the server)",
        ));
    }
    if kind == "https" && vhost_https.is_none() {
        return Err(ApiError::bad(
            "https tunnels need the vhost HTTPS port (set RUSTFRP_VHOST_HTTPS on the server)",
        ));
    }
    if kind == "tcpmux" && tcpmux.is_none() {
        return Err(ApiError::bad(
            "tcpmux needs tcpmuxHTTPConnectPort on the selected node",
        ));
    }
    if !subdomain.is_empty() && suffix.is_empty() {
        return Err(ApiError::bad(
            "subdomains need RUSTFRP_SUBDOMAIN_HOST on the server",
        ));
    }
    Ok(())
}

fn vhost_policy(cfg: &Config, kind: &str, subdomain: &str) -> Result<(), ApiError> {
    if kind == "http" && cfg.vhost_http.is_none() {
        return Err(ApiError::bad(
            "http tunnels need the vhost HTTP port (set RUSTFRP_VHOST_HTTP on the server)",
        ));
    }
    if kind == "https" && cfg.vhost_https.is_none() {
        return Err(ApiError::bad(
            "https tunnels need the vhost HTTPS port (set RUSTFRP_VHOST_HTTPS on the server)",
        ));
    }
    if kind == "tcpmux" && cfg.tcpmux_http_connect_port.is_none() {
        return Err(ApiError::bad("tcpmux needs tcpmuxHTTPConnectPort"));
    }
    if !subdomain.is_empty() && cfg.subdomain_host.is_empty() {
        return Err(ApiError::bad("subdomains need RUSTFRP_SUBDOMAIN_HOST"));
    }
    Ok(())
}

async fn create_tunnel(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<CreateTunnel>,
) -> Result<(StatusCode, Json<crate::models::Tunnel>), ApiError> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    port_policy(&st.cfg, b.remote_port)?;
    tunnel_node_policy(&st, &b.client_id, &b.kind, &b.subdomain).await?;
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let quota = st.cfg.max_tunnels_per_client;
    let new = NewTunnel {
        client_id: b.client_id,
        name: b.name,
        kind: b.kind,
        local_ip: b.local_ip,
        local_port: b.local_port,
        remote_port: b.remote_port,
        enabled: b.enabled,
        custom_domains: b.custom_domains,
        subdomain: b.subdomain,
        host_header_rewrite: b.host_header_rewrite,
        http: rustfrp_core::wire::HttpOpts {
            locations: b.locations,
            http_user: b.http_user,
            http_password: b.http_password,
            route_by_http_user: b.route_by_http_user,
            request_headers: b.request_headers,
            response_headers: b.response_headers,
        },
        proxy_protocol: b.proxy_protocol,
        bandwidth_bps: b.bandwidth_bps,
        bandwidth_server: b.bandwidth_server,
        max_connections: b.max_connections,
        multiplexer: b.multiplexer,
        use_encryption: b.use_encryption,
        use_compression: b.use_compression,
        lb: rustfrp_core::wire::LbOpts {
            group: b.lb_group,
            group_key: b.lb_group_key,
            health: b.health_check.filter(|h| !h.kind.is_empty()),
        },
        plugin: b.plugin,
    };
    let t = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let t = store::create_tunnel(&tx, &new, now)?;
            store::enforce_tunnel_quota(&tx, &t.client_id, quota)?;
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
struct BatchTunnels {
    ids: Vec<String>,
    enabled: bool,
    #[serde(default)]
    reason: String,
}
async fn batch_tunnels(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<BatchTunnels>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    let ids: std::collections::HashSet<_> = b.ids.iter().collect();
    if b.ids.is_empty()
        || b.ids.len() > 100
        || ids.len() != b.ids.len()
        || b.ids.iter().any(|id| id.is_empty() || id.len() > 64)
    {
        return Err(StoreError::Invalid("batch requires 1..100 distinct tunnel IDs".into()).into());
    }
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let updated = b.ids.len();
    let clients = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let mut clients = std::collections::HashSet::new();
            for id in b.ids {
                let t = store::update_tunnel(
                    &tx,
                    &id,
                    &TunnelPatch {
                        enabled: Some(b.enabled),
                        ..Default::default()
                    },
                    now,
                )?;
                store::audit(
                    &tx,
                    &actor,
                    "tunnel.batch",
                    "tunnel",
                    &id,
                    &why,
                    "ok",
                    &ip,
                    now,
                )?;
                clients.insert(t.client_id);
                clients.extend(store::tunnel_visitor_clients(&tx, &id)?);
            }
            tx.commit()?;
            Ok(clients)
        })
        .await?;
    for id in clients {
        st.hub.refresh(&id).await?;
    }
    Ok(Json(json!({"updated":updated})))
}

#[derive(Deserialize)]
struct PatchTunnel {
    name: Option<String>,
    local_ip: Option<String>,
    local_port: Option<i64>,
    remote_port: Option<i64>,
    enabled: Option<bool>,
    custom_domains: Option<Vec<String>>,
    subdomain: Option<String>,
    host_header_rewrite: Option<String>,
    locations: Option<Vec<String>>,
    http_user: Option<String>,
    /// 缺省 = 保持原密码；空串 = 清除。
    http_password: Option<String>,
    route_by_http_user: Option<String>,
    request_headers: Option<std::collections::BTreeMap<String, String>>,
    response_headers: Option<std::collections::BTreeMap<String, String>>,
    proxy_protocol: Option<u8>,
    bandwidth_bps: Option<u64>,
    bandwidth_server: Option<bool>,
    max_connections: Option<usize>,
    multiplexer: Option<String>,
    use_encryption: Option<bool>,
    use_compression: Option<bool>,
    lb_group: Option<String>,
    lb_group_key: Option<String>,
    health_check: Option<rustfrp_core::wire::HealthSpec>,
    #[serde(default)]
    plugin: Option<rustfrp_core::wire::PluginOpts>,
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
    let tunnel_id = id.clone();
    let client_id = st
        .db
        .call(move |c| Ok(store::get_tunnel(c, &tunnel_id)?.client_id))
        .await?;
    if let Some(subdomain) = &b.subdomain {
        let kind = st
            .db
            .call({
                let id = id.clone();
                move |c| Ok(store::get_tunnel(c, &id)?.kind)
            })
            .await?;
        tunnel_node_policy(&st, &client_id, &kind, subdomain).await?;
    }
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let patch = TunnelPatch {
        name: b.name,
        local_ip: b.local_ip,
        local_port: b.local_port,
        remote_port: b.remote_port,
        enabled: b.enabled,
        custom_domains: b.custom_domains,
        subdomain: b.subdomain,
        host_header_rewrite: b.host_header_rewrite,
        locations: b.locations,
        http_user: b.http_user,
        http_password: b.http_password,
        route_by_http_user: b.route_by_http_user,
        request_headers: b.request_headers,
        response_headers: b.response_headers,
        proxy_protocol: b.proxy_protocol,
        bandwidth_bps: b.bandwidth_bps,
        bandwidth_server: b.bandwidth_server,
        max_connections: b.max_connections,
        multiplexer: b.multiplexer,
        use_encryption: b.use_encryption,
        use_compression: b.use_compression,
        lb_group: b.lb_group,
        lb_group_key: b.lb_group_key,
        health_check: b.health_check,
        plugin: b.plugin,
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
            let visitors = store::tunnel_visitor_clients(&tx, &t.id)?;
            tx.commit()?;
            Ok((t, visitors))
        })
        .await?;
    let (t, visitors) = t;
    st.hub.refresh(&t.client_id).await?;
    for client in visitors {
        st.hub.refresh(&client).await?;
    }
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
            let visitors = store::tunnel_visitor_clients(&tx, &id)?;
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
            Ok((t.client_id, visitors))
        })
        .await?;
    let (client_id, visitors) = client_id;
    st.hub.refresh(&client_id).await?;
    for client in visitors {
        st.hub.refresh(&client).await?;
    }
    Ok(Json(json!({"ok": true})))
}

// ───────────── 访问者 ─────────────

async fn list_visitors(
    State(st): State<AppState>,
    _a: Auth,
    Query(q): Query<TunnelFilter>,
) -> ApiResult<Vec<crate::models::Visitor>> {
    Ok(Json(
        st.db
            .call(move |c| store::list_visitors(c, q.client_id.as_deref()))
            .await?,
    ))
}

#[derive(Deserialize)]
struct CreateVisitor {
    client_id: String,
    name: String,
    tunnel_id: String,
    #[serde(default)]
    bind_addr: String,
    #[serde(default)]
    bind_port: i64,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    use_encryption: bool,
    #[serde(default)]
    use_compression: bool,
    #[serde(default)]
    reason: String,
}

async fn create_visitor(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<CreateVisitor>,
) -> Result<(StatusCode, Json<crate::models::Visitor>), ApiError> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let new = store::NewVisitor {
        client_id: b.client_id,
        name: b.name,
        tunnel_id: b.tunnel_id,
        bind_addr: b.bind_addr,
        bind_port: b.bind_port,
        enabled: b.enabled,
        use_encryption: b.use_encryption,
        use_compression: b.use_compression,
    };
    let v = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let v = store::create_visitor(&tx, &new, now)?;
            store::audit(
                &tx,
                &actor,
                "visitor.create",
                "visitor",
                &v.id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(v)
        })
        .await?;
    st.hub.refresh(&v.client_id).await?;
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Deserialize)]
struct PatchVisitor {
    name: Option<String>,
    tunnel_id: Option<String>,
    bind_addr: Option<String>,
    bind_port: Option<i64>,
    enabled: Option<bool>,
    use_encryption: Option<bool>,
    use_compression: Option<bool>,
    #[serde(default)]
    reason: String,
}

async fn patch_visitor(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<PatchVisitor>,
) -> ApiResult<crate::models::Visitor> {
    a.require(&st, Role::Operator).await?;
    let why = reason(&b.reason)?;
    let (actor, ip) = (a.admin.name.clone(), a.ip.clone());
    let patch = store::VisitorPatch {
        name: b.name,
        tunnel_id: b.tunnel_id,
        bind_addr: b.bind_addr,
        bind_port: b.bind_port,
        enabled: b.enabled,
        use_encryption: b.use_encryption,
        use_compression: b.use_compression,
    };
    let v = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let v = store::update_visitor(&tx, &id, &patch, now)?;
            store::audit(
                &tx,
                &actor,
                "visitor.update",
                "visitor",
                &v.id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(v)
        })
        .await?;
    st.hub.refresh(&v.client_id).await?;
    Ok(Json(v))
}

async fn delete_visitor(
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
            let v = store::get_visitor(&tx, &id)?;
            store::delete_visitor(&tx, &id, now)?;
            store::audit(
                &tx,
                &actor,
                "visitor.delete",
                "visitor",
                &id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(v.client_id)
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
    /// `client`（默认）或 `node`。
    #[serde(default)]
    kind: String,
    /// 设备令牌：由哪个外置节点服务（缺省 = 内置节点）。
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    reason: String,
}

fn default_ttl() -> i64 {
    60
}

/// 设备接入的目标：内置节点，或某个外置节点。
struct Target {
    host: String,
    port: u16,
    fingerprint: String,
}

/// 渲染各平台的安装 / 接入命令（内置节点）。
fn commands(cfg: &Config, token: &str, name: &str) -> serde_json::Value {
    let target = Target {
        host: cfg.public_host.clone(),
        port: cfg.edge_port(),
        fingerprint: cfg.edge_tls_fingerprint.clone(),
    };
    commands_to(cfg, token, name, &target)
}

/// 渲染各平台的安装 / 接入命令。
fn commands_to(cfg: &Config, token: &str, name: &str, target: &Target) -> serde_json::Value {
    let controller = format!("{}:{}", target.host, target.port);
    let edge_tls_fingerprint = &target.fingerprint;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let ps_quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let direct = format!(
        "rustfrp-agent --mode client --controller {} --enroll-token {} --name {}",
        quote(&controller),
        quote(token),
        quote(name)
    );
    let pin = if edge_tls_fingerprint.is_empty() {
        String::new()
    } else {
        format!(" --tls-pin {}", quote(edge_tls_fingerprint))
    };
    let direct = format!("{direct}{pin}");
    let mut v = serde_json::Map::new();
    v.insert("controller".into(), json!(controller));
    v.insert("direct".into(), json!(direct));
    if !cfg.public_url.is_empty() {
        let base = cfg.public_url.trim_end_matches('/');
        let args = format!(
            "--controller {} --enroll-token {} --name {} --download-base {}",
            quote(&controller),
            quote(token),
            quote(name),
            quote(&format!("{base}/downloads"))
        );
        let args = format!("{args}{pin}");
        v.insert(
            "linux_macos".into(),
            json!(format!(
                "curl -fsSL {} | sh -s -- {args}",
                quote(&format!("{base}/install.sh"))
            )),
        );
        // PowerShell literals preserve names and tokens without interpolation.
        let script = format!(
            "& {{ $ErrorActionPreference = 'Stop'; & ([scriptblock]::Create((Invoke-WebRequest -UseBasicParsing {}).Content)); Install-RustFrp -Controller {} -EnrollToken {} -Name {} -DownloadBase {} }}",
            ps_quote(&format!("{base}/install.ps1")), ps_quote(&controller),
            ps_quote(token), ps_quote(name), ps_quote(&format!("{base}/downloads"))
        );
        let script = if edge_tls_fingerprint.is_empty() {
            script
        } else {
            script.replace(
                " -DownloadBase ",
                &format!(" -TlsPin {} -DownloadBase ", ps_quote(edge_tls_fingerprint)),
            )
        };
        v.insert("windows".into(), json!(script));
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
    let kind = if b.kind.is_empty() {
        "client"
    } else {
        b.kind.as_str()
    };
    if !["client", "node"].contains(&kind) {
        return Err(ApiError::bad("kind must be client or node"));
    }
    if kind == "client" && b.node_id.is_empty() && !st.cfg.builtin_edge {
        return Err(ApiError::bad(
            "the built-in node is disabled; choose an external node (node_id)",
        ));
    }
    if kind == "node" && st.cfg.node_addr.is_empty() {
        return Err(ApiError::bad(
            "the node port is not enabled (set RUSTFRP_NODE_ADDR on the server)",
        ));
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
    let (kind_s, node_s) = (kind.to_string(), b.node_id.clone());
    let (rec, node) = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let node = if node_s.is_empty() {
                None
            } else {
                Some(store::get_node(&tx, &node_s)?)
            };
            let rec = store::create_token_for(&tx, &th, &name, &actor, now, ttl, &kind_s, &node_s)?;
            store::audit(
                &tx,
                &actor,
                if kind_s == "node" {
                    "node_token.create"
                } else {
                    "enroll_token.create"
                },
                "enroll_token",
                &rec.id,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok((rec, node))
        })
        .await?;
    // 明文令牌只在这一次响应里出现。
    let cmds = if kind == "node" {
        node_commands(&st.cfg, &token, &rec.name)
    } else if let Some(n) = node {
        if n.public_host.is_empty() || n.edge_port == 0 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "conflict",
                "this node has not reported its public host yet; connect the node first",
            ));
        }
        commands_to(
            &st.cfg,
            &token,
            &rec.name,
            &Target {
                host: n.public_host,
                port: n.edge_port,
                fingerprint: n.tls_fingerprint,
            },
        )
    } else {
        commands(&st.cfg, &token, &rec.name)
    };
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id": rec.id, "token": token, "expires_at": rec.expires_at, "kind": rec.kind, "commands": cmds}),
        ),
    ))
}

/// 接入新节点的命令：节点端口与内置 Edge 共用证书，所以沿用同一个指纹。
fn node_commands(cfg: &Config, token: &str, name: &str) -> serde_json::Value {
    let port = cfg
        .node_addr
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(7001);
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let controller = format!("{}:{port}", cfg.public_host);
    let pin = if cfg.edge_tls_fingerprint.is_empty() {
        String::new()
    } else {
        format!(" --tls-pin {}", quote(&cfg.edge_tls_fingerprint))
    };
    json!({
        "controller": controller,
        "direct": format!(
            "rustfrp-agent --mode edge --controller {} --enroll-token {} --name {} --public-host <this-node-public-host>{pin}",
            quote(&controller), quote(token), quote(name)
        ),
    })
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

#[derive(Deserialize)]
struct NewAdmin {
    name: String,
    password: String,
    role: Role,
    #[serde(default)]
    reason: String,
}

async fn create_admin(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<NewAdmin>,
) -> Result<(StatusCode, Json<Admin>), ApiError> {
    a.require(&st, Role::Owner).await?;
    let why = reason(&b.reason)?;
    auth::check_password(&b.password).map_err(ApiError::bad)?;
    let hash = auth::hash_password(&b.password)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e))?;
    let (actor, ip, name, role) = (a.admin.name.clone(), a.ip.clone(), b.name, b.role);
    let created = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let rec = store::create_admin(&tx, &name, &hash, role, now)?;
            store::audit(
                &tx,
                &actor,
                "admin.create",
                "admin",
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
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Deserialize)]
struct PatchAdmin {
    role: Option<Role>,
    disabled: Option<bool>,
    password: Option<String>,
    #[serde(default)]
    reason: String,
}

async fn patch_admin(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<PatchAdmin>,
) -> ApiResult<Admin> {
    a.require(&st, Role::Owner).await?;
    let why = reason(&b.reason)?;
    if b.role.is_none() && b.disabled.is_none() && b.password.is_none() {
        return Err(ApiError::bad("nothing to change"));
    }
    if id == a.admin.id && (b.role.is_some() || b.disabled.is_some()) {
        return Err(ApiError::forbidden("cannot change your own role or status"));
    }
    let hash = match &b.password {
        Some(p) => {
            auth::check_password(p).map_err(ApiError::bad)?;
            Some(
                auth::hash_password(p)
                    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e))?,
            )
        }
        None => None,
    };
    let (actor, ip, id2) = (a.admin.name.clone(), a.ip.clone(), id);
    let (role, disabled) = (b.role, b.disabled);
    let updated = st
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let rec = store::update_admin(&tx, &id2, role, disabled, hash.as_deref(), now)?;
            store::audit(
                &tx,
                &actor,
                "admin.update",
                "admin",
                &id2,
                &why,
                "ok",
                &ip,
                now,
            )?;
            tx.commit()?;
            Ok(rec)
        })
        .await?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
struct LogQuery {
    after: Option<u64>,
}

async fn controller_logs(
    State(st): State<AppState>,
    a: Auth,
    Query(query): Query<LogQuery>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Admin).await?;
    let (cursor, gap, items) = st.logs.snapshot_after(query.after.unwrap_or(0));
    Ok(Json(
        json!({"items":items,"capacity":128,"line_bytes":512,"cursor":cursor,"gap":gap}),
    ))
}

#[derive(Deserialize)]
struct NewApiToken {
    name: String,
    scope: String,
    days: i64,
    #[serde(default)]
    reason: String,
}
async fn list_api_tokens(
    State(st): State<AppState>,
    a: Auth,
) -> ApiResult<Vec<crate::api_tokens::ApiToken>> {
    a.require(&st, Role::Owner).await?;
    let id = a.admin.id;
    Ok(Json(
        st.db.call(move |c| crate::api_tokens::list(c, &id)).await?,
    ))
}
async fn create_api_token(
    State(st): State<AppState>,
    a: Auth,
    Json(b): Json<NewApiToken>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Owner).await?;
    let why = reason(&b.reason)?;
    let token = auth::new_token()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "rng", e))?;
    let hash = token_hash(&token);
    let info = st
        .db
        .call(move |c| {
            crate::api_tokens::create(
                c,
                &a.admin,
                crate::api_tokens::NewToken {
                    name: &b.name,
                    scope: &b.scope,
                    days: b.days,
                },
                &hash,
                &why,
                &a.ip,
                store::now_ms(),
            )
        })
        .await?;
    Ok(Json(json!({"token":token,"info":info})))
}
async fn revoke_api_token(
    State(st): State<AppState>,
    a: Auth,
    Path(id): Path<String>,
    Json(b): Json<ReasonBody>,
) -> ApiResult<serde_json::Value> {
    a.require(&st, Role::Owner).await?;
    let why = reason(&b.reason)?;
    st.db
        .call(move |c| crate::api_tokens::revoke(c, &a.admin, &id, &why, &a.ip, store::now_ms()))
        .await?;
    Ok(Json(json!({"ok":true})))
}

// ───────────── 静态面板与安全头 ─────────────

async fn panel(State(st): State<AppState>, req: Request) -> Response {
    if req.uri().path().starts_with("/api/") {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found").into_response();
    }
    crate::panel::serve(
        st.cfg.panel_dir.as_deref(),
        req.uri().path(),
        req.method(),
        req.headers(),
    )
    .await
}

async fn security_headers(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(h, "content-security-policy", "default-src 'self'; script-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self'; font-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'");
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
        .route("/logs", get(controller_logs))
        .route("/api-tokens", get(list_api_tokens).post(create_api_token))
        .route("/api-tokens/{id}/revoke", post(revoke_api_token))
        .route("/overview", get(overview))
        .route("/traffic", get(traffic_history))
        .route("/node", get(node))
        .route("/nodes", get(list_nodes))
        .route("/nodes/{id}", patch(patch_node).delete(delete_node))
        .route("/clients/{id}/config/preview", post(preview_config))
        .route("/clients/{id}/config/import", post(import_config))
        .route("/clients/{id}/config/export", get(export_config))
        .route("/clients", get(list_clients))
        .route(
            "/clients/{id}",
            get(get_client).patch(patch_client).delete(delete_client),
        )
        .route("/clients/{id}/kick", post(kick_client))
        .route("/clients/{id}/restart", post(restart_client))
        .route("/clients/{id}/migrate", post(move_client))
        .route("/clients/{id}/update", post(update_client))
        .route("/tunnels", get(list_tunnels).post(create_tunnel))
        .route("/tunnels/batch", post(batch_tunnels))
        .route(
            "/tunnels/{id}",
            get(get_tunnel).patch(patch_tunnel).delete(delete_tunnel),
        )
        .route("/visitors", get(list_visitors).post(create_visitor))
        .route(
            "/visitors/{id}",
            patch(patch_visitor).delete(delete_visitor),
        )
        .route("/enroll-tokens", get(list_tokens).post(create_token))
        .route("/enroll-tokens/{id}", delete(revoke_token))
        .route("/audit", get(list_audit))
        .route("/admins", get(list_admins).post(create_admin))
        .route("/admins/{id}", patch(patch_admin));
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route(
            "/install.sh",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../../installer/install.sh"),
                )
            }),
        )
        .route(
            "/install.ps1",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../../installer/install.ps1"),
                )
            }),
        )
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
    use crate::panel::{content_type, relative_path as panel_relative};

    #[test]
    fn process_metric_lines_include_optional_linux_measurements_only_when_present() {
        let linux = process_metric_lines(&crate::process::ProcessStats {
            uptime_seconds: 17,
            rss_bytes: Some(4096),
            cpu_seconds: Some(2.5),
        });
        assert!(linux.contains("rustfrp_process_uptime_seconds 17"));
        assert!(linux.contains("rustfrp_process_resident_memory_bytes 4096"));
        assert!(linux.contains("rustfrp_process_cpu_seconds_total 2.50"));

        let other_platform = process_metric_lines(&crate::process::ProcessStats {
            uptime_seconds: 8,
            rss_bytes: None,
            cpu_seconds: None,
        });
        assert!(other_platform.contains("rustfrp_process_uptime_seconds 8"));
        assert!(!other_platform.contains("resident_memory"));
        assert!(!other_platform.contains("cpu_seconds_total"));
    }

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
        assert_eq!(panel_relative("/app.js"), Some("app.js".into()));
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
    fn reserved_and_listening_ports_are_refused() {
        let cfg = Config {
            reserved_ports: vec![(2200, 2299)],
            vhost_http: Some(8081),
            vhost_https: Some(8443),
            edge_addr: "0.0.0.0:7100".into(),
            http_addr: "127.0.0.1:8088".into(),
            ..Config::default()
        };
        for p in [2200, 2250, 2299, 7100, 8081, 8443, 8088] {
            let e = port_policy(&cfg, p).unwrap_err();
            assert!(e.message.contains("reserved"), "{p}: {}", e.message);
        }
        for p in [0, 2199, 2300, 7000, 22] {
            assert!(port_policy(&cfg, p).is_ok(), "{p}");
        }
    }

    #[test]
    fn vhost_policy_rules() {
        let off = Config::default();
        assert!(vhost_policy(&off, "tcp", "").is_ok());
        assert!(vhost_policy(&off, "http", "").is_err());
        assert!(vhost_policy(&off, "https", "").is_err());
        assert!(vhost_policy(&off, "udp", "x").is_err());
        let on = Config {
            vhost_http: Some(80),
            vhost_https: Some(443),
            subdomain_host: "t.com".into(),
            ..Config::default()
        };
        assert!(vhost_policy(&on, "http", "web").is_ok() && vhost_policy(&on, "https", "").is_ok());
    }

    #[test]
    fn install_command_preserves_shell_literals() {
        let name = "中文 ' \" $(printf injected); `printf injected`\nnext";
        let cfg = Config {
            public_url: "https://example.test/".into(),
            ..Config::default()
        };
        let v = commands(&cfg, "t'ok", name);
        let command = v["direct"].as_str().unwrap();
        let args = command.strip_prefix("rustfrp-agent ").unwrap();
        let script = format!("set -- {args}; printf '%s\\n' \"$8\"");
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{name}\n")
        );
        let windows = v["windows"].as_str().unwrap();
        assert!(windows.contains("-EnrollToken 't''ok'"));
        assert!(windows.contains("-Name '中文 '' "));
        assert!(!windows.contains("test//install"));
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
            .contains("--enroll-token 'rfrp_tok' --name 'my \"box\"'"));
        assert!(v.get("linux_macos").is_none());
        cfg.public_url = "https://frp.example.com".into();
        let v = commands(&cfg, "t", "n");
        assert!(v["linux_macos"]
            .as_str()
            .unwrap()
            .starts_with("curl -fsSL 'https://frp.example.com/install.sh' | sh -s --"));
        assert!(v["windows"].as_str().unwrap().contains("install.ps1"));
    }
    #[test]
    fn managed_commands_fix_the_actual_leaf_certificate() {
        let cfg = Config {
            public_url: "https://panel.test".into(),
            edge_tls_fingerprint: "a".repeat(64),
            ..Default::default()
        };
        let v = commands(&cfg, "token", "name");
        assert!(v["direct"].as_str().unwrap().contains("--tls-pin"));
        assert!(v["linux_macos"].as_str().unwrap().contains("--tls-pin"));
        assert!(v["windows"].as_str().unwrap().contains("-TlsPin"));
    }
}

#[cfg(test)]
mod feature_route_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use rustfrp_core::node::NodeInfo;
    use serde_json::Value;
    use tower::ServiceExt;

    const PUBLIC_KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Rig {
        app: Router,
        db: Db,
        cookie: String,
        csrf: String,
    }

    impl Rig {
        async fn new() -> Self {
            let cfg = Config {
                vhost_http: Some(8080),
                vhost_https: Some(8443),
                tcpmux_http_connect_port: Some(7443),
                subdomain_host: "tunnel.example.test".into(),
                edge_tls_fingerprint: "ab".repeat(32),
                ..Config::default()
            };
            Self::with_config(cfg).await
        }

        async fn with_config(cfg: Config) -> Self {
            let db = Db::open_memory().unwrap();
            let (hub, events) = Hub::new(db.clone());
            drop(events);
            let state = AppState {
                db: db.clone(),
                hub,
                cfg: Arc::new(cfg),
                limiter: Arc::new(LoginLimiter::new(5, 60_000)),
                metrics: Arc::default(),
                logs: Arc::default(),
            };
            let admin = db
                .call(|c| store::create_admin(c, "owner", "hash", Role::Owner, 1))
                .await
                .unwrap();
            let token = "feature-route-session".to_string();
            let csrf = "feature-route-csrf".to_string();
            let token_digest = token_hash(&token);
            let csrf_digest = csrf.clone();
            let admin_id = admin.id;
            db.call(move |c| {
                store::create_session(
                    c,
                    &token_digest,
                    &admin_id,
                    &csrf_digest,
                    store::now_ms(),
                    60_000,
                )
            })
            .await
            .unwrap();
            Self {
                app: router(state),
                db,
                cookie: format!("{COOKIE}={token}"),
                csrf,
            }
        }

        async fn client(&self, name: &str) -> String {
            let name = name.to_string();
            self.db
                .call(move |c| {
                    Ok(store::create_client(
                        c,
                        &store::new_id(),
                        &name,
                        PUBLIC_KEY,
                        "linux",
                        "x86_64",
                        "0.1.0",
                        store::now_ms(),
                    )?
                    .id)
                })
                .await
                .unwrap()
        }

        async fn add_node(&self, id: &str, info: NodeInfo, disabled: bool) {
            let id = id.to_string();
            self.db
                .call(move |c| {
                    store::create_node(c, &id, &id, &"cd".repeat(32), &info, store::now_ms())?;
                    if disabled {
                        store::update_node(c, &id, None, Some(true), store::now_ms())?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }

        async fn assign(&self, client: &str, node: &str) {
            let (client, node) = (client.to_string(), node.to_string());
            self.db
                .call(move |c| store::set_client_node(c, &client, &node))
                .await
                .unwrap();
        }

        async fn create_tunnel(
            &self,
            client: &str,
            name: &str,
            kind: &str,
            extra: Value,
        ) -> (StatusCode, Value) {
            let mut body = json!({
                "client_id": client,
                "name": name,
                "kind": kind,
                "local_port": 8080,
                "reason": "node policy test"
            });
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            self.request(Method::POST, "/api/v1/tunnels", body).await
        }

        async fn request(&self, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "panel.test")
                .header(header::COOKIE, &self.cookie)
                .header(header::ORIGIN, "http://panel.test")
                .header("x-csrf-token", &self.csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let response = self.app.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, value)
        }
    }

    #[tokio::test]
    async fn tunnel_visitor_limit_and_migration_routes_preserve_runtime_settings() {
        let rig = Rig::new().await;
        let provider = rig.client("provider").await;
        let visitor_client = rig.client("visitor").await;

        let (status, tcpmux) = rig
            .request(
                Method::POST,
                "/api/v1/tunnels",
                json!({
                    "client_id": provider,
                    "name": "mux",
                    "kind": "tcpmux",
                    "local_port": 22,
                    "remote_port": 0,
                    "custom_domains": ["mux.example.test"],
                    "multiplexer": "httpconnect",
                    "max_connections": 12,
                    "use_encryption": true,
                    "use_compression": true,
                    "reason": "coverage setup"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{tcpmux}");
        assert_eq!(tcpmux["max_connections"], 12);
        assert_eq!(tcpmux["multiplexer"], "httpconnect");
        assert!(tcpmux["use_encryption"].as_bool().unwrap_or(false));
        let provider = tcpmux["client_id"].as_str().unwrap().to_string();

        let (status, udp) = rig
            .request(
                Method::POST,
                "/api/v1/tunnels",
                json!({
                    "client_id": provider,
                    "name": "dns",
                    "kind": "udp",
                    "local_port": 53,
                    "max_connections": 4,
                    "reason": "coverage setup"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{udp}");
        assert_eq!(udp["max_connections"], 4);

        let (status, secret) = rig
            .request(
                Method::POST,
                "/api/v1/tunnels",
                json!({
                    "client_id": provider,
                    "name": "private",
                    "kind": "stcp",
                    "local_port": 22,
                    "max_connections": 3,
                    "reason": "coverage setup"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{secret}");
        assert_eq!(secret["max_connections"], 3);

        let visitor_body = json!({
            "client_id": visitor_client,
            "name": "private-ssh",
            "tunnel_id": secret["id"],
            "bind_port": 0,
            "use_encryption": true,
            "use_compression": true,
            "reason": "coverage setup"
        });
        let (status, visitor) = rig
            .request(Method::POST, "/api/v1/visitors", visitor_body)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{visitor}");
        assert!(visitor["use_encryption"].as_bool().unwrap_or(false));
        assert!(visitor["use_compression"].as_bool().unwrap_or(false));
        let visitor_id = visitor["id"].as_str().unwrap().to_string();
        let (status, visitor) = rig
            .request(
                Method::PATCH,
                &format!("/api/v1/visitors/{visitor_id}"),
                json!({
                    "use_encryption": false,
                    "use_compression": false,
                    "reason": "coverage update"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{visitor}");
        assert_eq!(visitor["use_encryption"], false);

        let (status, client) = rig
            .request(
                Method::PATCH,
                &format!("/api/v1/clients/{provider}"),
                json!({"max_connections": 21, "reason": "coverage limit"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{client}");
        assert_eq!(client["max_connections"], 21);
        let (status, update) = rig
            .request(
                Method::POST,
                &format!("/api/v1/clients/{provider}/update"),
                json!({"version": "v9.9.9", "reason": "coverage update"}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{update}");

        let target_id = "coverage-target-node";
        let key = "cd".repeat(32);
        rig.db
            .call(move |c| {
                store::create_node(
                    c,
                    target_id,
                    "target",
                    &key,
                    &NodeInfo {
                        public_host: "edge.example.test".into(),
                        edge_port: 7443,
                        tls_fingerprint: "ab".repeat(32),
                        tcpmux_http_connect_port: Some(7443),
                        ..Default::default()
                    },
                    store::now_ms(),
                )
            })
            .await
            .unwrap();
        let (status, blocked) = rig
            .request(
                Method::POST,
                &format!("/api/v1/clients/{provider}/migrate"),
                json!({"node_id": target_id, "reason": "coverage migration"}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{blocked}");
        assert!(blocked["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("visitor"));
        let (status, _) = rig
            .request(
                Method::DELETE,
                &format!("/api/v1/visitors/{visitor_id}?reason=coverage"),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, moved) = rig
            .request(
                Method::POST,
                &format!("/api/v1/clients/{provider}/migrate"),
                json!({"node_id": target_id, "reason": "coverage migration"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert_eq!(moved["node_id"], target_id);
    }

    #[tokio::test]
    async fn node_capabilities_gate_tunnel_creation_and_client_migration() {
        let rig = Rig::new().await;
        let caps = [
            (
                "http",
                json!({"custom_domains": ["http-move.test"]}),
                None,
                "does not support the existing http",
            ),
            (
                "https",
                json!({"custom_domains": ["https-move.test"]}),
                None,
                "does not support the existing https",
            ),
            (
                "tcpmux",
                json!({"custom_domains": ["mux-move.test"], "multiplexer": "httpconnect"}),
                None,
                "does not support the existing tcpmux",
            ),
            (
                "tcp",
                json!({"remote_port": 6500}),
                Some(("port", "6000")),
                "does not allow existing port",
            ),
            (
                "http",
                json!({"subdomain": "api"}),
                Some(("subdomain", "")),
                "does not support the existing http",
            ),
        ];
        for (index, (kind, extra, capability, expected)) in caps.into_iter().enumerate() {
            let client = rig.client(&format!("cap-client-{index}")).await;
            let mut tunnel = json!({
                "client_id": client,
                "name": format!("cap-tunnel-{index}"),
                "kind": kind,
                "local_port": 80,
                "reason": "capability test"
            });
            if let Some(fields) = extra.as_object() {
                tunnel.as_object_mut().unwrap().extend(fields.clone());
            }
            if kind == "tcp" {
                tunnel["remote_port"] = json!(6500);
            }
            let (status, created) = rig.request(Method::POST, "/api/v1/tunnels", tunnel).await;
            assert_eq!(status, StatusCode::CREATED, "{created}");

            let node_id = format!("cap-node-{index}");
            let node_id_for_db = node_id.clone();
            let info = match capability {
                Some(("subdomain", _)) => NodeInfo {
                    public_host: format!("edge-{index}.example.test"),
                    edge_port: 7443,
                    tls_fingerprint: "ab".repeat(32),
                    vhost_http: Some(8080),
                    ..Default::default()
                },
                Some(("port", _)) => NodeInfo {
                    public_host: format!("edge-{index}.example.test"),
                    edge_port: 7443,
                    tls_fingerprint: "ab".repeat(32),
                    allow_ports: vec![(6000, 6100)],
                    ..Default::default()
                },
                _ => NodeInfo {
                    public_host: format!("edge-{index}.example.test"),
                    edge_port: 7443,
                    tls_fingerprint: "ab".repeat(32),
                    ..Default::default()
                },
            };
            let expected_http = info.vhost_http;
            rig.db
                .call(move |c| {
                    store::create_node(
                        c,
                        &node_id_for_db,
                        &node_id_for_db,
                        &"cd".repeat(32),
                        &info,
                        store::now_ms(),
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            let stored_node = rig
                .db
                .call({
                    let node_id = node_id.clone();
                    move |c| store::get_node(c, &node_id)
                })
                .await
                .unwrap();
            assert_eq!(stored_node.vhost_http, expected_http, "{node_id}");
            let (status, result) = rig
                .request(
                    Method::POST,
                    &format!("/api/v1/clients/{client}/migrate"),
                    json!({"node_id": node_id, "reason": "capability test"}),
                )
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
            assert!(
                result["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(expected),
                "expected {expected:?}, got {result}"
            );
        }

        let offline_config = Config {
            builtin_edge: false,
            vhost_http: Some(8080),
            ..Config::default()
        };
        let no_builtin = Rig::with_config(offline_config).await;
        let client = no_builtin.client("no-builtin").await;
        let (status, result) = no_builtin
            .request(
                Method::POST,
                "/api/v1/tunnels",
                json!({
                    "client_id": client,
                    "name": "http",
                    "kind": "http",
                    "local_port": 8080,
                    "custom_domains": ["no-builtin.test"],
                    "reason": "node capability test"
                }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
        assert!(result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("built-in edge is disabled"));

        let empty_policy = Config::default();
        assert!(vhost_policy(&empty_policy, "http", "").is_err());
        assert!(vhost_policy(&empty_policy, "https", "").is_err());
        assert!(vhost_policy(&empty_policy, "tcpmux", "").is_err());
        assert!(vhost_policy(&empty_policy, "tcp", "api").is_err());
    }

    #[tokio::test]
    async fn external_node_policy_checks_vhost_and_disabled_capabilities() {
        let rig = Rig::new().await;
        let info = |index: u16| NodeInfo {
            public_host: format!("edge-{index}.example.test"),
            edge_port: 7443,
            tls_fingerprint: "ab".repeat(32),
            ..Default::default()
        };

        for (suffix, kind, extra, expected) in [
            (
                "http-port",
                "http",
                json!({"custom_domains": ["missing-http.example.test"]}),
                "vhost HTTP port",
            ),
            (
                "https-port",
                "https",
                json!({"custom_domains": ["missing-https.example.test"]}),
                "vhost HTTPS port",
            ),
            (
                "mux-port",
                "tcpmux",
                json!({"custom_domains": ["missing-mux.example.test"], "multiplexer": "httpconnect"}),
                "tcpmuxHTTPConnectPort",
            ),
            (
                "subdomain",
                "http",
                json!({"subdomain": "api"}),
                "subdomains need",
            ),
        ] {
            let node_id = format!("node-{suffix}");
            let mut capabilities = info(7000 + suffix.len() as u16);
            if suffix == "https-port" || suffix == "mux-port" || suffix == "subdomain" {
                capabilities.vhost_http = Some(8080);
            }
            if suffix == "mux-port" {
                capabilities.vhost_https = Some(8443);
            }
            rig.add_node(&node_id, capabilities, false).await;
            let client = rig.client(&format!("client-{suffix}")).await;
            rig.assign(&client, &node_id).await;
            let (status, result) = rig
                .create_tunnel(&client, &format!("tunnel-{suffix}"), kind, extra)
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
            assert!(
                result["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(expected),
                "{result}"
            );
        }

        let disabled_id = "node-disabled-policy";
        rig.add_node(disabled_id, info(7443), true).await;
        let disabled_client = rig.client("disabled-policy-client").await;
        rig.assign(&disabled_client, disabled_id).await;
        let (status, result) = rig
            .create_tunnel(&disabled_client, "disabled-policy-tunnel", "tcp", json!({}))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
        assert!(result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("client's node is disabled"));

        let active_id = "node-active-policy";
        rig.add_node(
            active_id,
            NodeInfo {
                vhost_http: Some(8080),
                vhost_https: Some(8443),
                tcpmux_http_connect_port: Some(7443),
                subdomain_host: "node.example.test".into(),
                ..info(7555)
            },
            false,
        )
        .await;
        let active_client = rig.client("active-policy-client").await;
        rig.assign(&active_client, active_id).await;
        let (status, created) = rig
            .create_tunnel(
                &active_client,
                "active-policy-mux",
                "tcpmux",
                json!({"custom_domains": ["active-mux.example.test"], "multiplexer": "httpconnect"}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let (status, returned) = rig
            .request(
                Method::POST,
                &format!("/api/v1/clients/{active_client}/migrate"),
                json!({"node_id": "", "reason": "return to builtin"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{returned}");
        assert_eq!(returned["node_id"], "");

        let no_builtin = Rig::with_config(Config {
            builtin_edge: false,
            ..Config::default()
        })
        .await;
        let disabled_client = no_builtin.client("disabled-migration-client").await;
        let (status, disabled_move) = no_builtin
            .request(
                Method::POST,
                &format!("/api/v1/clients/{disabled_client}/migrate"),
                json!({"node_id": "", "reason": "disabled built-in edge"}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{disabled_move}");
        assert!(disabled_move["error"]["message"]
            .as_str()
            .unwrap()
            .contains("built-in edge is disabled"));
    }

    #[tokio::test]
    async fn node_tunnel_and_owner_routes_reject_noop_or_unsafe_changes() {
        let rig = Rig::new().await;
        rig.add_node("patch-node", NodeInfo::default(), false).await;
        let (status, no_node_change) = rig
            .request(
                Method::PATCH,
                "/api/v1/nodes/patch-node",
                json!({"reason": "no changes"}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{no_node_change}");
        assert!(no_node_change["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nothing to change"));
        let (status, disabled) = rig
            .request(
                Method::PATCH,
                "/api/v1/nodes/patch-node",
                json!({"disabled": true, "reason": "maintenance"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{disabled}");
        assert!(disabled["disabled_at"].is_i64());
        let (status, enabled) = rig
            .request(
                Method::PATCH,
                "/api/v1/nodes/patch-node",
                json!({"disabled": false, "reason": "maintenance complete"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{enabled}");
        assert!(enabled["disabled_at"].is_null());

        let client = rig.client("patch-tunnel-client").await;
        let (status, tunnel) = rig
            .create_tunnel(
                &client,
                "patch-subdomain",
                "http",
                json!({"custom_domains": ["patch.example.test"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{tunnel}");
        let tunnel_id = tunnel["id"].as_str().unwrap();
        let (status, patched) = rig
            .request(
                Method::PATCH,
                &format!("/api/v1/tunnels/{tunnel_id}"),
                json!({"subdomain": "patch", "reason": "add subdomain"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{patched}");
        assert_eq!(patched["subdomain"], "patch");

        let owner_id = rig
            .db
            .call(|c| Ok(store::list_admins(c)?.remove(0).id))
            .await
            .unwrap();
        let (status, no_admin_change) = rig
            .request(
                Method::PATCH,
                &format!("/api/v1/admins/{owner_id}"),
                json!({"reason": "no changes"}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{no_admin_change}");
        let (status, self_change) = rig
            .request(
                Method::PATCH,
                &format!("/api/v1/admins/{owner_id}"),
                json!({"role": "viewer", "reason": "self change"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{self_change}");

        let (status, unready) = rig
            .request(
                Method::POST,
                "/api/v1/enroll-tokens",
                json!({"name": "unready client", "node_id": "patch-node", "reason": "setup"}),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{unready}");
        assert!(unready["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has not reported its public host"));
    }
}
