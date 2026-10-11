//! 管理 API：认证、CSRF、权限、审计、CRUD 与静态面板。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use rustfrp_server::api::{router, AppState, LoginLimiter, COOKIE};
use rustfrp_server::auth::{hash_password, token_hash};
use rustfrp_server::config::Config;
use rustfrp_server::db::Db;
use rustfrp_server::hub::Hub;
use rustfrp_server::models::Role;
use rustfrp_server::store;
use serde_json::{json, Value};
use tower::ServiceExt;

const PK: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PASSWORD: &str = "correct horse battery";

fn pw_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password(PASSWORD).unwrap())
}

struct Rig {
    app: Router,
    db: Db,
    hub: Arc<Hub>,
}

#[derive(Clone)]
struct Sess {
    cookie: String,
    csrf: String,
}

async fn rig_with(cfg: Config) -> Rig {
    let db = Db::open_memory().unwrap();
    let (hub, _events) = Hub::new(db.clone());
    let state = AppState {
        db: db.clone(),
        hub: hub.clone(),
        cfg: Arc::new(cfg),
        limiter: Arc::new(LoginLimiter::new(5, 600_000)),
    };
    Rig {
        app: router(state),
        db,
        hub,
    }
}

async fn rig() -> Rig {
    rig_with(Config::default()).await
}

impl Rig {
    async fn admin(&self, name: &str, role: Role) -> Sess {
        let (n, r) = (name.to_string(), role);
        let a = self
            .db
            .call(move |c| store::create_admin(c, &n, pw_hash(), r, 1))
            .await
            .unwrap();
        let (token, csrf) = (format!("tok-{name}"), format!("csrf-{name}"));
        let (th, c2) = (token_hash(&token), csrf.clone());
        self.db
            .call(move |c| store::create_session(c, &th, &a.id, &c2, store::now_ms(), 3_600_000))
            .await
            .unwrap();
        Sess {
            cookie: format!("{COOKIE}={token}"),
            csrf,
        }
    }

    async fn client(&self, name: &str) -> String {
        let n = name.to_string();
        self.db
            .call(move |c| {
                store::create_client(c, &store::new_id(), &n, PK, "linux", "x86_64", "0.1", 1)
            })
            .await
            .unwrap()
            .id
    }

    async fn req(
        &self,
        method: Method,
        path: &str,
        s: Option<&Sess>,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut b = Request::builder().method(method.clone()).uri(path);
        b = b.header(header::HOST, "panel.test");
        if let Some(s) = s {
            b = b.header(header::COOKIE, &s.cookie);
            if method != Method::GET {
                b = b
                    .header("x-csrf-token", &s.csrf)
                    .header(header::ORIGIN, "http://panel.test");
            }
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let res = self
            .app
            .clone()
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, headers, json)
    }

    async fn get(&self, path: &str, s: &Sess) -> (StatusCode, Value) {
        let (st, _, v) = self.req(Method::GET, path, Some(s), None).await;
        (st, v)
    }
    async fn send(&self, m: Method, path: &str, s: &Sess, body: Value) -> (StatusCode, Value) {
        let (st, _, v) = self.req(m, path, Some(s), Some(body)).await;
        (st, v)
    }
    async fn audit_actions(&self) -> Vec<String> {
        self.db
            .call(|c| store::list_audit(c, 500, None))
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.action)
            .collect()
    }
}

fn err_code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn healthz_and_security_headers() {
    let r = rig().await;
    let (st, h, v) = r.req(Method::GET, "/healthz", None, None).await;
    assert_eq!((st, v["status"].as_str()), (StatusCode::OK, Some("ok")));
    assert!(h["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
    let (_, h, _) = r.req(Method::GET, "/api/v1/auth/me", None, None).await;
    assert_eq!(h["cache-control"], "no-store");
}

#[tokio::test]
async fn login_logout_cycle_and_guards() {
    let r = rig().await;
    let _ = r.admin("boss", Role::Owner).await;
    let login = |pw: &'static str| {
        let r = &r;
        async move {
            let req = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/login")
                .header(header::HOST, "panel.test")
                .header(header::ORIGIN, "http://panel.test")
                .header("x-rustfrp-admin", "1")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"name": "boss", "password": pw}).to_string(),
                ))
                .unwrap();
            let res = r.app.clone().oneshot(req).await.unwrap();
            let (st, h) = (res.status(), res.headers().clone());
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            (st, h, v)
        }
    };

    // 缺少自定义请求头 / 跨源：拒绝（防登录 CSRF）。
    let (st, _, _) = r
        .req(
            Method::POST,
            "/api/v1/auth/login",
            None,
            Some(json!({"name":"boss","password":PASSWORD})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::HOST, "panel.test")
        .header(header::ORIGIN, "http://evil.test")
        .header("x-rustfrp-admin", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"name":"boss","password":PASSWORD}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        r.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    let (st, _, v) = login("wrong password").await;
    assert_eq!(
        (st, err_code(&v)),
        (StatusCode::UNAUTHORIZED, "invalid_credentials")
    );

    let (st, h, v) = login(PASSWORD).await;
    assert_eq!(st, StatusCode::OK);
    let cookie = h[header::SET_COOKIE].to_str().unwrap().to_string();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && !cookie.contains("Secure")
    );
    let csrf = v["csrf"].as_str().unwrap().to_string();
    assert_eq!(v["admin"]["name"], "boss");
    let sess = Sess {
        cookie: cookie.split(';').next().unwrap().to_string(),
        csrf,
    };
    let (st, v) = r.get("/api/v1/auth/me", &sess).await;
    assert_eq!(
        (st, v["admin"]["role"].as_str()),
        (StatusCode::OK, Some("owner"))
    );

    let (st, h, _) = r
        .req(
            Method::POST,
            "/api/v1/auth/logout",
            Some(&sess),
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        r.get("/api/v1/auth/me", &sess).await.0,
        StatusCode::UNAUTHORIZED
    );

    let actions = r.audit_actions().await;
    assert_eq!(actions.iter().filter(|a| *a == "auth.login").count(), 2);
}

#[tokio::test]
async fn login_rate_limit_unknown_user_and_disabled_admin() {
    let r = rig().await;
    r.admin("boss", Role::Owner).await;
    let attempt = |name: &'static str, pw: &'static str| {
        let r = &r;
        async move {
            let req = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/auth/login")
                .header(header::HOST, "panel.test")
                .header("x-rustfrp-admin", "1")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"name": name, "password": pw}).to_string(),
                ))
                .unwrap();
            r.app.clone().oneshot(req).await.unwrap().status()
        }
    };
    // 不存在的用户与错误密码返回同样的状态，不泄露账号是否存在。
    assert_eq!(
        attempt("ghost", "whatever-password").await,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..4 {
        assert_eq!(attempt("boss", "bad").await, StatusCode::UNAUTHORIZED);
    }
    // 第 5 次失败之后同一来源被限流（即使密码正确）。
    assert_eq!(
        attempt("boss", PASSWORD).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    let r2 = rig().await;
    r2.admin("off", Role::Admin).await;
    r2.db
        .call(|c| store::set_admin_disabled(c, "off", true, 5))
        .await
        .unwrap();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::HOST, "panel.test")
        .header("x-rustfrp-admin", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"name":"off","password":PASSWORD}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        r2.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected_everywhere() {
    let r = rig().await;
    for (m, p) in [
        (Method::GET, "/api/v1/overview"),
        (Method::GET, "/api/v1/node"),
        (Method::GET, "/api/v1/clients"),
        (Method::GET, "/api/v1/tunnels"),
        (Method::POST, "/api/v1/tunnels"),
        (Method::GET, "/api/v1/enroll-tokens"),
        (Method::GET, "/api/v1/audit"),
        (Method::GET, "/api/v1/admins"),
    ] {
        let body = (m == Method::POST).then(|| json!({}));
        let (st, _, v) = r.req(m, p, None, body).await;
        assert_eq!(
            (st, err_code(&v)),
            (StatusCode::UNAUTHORIZED, "unauthorized"),
            "{p}"
        );
    }
    let bogus = Sess {
        cookie: format!("{COOKIE}=nonsense"),
        csrf: "x".into(),
    };
    assert_eq!(
        r.get("/api/v1/overview", &bogus).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn csrf_and_origin_are_enforced_on_writes() {
    let r = rig().await;
    let s = r.admin("op", Role::Owner).await;
    let cid = r.client("c1").await;
    let body = json!({"client_id": cid, "name": "t", "local_port": 22, "reason": "why"});

    let mut no_csrf = s.clone();
    no_csrf.csrf = String::new();
    let (st, v) = r
        .send(Method::POST, "/api/v1/tunnels", &no_csrf, body.clone())
        .await;
    assert_eq!((st, err_code(&v)), (StatusCode::FORBIDDEN, "forbidden"));
    let mut wrong = s.clone();
    wrong.csrf = "forged".into();
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &wrong, body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/tunnels")
        .header(header::HOST, "panel.test")
        .header(header::ORIGIN, "http://evil.test")
        .header(header::COOKIE, &s.cookie)
        .header("x-csrf-token", &s.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        r.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &s, body).await.0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn roles_are_enforced_and_denials_are_audited() {
    let r = rig().await;
    let viewer = r.admin("v", Role::Viewer).await;
    let operator = r.admin("o", Role::Operator).await;
    let admin = r.admin("a", Role::Admin).await;
    let owner = r.admin("w", Role::Owner).await;
    let cid = r.client("c1").await;

    // viewer：只读。
    for p in [
        "/api/v1/overview",
        "/api/v1/node",
        "/api/v1/clients",
        "/api/v1/tunnels",
    ] {
        assert_eq!(r.get(p, &viewer).await.0, StatusCode::OK, "{p}");
    }
    let t = json!({"client_id": cid, "name": "t", "local_port": 22, "reason": "x"});
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &viewer, t.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        r.get("/api/v1/audit", &viewer).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        r.get("/api/v1/enroll-tokens", &viewer).await.0,
        StatusCode::FORBIDDEN
    );

    // operator：能管隧道，不能管客户端与令牌。
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &operator, t)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        r.send(
            Method::PATCH,
            &format!("/api/v1/clients/{cid}"),
            &operator,
            json!({"name":"x","reason":"r"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        r.send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &operator,
            json!({"name":"n","reason":"r"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // admin：能管客户端与令牌，但看不到管理员列表。
    assert_eq!(r.get("/api/v1/audit", &admin).await.0, StatusCode::OK);
    assert_eq!(
        r.get("/api/v1/admins", &admin).await.0,
        StatusCode::FORBIDDEN
    );
    let (st, v) = r.get("/api/v1/admins", &owner).await;
    assert_eq!((st, v.as_array().unwrap().len()), (StatusCode::OK, 4));
    assert!(
        v.to_string().find("password").is_none(),
        "password hashes are never exposed"
    );

    let denied = r
        .audit_actions()
        .await
        .iter()
        .filter(|a| *a == "permission.denied")
        .count();
    assert!(denied >= 5, "denials are audited: {denied}");
}

#[tokio::test]
async fn overview_and_node() {
    let r = rig_with(Config {
        public_host: "frp.example.com".into(),
        edge_addr: "0.0.0.0:7100".into(),
        allow_ports: vec![(6000, 6100)],
        ..Config::default()
    })
    .await;
    let s = r.admin("o", Role::Viewer).await;
    let cid = r.client("c1").await;
    let (_, v) = r.get("/api/v1/overview", &s).await;
    assert_eq!(
        (
            v["counts"]["clients"].as_i64(),
            v["clients_online"].as_i64()
        ),
        (Some(1), Some(0))
    );
    let (_, v) = r.get("/api/v1/node", &s).await;
    assert_eq!(
        (
            v["public_host"].as_str(),
            v["edge_port"].as_i64(),
            v["allow_ports"][0][0].as_i64()
        ),
        (Some("frp.example.com"), Some(7100), Some(6000))
    );
    let (_, v) = r.get(&format!("/api/v1/clients/{cid}"), &s).await;
    assert_eq!(v["online"], false);
    assert!(v.get("pubkey").is_none(), "public keys stay server-side");
}

#[tokio::test]
async fn client_management() {
    let r = rig().await;
    let admin = r.admin("a", Role::Admin).await;
    let id = r.client("laptop").await;

    let (_, v) = r.get("/api/v1/clients", &admin).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(
        r.get("/api/v1/clients/nope", &admin).await.0,
        StatusCode::NOT_FOUND
    );

    let path = format!("/api/v1/clients/{id}");
    let (st, v) = r
        .send(Method::PATCH, &path, &admin, json!({"name":"renamed"}))
        .await;
    assert_eq!(
        (st, err_code(&v)),
        (StatusCode::BAD_REQUEST, "bad_request"),
        "reason is required"
    );
    let (st, v) = r
        .send(
            Method::PATCH,
            &path,
            &admin,
            json!({"name":"renamed","reason":"tidy"}),
        )
        .await;
    assert_eq!((st, v["name"].as_str()), (StatusCode::OK, Some("renamed")));
    let (st, v) = r
        .send(
            Method::PATCH,
            &path,
            &admin,
            json!({"disabled":true,"reason":"lost laptop"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["disabled_at"].is_i64());
    assert_eq!(
        r.send(
            Method::PATCH,
            &path,
            &admin,
            json!({"name":"","reason":"r"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(
            Method::PATCH,
            "/api/v1/clients/nope",
            &admin,
            json!({"reason":"r"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        r.send(Method::POST, &format!("{path}/kick"), &admin, json!({}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(
            Method::POST,
            &format!("{path}/kick"),
            &admin,
            json!({"reason":"reset"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        r.send(
            Method::POST,
            "/api/v1/clients/nope/kick",
            &admin,
            json!({"reason":"r"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let (st, _, v) = r.req(Method::DELETE, &path, Some(&admin), None).await;
    assert_eq!((st, err_code(&v)), (StatusCode::BAD_REQUEST, "bad_request"));
    let (st, _, _) = r
        .req(
            Method::DELETE,
            &format!("{path}?reason=decommissioned"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r.get(&path, &admin).await.0, StatusCode::NOT_FOUND);
    let (st, _, _) = r
        .req(
            Method::DELETE,
            &format!("{path}?reason=again"),
            Some(&admin),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let actions = r.audit_actions().await;
    for a in ["client.update", "client.kick", "client.delete"] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
}

#[tokio::test]
async fn tunnel_crud_and_validation() {
    let r = rig_with(Config {
        allow_ports: vec![(6000, 6100)],
        ..Config::default()
    })
    .await;
    let op = r.admin("o", Role::Operator).await;
    let cid = r.client("c1").await;
    let cid2 = r.client("c2").await;

    let mk = |name: &str, remote: i64| json!({"client_id": cid, "name": name, "local_port": 22, "remote_port": remote, "reason": "need ssh"});
    let (st, t) = r
        .send(Method::POST, "/api/v1/tunnels", &op, mk("ssh", 6000))
        .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(
        (
            t["kind"].as_str(),
            t["local_ip"].as_str(),
            t["state"].as_str(),
            t["enabled"].as_bool()
        ),
        (Some("tcp"), Some("127.0.0.1"), Some("pending"), Some(true))
    );
    let tid = t["id"].as_str().unwrap().to_string();

    // 校验：端口冲突 409、超出允许范围 400、类型不支持 400、缺 reason 400、客户端不存在 404。
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &op, mk("dup", 6000))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (st, v) = r
        .send(Method::POST, "/api/v1/tunnels", &op, mk("out", 7000))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"].as_str().unwrap().contains("outside"));
    let mut udp = mk("u", 0);
    udp["kind"] = json!("udp");
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &op, udp).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut no_reason = mk("n", 0);
    no_reason["reason"] = json!("");
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &op, no_reason)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut ghost = mk("g", 0);
    ghost["client_id"] = json!("nope");
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &op, ghost).await.0,
        StatusCode::NOT_FOUND
    );

    r.send(
        Method::POST,
        "/api/v1/tunnels",
        &op,
        json!({"client_id": cid2, "name": "other", "local_port": 80, "reason": "x"}),
    )
    .await;
    let (_, all) = r.get("/api/v1/tunnels", &op).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let (_, mine) = r
        .get(&format!("/api/v1/tunnels?client_id={cid}"), &op)
        .await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    let (st, one) = r.get(&format!("/api/v1/tunnels/{tid}"), &op).await;
    assert_eq!((st, one["name"].as_str()), (StatusCode::OK, Some("ssh")));
    assert_eq!(
        r.get("/api/v1/tunnels/nope", &op).await.0,
        StatusCode::NOT_FOUND
    );

    let path = format!("/api/v1/tunnels/{tid}");
    let (st, v) = r
        .send(
            Method::PATCH,
            &path,
            &op,
            json!({"local_port": 2222, "enabled": false, "reason": "move"}),
        )
        .await;
    assert_eq!(
        (st, v["local_port"].as_i64(), v["enabled"].as_bool()),
        (StatusCode::OK, Some(2222), Some(false))
    );
    assert_eq!(
        r.send(
            Method::PATCH,
            &path,
            &op,
            json!({"remote_port": 7000, "reason": "x"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(
            Method::PATCH,
            &path,
            &op,
            json!({"local_port": 0, "reason": "x"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(Method::PATCH, &path, &op, json!({"local_port": 1}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(
            Method::PATCH,
            "/api/v1/tunnels/nope",
            &op,
            json!({"reason": "x"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        r.req(Method::DELETE, &path, Some(&op), None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.req(
            Method::DELETE,
            &format!("{path}?reason=done"),
            Some(&op),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        r.req(
            Method::DELETE,
            &format!("{path}?reason=done"),
            Some(&op),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // 删除后端口释放。
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &op, mk("again", 6000))
            .await
            .0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn enroll_tokens() {
    let r = rig_with(Config {
        public_host: "frp.example.com".into(),
        public_url: "https://frp.example.com".into(),
        ..Config::default()
    })
    .await;
    let admin = r.admin("a", Role::Admin).await;

    let (st, v) = r
        .send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &admin,
            json!({"name":"office-pc","ttl_minutes":30,"reason":"onboarding"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let token = v["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("rfrp_"));
    let cmd = v["commands"]["linux_macos"].as_str().unwrap();
    assert!(
        cmd.contains(&token) && cmd.contains("frp.example.com:7000") && cmd.contains("office-pc")
    );
    assert!(v["commands"]["windows"].as_str().unwrap().contains(&token));
    assert!(v["commands"]["direct"]
        .as_str()
        .unwrap()
        .contains("--enroll-token"));
    let id = v["id"].as_str().unwrap().to_string();

    // 列表里没有明文，库里只有哈希。
    let (_, list) = r.get("/api/v1/enroll-tokens", &admin).await;
    assert!(!list.to_string().contains(&token));
    assert_eq!(list[0]["name"], "office-pc");
    let th = token_hash(&token);
    let n: i64 =
        r.db.call(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM enroll_tokens WHERE token_hash = ?1",
                [th],
                |r| r.get(0),
            )
            .unwrap())
        })
        .await
        .unwrap();
    assert_eq!(n, 1);

    for bad in [
        json!({"name":"x","ttl_minutes":0,"reason":"r"}),
        json!({"name":"x","ttl_minutes":99999,"reason":"r"}),
        json!({"name":"","reason":"r"}),
        json!({"name":"x"}),
    ] {
        assert_eq!(
            r.send(Method::POST, "/api/v1/enroll-tokens", &admin, bad)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    let path = format!("/api/v1/enroll-tokens/{id}");
    assert_eq!(
        r.req(Method::DELETE, &path, Some(&admin), None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.req(
            Method::DELETE,
            &format!("{path}?reason=oops"),
            Some(&admin),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        r.req(
            Method::DELETE,
            &format!("{path}?reason=oops"),
            Some(&admin),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        r.req(
            Method::DELETE,
            "/api/v1/enroll-tokens/nope?reason=x",
            Some(&admin),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let actions = r.audit_actions().await;
    assert!(
        actions.contains(&"enroll_token.create".to_string())
            && actions.contains(&"enroll_token.revoke".to_string())
    );
}

#[tokio::test]
async fn audit_listing_and_pagination() {
    let r = rig().await;
    let admin = r.admin("a", Role::Admin).await;
    let cid = r.client("c").await;
    for i in 0..3 {
        r.send(Method::POST, "/api/v1/tunnels", &admin, json!({"client_id": cid, "name": format!("t{i}"), "local_port": 1 + i, "reason": "seed"})).await;
    }
    let (st, page) = r.get("/api/v1/audit?limit=2", &admin).await;
    assert_eq!((st, page.as_array().unwrap().len()), (StatusCode::OK, 2));
    assert_eq!(page[0]["action"], "tunnel.create");
    assert_eq!(page[0]["actor"], "a");
    assert_eq!(page[0]["reason"], "seed");
    let last = page[1]["id"].as_i64().unwrap();
    let (_, next) = r
        .get(&format!("/api/v1/audit?limit=2&before={last}"), &admin)
        .await;
    assert!(next
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["id"].as_i64().unwrap() < last));
}

#[tokio::test]
async fn panel_static_files_spa_fallback_and_traversal() {
    let dir = std::env::temp_dir().join(format!("rustfrp-panel-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), "<html>panel</html>").unwrap();
    std::fs::write(dir.join("main.dart.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("assets/logo.svg"), "<svg/>").unwrap();
    std::fs::write(dir.join("secret.txt"), "top secret").unwrap();
    let r = rig_with(Config {
        panel_dir: Some(dir.join("assets").parent().unwrap().to_path_buf()),
        ..Config::default()
    })
    .await;

    let (st, h, v) = r.req(Method::GET, "/", None, None).await;
    assert_eq!(
        (st, v.as_str()),
        (StatusCode::OK, Some("<html>panel</html>"))
    );
    assert_eq!(h[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(h[header::CACHE_CONTROL], "no-cache");
    let (st, h, _) = r.req(Method::GET, "/main.dart.js", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(h[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    assert!(h[header::CACHE_CONTROL]
        .to_str()
        .unwrap()
        .contains("max-age"));
    assert_eq!(
        r.req(Method::GET, "/assets/logo.svg", None, None).await.1[header::CONTENT_TYPE],
        "image/svg+xml"
    );
    // 单页应用路由回落到 index.html；带扩展名的缺失文件与 /api 未知路径都是 404。
    assert_eq!(
        r.req(Method::GET, "/nodes/abc", None, None)
            .await
            .2
            .as_str(),
        Some("<html>panel</html>")
    );
    assert_eq!(
        r.req(Method::GET, "/missing.js", None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        r.req(Method::GET, "/api/v1/unknown", None, None).await.0,
        StatusCode::NOT_FOUND
    );
    // 路径穿越：既不能越出面板目录，也不能读到目录外的文件。
    for p in [
        "/../Cargo.toml",
        "/%2e%2e/secret.txt",
        "/assets/../secret.txt",
        "/a//b",
    ] {
        let (st, _, v) = r.req(Method::GET, p, None, None).await;
        assert!(
            st == StatusCode::NOT_FOUND || v.as_str() == Some("<html>panel</html>"),
            "{p}: {st}"
        );
        assert_ne!(v.as_str(), Some("top secret"), "{p}");
    }

    let no_panel = rig().await;
    assert_eq!(
        no_panel.req(Method::GET, "/", None, None).await.0,
        StatusCode::NOT_FOUND
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = r.hub.online_ids();
}
