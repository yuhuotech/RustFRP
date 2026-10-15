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
        metrics: Arc::default(),
        logs: Arc::default(),
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
    assert_eq!(v["api"], "v1");
    let sess = Sess {
        cookie: cookie.split(';').next().unwrap().to_string(),
        csrf,
    };
    let (st, v) = r.get("/api/v1/auth/me", &sess).await;
    assert_eq!(v["api"], "v1");
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
    // 进程指标与近 24 小时流量（只统计窗口内的分钟桶）。
    assert!(v["process"]["uptime_seconds"].is_u64());
    assert!(v["process"].get("rss_bytes").is_some());
    assert_eq!(v["traffic_24h"]["bytes_in"], 0);
    let now = store::now_ms();
    r.db.call(move |c| {
        for (minute, bin, bout) in [
            (now - 60_000, 100, 10),
            (now - 3 * 3_600_000, 5, 1),
            (now - 25 * 3_600_000, 1_000, 1_000),
        ] {
            c.execute(
                "INSERT INTO traffic_minutes VALUES ('t', ?1, ?2, ?3, 0, 0, 1, 0)",
                rusqlite::params![minute, bin, bout],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let (_, v) = r.get("/api/v1/overview", &s).await;
    assert_eq!(
        (
            v["traffic_24h"]["bytes_in"].as_i64(),
            v["traffic_24h"]["bytes_out"].as_i64()
        ),
        (Some(105), Some(11))
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
    udp["kind"] = json!("xtcp");
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
async fn managed_plugin_api_returns_only_secret_set_flags() {
    let r = rig_with(Config {
        allow_ports: vec![(6000, 6100)],
        ..Config::default()
    })
    .await;
    let operator = r.admin("plugin-operator", Role::Operator).await;
    let client_id = r.client("plugin-client").await;
    let secret = "never-include-this-password";
    let (status, response) = r
        .send(
            Method::POST,
            "/api/v1/tunnels",
            &operator,
            json!({
                "client_id": client_id.clone(),
                "name": "socks",
                "local_port": 1080,
                "remote_port": 6002,
                "plugin": {
                    "kind": "socks5",
                    "username": "user",
                    "password": secret
                },
                "reason": "configure local proxy"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response["plugin"]["kind"], "socks5");
    assert_eq!(response["plugin"]["password_set"], true);
    assert!(!response.to_string().contains(secret));

    let id = client_id.clone();
    let config =
        r.db.call(move |c| store::managed_config(c, &id))
            .await
            .unwrap();
    assert_eq!(config.proxies[0].plugin.as_ref().unwrap().password, secret);
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
    std::fs::write(dir.join("app.js"), "console.log(1)").unwrap();
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
    let (st, h, _) = r.req(Method::GET, "/app.js", None, None).await;
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
    let (status, _, body) = no_panel.req(Method::GET, "/", None, None).await;
    if cfg!(feature = "embedded-panel") {
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_str().unwrap_or("").contains("RustFRP"));
    } else {
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = r.hub.online_ids();
}

#[tokio::test]
async fn udp_and_web_tunnels_via_api() {
    let r = rig_with(Config {
        vhost_http: Some(8080),
        subdomain_host: "t.example.com".into(),
        ..Config::default()
    })
    .await;
    let op = r.admin("o", Role::Operator).await;
    let cid = r.client("c1").await;
    let (_, node) = r.get("/api/v1/node", &op).await;
    assert_eq!(node["vhost_http_port"], 8080);
    assert!(node["vhost_https_port"].is_null());
    assert_eq!(node["subdomain_host"], "t.example.com");

    let (st, udp) = r.send(Method::POST, "/api/v1/tunnels", &op, json!({"client_id": cid, "name": "dns", "kind": "udp", "local_port": 53, "remote_port": 5353, "reason": "x"})).await;
    assert_eq!(
        (st, udp["kind"].as_str()),
        (StatusCode::CREATED, Some("udp"))
    );

    let web = json!({"client_id": cid, "name": "site", "kind": "http", "local_port": 80, "custom_domains": ["WWW.Example.com"], "subdomain": "blog", "host_header_rewrite": "internal.lan", "reason": "x"});
    let (st, t) = r
        .send(Method::POST, "/api/v1/tunnels", &op, web.clone())
        .await;
    assert_eq!(st, StatusCode::CREATED, "{t}");
    assert_eq!(
        (
            t["custom_domains"][0].as_str(),
            t["subdomain"].as_str(),
            t["host_header_rewrite"].as_str()
        ),
        (Some("www.example.com"), Some("blog"), Some("internal.lan"))
    );
    let path = format!("/api/v1/tunnels/{}", t["id"].as_str().unwrap());
    let (st, up) = r
        .send(
            Method::PATCH,
            &path,
            &op,
            json!({"custom_domains": ["b.example.com"], "reason": "x"}),
        )
        .await;
    assert_eq!(
        (st, up["custom_domains"][0].as_str()),
        (StatusCode::OK, Some("b.example.com"))
    );
    // 同域名冲突 / 无域名 / https 未开启 / 子域名后缀未配置。
    let mut dup = web.clone();
    dup["name"] = json!("dup");
    dup["custom_domains"] = json!(["b.example.com"]);
    dup["subdomain"] = json!("");
    assert_eq!(
        r.send(Method::POST, "/api/v1/tunnels", &op, dup).await.0,
        StatusCode::CONFLICT
    );
    let mut https = web.clone();
    https["kind"] = json!("https");
    https["name"] = json!("tls");
    let (st, v) = r.send(Method::POST, "/api/v1/tunnels", &op, https).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("RUSTFRP_VHOST_HTTPS"));

    let none = rig().await;
    let op2 = none.admin("o", Role::Operator).await;
    let c2 = none.client("c").await;
    for (body, want) in [
        (
            json!({"client_id": c2, "name": "w", "kind": "http", "local_port": 80, "custom_domains": ["a.com"], "reason": "x"}),
            "RUSTFRP_VHOST_HTTP",
        ),
        (
            json!({"client_id": c2, "name": "s", "kind": "tcp", "local_port": 80, "subdomain": "x", "reason": "x"}),
            "RUSTFRP_SUBDOMAIN_HOST",
        ),
    ] {
        let (st, v) = none.send(Method::POST, "/api/v1/tunnels", &op2, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(
            v["error"]["message"].as_str().unwrap().contains(want),
            "{v}"
        );
    }
}

#[tokio::test]
async fn installers_available_without_login_or_panel() {
    let rig = rig().await;
    for (path, marker) in [
        ("/install.sh", "#!/bin/sh"),
        ("/install.ps1", "function Install-RustFrp"),
    ] {
        let response = rig
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with(marker));
        assert!(text.contains("SHA256"));
    }
}

#[tokio::test]
async fn traffic_history_requires_auth_and_bounds_queries() {
    use rustfrp_agent::metrics::Sample;
    use rustfrp_core::pipe::TrafficSnapshot;
    let r = rig().await;
    assert_eq!(
        r.req(Method::GET, "/api/v1/traffic", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let viewer = r.admin("traffic-viewer", Role::Viewer).await;
    assert_eq!(
        r.get("/api/v1/traffic?hours=23", &viewer).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.get("/api/v1/traffic?tunnel_id=missing", &viewer).await.0,
        StatusCode::NOT_FOUND
    );
    let client = r.client("traffic-device").await;
    let client_id = client.clone();
    let id =
        r.db.call(move |c| {
            Ok(store::create_tunnel(
                c,
                &store::NewTunnel {
                    client_id: client,
                    name: "traffic".into(),
                    kind: "tcp".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port: 80,
                    enabled: true,
                    ..Default::default()
                },
                store::now_ms(),
            )?
            .id)
        })
        .await
        .unwrap();
    assert_eq!(
        r.get("/api/v1/traffic?client_id=missing", &viewer).await.0,
        StatusCode::NOT_FOUND
    );
    let sample_id = id.clone();
    r.db.call(move |c| {
        rustfrp_server::traffic::persist(
            c,
            "api-test",
            store::now_ms(),
            &[Sample {
                name: sample_id,
                kind: "tcp".into(),
                active: true,
                generation: 0,
                traffic: TrafficSnapshot {
                    bytes_in: 19,
                    bytes_out: 23,
                    connections: 1,
                    connections_total: 1,
                },
            }],
        )
    })
    .await
    .unwrap();
    let (status, result) = r
        .get(&format!("/api/v1/traffic?tunnel_id={id}"), &viewer)
        .await;
    assert_eq!(status, StatusCode::OK);
    let filtered = r
        .get(
            &format!("/api/v1/traffic?client_id={client_id}&tunnel_id={id}"),
            &viewer,
        )
        .await;
    assert_eq!(filtered.1["items"], result["items"]);
    assert!(result["window_end"].as_i64().is_some());
    assert_eq!(result["range_hours"], 24);
    assert_eq!(result["items"][0]["bytes_in"], 19);
    assert_eq!(result["items"][0]["bytes_out"], 23);
    assert_eq!(result["items"][0]["gap"], true);
    assert_eq!(
        r.get("/api/v1/traffic?hours=168", &viewer).await.0,
        StatusCode::OK
    );
    assert_eq!(
        r.get("/api/v1/overview", &viewer).await.1["traffic"]["connections"],
        0
    );
}

#[tokio::test]
async fn batch_tunnels_are_authorized_bounded_and_atomic() {
    let r = rig().await;
    let operator = r.admin("batch-op", Role::Operator).await;
    let viewer = r.admin("batch-view", Role::Viewer).await;
    let client = r.client("batch-client").await;
    let ids =
        r.db.call(move |c| {
            let mut ids = vec![];
            for n in 0..2 {
                ids.push(
                    store::create_tunnel(
                        c,
                        &store::NewTunnel {
                            client_id: client.clone(),
                            name: format!("batch-{n}"),
                            kind: "tcp".into(),
                            local_ip: "localhost".into(),
                            local_port: 22,
                            enabled: true,
                            ..Default::default()
                        },
                        store::now_ms(),
                    )?
                    .id,
                );
            }
            Ok(ids)
        })
        .await
        .unwrap();
    let path = "/api/v1/tunnels/batch";
    assert_eq!(
        r.send(
            Method::POST,
            path,
            &viewer,
            json!({"ids":ids,"enabled":false,"reason":"test"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for list in [
        vec![],
        vec![ids[0].clone(); 2],
        vec!["x".into(); 101],
        vec!["x".repeat(65)],
        vec!["".into()],
    ] {
        assert_eq!(
            r.send(
                Method::POST,
                path,
                &operator,
                json!({"ids":list,"enabled":false,"reason":"test"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        r.send(
            Method::POST,
            path,
            &operator,
            json!({"ids":ids,"enabled":false})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(
            Method::POST,
            path,
            &operator,
            json!({"ids":[ids[0],"missing"],"enabled":false,"reason":"test"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let first = ids[0].clone();
    assert!(r
        .db
        .call(move |c| Ok(store::get_tunnel(c, &first)?.enabled))
        .await
        .unwrap());
    r.db.call(|c|{c.execute_batch("CREATE TRIGGER deny_batch BEFORE INSERT ON audit_log WHEN NEW.action='tunnel.batch' BEGIN SELECT RAISE(ABORT,'test'); END;")?;Ok(())}).await.unwrap();
    assert_ne!(
        r.send(
            Method::POST,
            path,
            &operator,
            json!({"ids":ids,"enabled":false,"reason":"test"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let first = ids[0].clone();
    assert!(r
        .db
        .call(move |c| Ok(store::get_tunnel(c, &first)?.enabled))
        .await
        .unwrap());
    r.db.call(|c| {
        c.execute_batch("DROP TRIGGER deny_batch")?;
        Ok(())
    })
    .await
    .unwrap();
    let result = r
        .send(
            Method::POST,
            path,
            &operator,
            json!({"ids":ids,"enabled":false,"reason":"batch off"}),
        )
        .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["updated"], 2);
    assert!(r
        .db
        .call(|c| Ok(store::list_tunnels(c, None)?.iter().all(|t| !t.enabled)))
        .await
        .unwrap());
    assert_eq!(
        r.audit_actions()
            .await
            .iter()
            .filter(|a| *a == "tunnel.batch")
            .count(),
        2
    );
}

#[tokio::test]
async fn automation_tokens_scopes_revocation_and_cookie_conflict() {
    let r = rig().await;
    let owner = r.admin("automation", Role::Owner).await;
    let viewer = r.admin("auto-view", Role::Viewer).await;
    let path = "/api/v1/api-tokens";
    assert_eq!(
        r.send(
            Method::POST,
            path,
            &viewer,
            json!({"name":"x","scope":"read","days":1,"reason":"test"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for (scope, days) in [("unknown", 1), ("read", 0), ("read", 366)] {
        assert_eq!(
            r.send(
                Method::POST,
                path,
                &owner,
                json!({"name":"x","scope":scope,"days":days,"reason":"test"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    async fn bearer(
        r: &Rig,
        method: Method,
        path: &str,
        token: &str,
        origin: Option<&str>,
        cookie: Option<&str>,
        body: Value,
    ) -> StatusCode {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "panel.test")
            .header(header::AUTHORIZATION, token)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        r.app
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
            .status()
    }
    let (status, result) = r
        .send(
            Method::POST,
            path,
            &owner,
            json!({"name":"CI","scope":"read","days":1,"reason":"test"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let token = result["token"].as_str().unwrap();
    let auth = format!("Bearer {token}");
    let id = result["info"]["id"].as_str().unwrap();
    assert_eq!(r.get(path, &owner).await.1[0]["name"], "CI");
    assert!(r.get(path, &owner).await.1[0].get("token_hash").is_none());
    assert_eq!(
        bearer(
            &r,
            Method::GET,
            "/api/v1/overview",
            &auth,
            None,
            None,
            json!({})
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        bearer(
            &r,
            Method::GET,
            "/api/v1/auth/me",
            &auth,
            None,
            None,
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bearer(
            &r,
            Method::GET,
            "/api/v1/api-tokens",
            &auth,
            None,
            None,
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bearer(
            &r,
            Method::POST,
            "/api/v1/tunnels/batch",
            &auth,
            None,
            None,
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bearer(
            &r,
            Method::GET,
            "/api/v1/overview",
            &auth,
            Some("https://evil.test"),
            None,
            json!({})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        bearer(
            &r,
            Method::GET,
            "/api/v1/overview",
            "Bearer invalid",
            None,
            Some(&owner.cookie),
            json!({})
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        r.send(
            Method::POST,
            &format!("{path}/{id}/revoke"),
            &owner,
            json!({"reason":"retire"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        bearer(
            &r,
            Method::GET,
            "/api/v1/overview",
            &auth,
            None,
            None,
            json!({})
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        r.send(
            Method::POST,
            &format!("{path}/missing/revoke"),
            &owner,
            json!({"reason":"retire"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let client = r.client("token-client").await;
    let id =
        r.db.call(move |c| {
            Ok(store::create_tunnel(
                c,
                &store::NewTunnel {
                    client_id: client,
                    name: "token".into(),
                    kind: "tcp".into(),
                    local_ip: "localhost".into(),
                    local_port: 22,
                    enabled: true,
                    ..Default::default()
                },
                store::now_ms(),
            )?
            .id)
        })
        .await
        .unwrap();
    for scope in ["tunnels", "admin"] {
        let result = r
            .send(
                Method::POST,
                path,
                &owner,
                json!({"name":scope,"scope":scope,"days":1,"reason":"test"}),
            )
            .await
            .1;
        let auth = format!("Bearer {}", result["token"].as_str().unwrap());
        assert_eq!(
            bearer(
                &r,
                Method::POST,
                "/api/v1/tunnels/batch",
                &auth,
                None,
                None,
                json!({"ids":[id],"enabled":false,"reason":"automation"})
            )
            .await,
            StatusCode::OK
        );
        if scope == "tunnels" {
            assert_eq!(
                bearer(
                    &r,
                    Method::POST,
                    "/api/v1/enroll-tokens",
                    &auth,
                    None,
                    None,
                    json!({})
                )
                .await,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            bearer(
                &r,
                Method::GET,
                "/api/v1/overview",
                &auth,
                Some("http://panel.test"),
                None,
                json!({})
            )
            .await,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn controller_logs_require_admin_and_remain_bounded() {
    let r = rig().await;
    let admin = r.admin("logs-admin", Role::Admin).await;
    let viewer = r.admin("logs-viewer", Role::Viewer).await;
    assert_eq!(
        r.get("/api/v1/logs", &viewer).await.0,
        StatusCode::FORBIDDEN
    );
    let result = r.get("/api/v1/logs", &admin).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["capacity"], 128);
    assert_eq!(result.1["items"], json!([]));
}

#[tokio::test]
async fn managed_config_transfer_requires_preview_digest_and_roundtrips_supported_tunnels() {
    let r = rig().await;
    let admin = r.admin("importer", Role::Owner).await;
    let client = r.client("import-client").await;
    let text="serverAddr='ignored'\nserverPort=7000\n[[proxies]]\nname='ssh'\ntype='tcp'\nlocalIP='127.0.0.1'\nlocalPort=22\nremotePort=6000\n";
    let preview = r
        .send(
            Method::POST,
            &format!("/api/v1/clients/{client}/config/preview"),
            &admin,
            json!({"text":text}),
        )
        .await;
    assert_eq!(preview.0, StatusCode::OK);
    assert_eq!(preview.1["changes"][0]["name"], "ssh");
    let digest = preview.1["digest"].as_str().unwrap();
    assert_eq!(
        r.send(
            Method::POST,
            &format!("/api/v1/clients/{client}/config/import"),
            &admin,
            json!({"text":text,"digest":"bad","accept_warnings":true,"reason":"import"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        r.send(
            Method::POST,
            &format!("/api/v1/clients/{client}/config/import"),
            &admin,
            json!({"text":text,"digest":digest,"accept_warnings":false,"reason":"import"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let imported = r
        .send(
            Method::POST,
            &format!("/api/v1/clients/{client}/config/import"),
            &admin,
            json!({"text":text,"digest":digest,"accept_warnings":true,"reason":"import"}),
        )
        .await;
    assert_eq!(imported.0, StatusCode::OK);
    assert_eq!(imported.1["updated"], 1);
    let export = r
        .get(&format!("/api/v1/clients/{client}/config/export"), &admin)
        .await;
    assert_eq!(export.0, StatusCode::OK);
    assert!(export.1["text"].as_str().unwrap().contains("serverAddr"));
    let bad = "[common]";
    assert_eq!(
        r.send(
            Method::POST,
            &format!("/api/v1/clients/{client}/config/preview"),
            &admin,
            json!({"text":bad})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let huge = "x".repeat(1024 * 1024 + 1);
    assert_eq!(
        r.send(
            Method::POST,
            &format!("/api/v1/clients/{client}/config/preview"),
            &admin,
            json!({"text":huge})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn admin_management_is_owner_only_audited_and_keeps_an_owner() {
    let r = rig().await;
    let owner = r.admin("boss", Role::Owner).await;
    let admin = r.admin("adm", Role::Admin).await;
    let new = |name: &str, role: &str, pw: &str, why: &str| json!({"name": name, "role": role, "password": pw, "reason": why});

    // 非 owner 不能创建；owner 需理由、强密码、合法角色。
    let body = new("ops", "operator", PASSWORD, "onboard");
    assert_eq!(
        r.send(Method::POST, "/api/v1/admins", &admin, body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        new("ops", "operator", PASSWORD, ""),
        new("ops", "operator", "short", "r"),
        new("", "operator", PASSWORD, "r"),
    ] {
        assert_eq!(
            r.send(Method::POST, "/api/v1/admins", &owner, bad).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        r.send(
            Method::POST,
            "/api/v1/admins",
            &owner,
            json!({"name":"x","role":"god","password":PASSWORD,"reason":"r"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (st, v) = r
        .send(Method::POST, "/api/v1/admins", &owner, body.clone())
        .await;
    assert_eq!(
        (st, v["role"].as_str()),
        (StatusCode::CREATED, Some("operator"))
    );
    assert!(!v.to_string().contains("password"));
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        r.send(Method::POST, "/api/v1/admins", &owner, body).await.0,
        StatusCode::CONFLICT
    );

    // 修改：空修改 / 缺理由 / 弱密码 / 未知 ID。
    let path = format!("/api/v1/admins/{id}");
    for (b, want) in [
        (json!({"reason":"r"}), StatusCode::BAD_REQUEST),
        (json!({"role":"admin"}), StatusCode::BAD_REQUEST),
        (
            json!({"password":"short","reason":"r"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(r.send(Method::PATCH, &path, &owner, b).await.0, want);
    }
    assert_eq!(
        r.send(
            Method::PATCH,
            "/api/v1/admins/missing",
            &owner,
            json!({"role":"admin","reason":"r"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        r.send(
            Method::PATCH,
            &path,
            &admin,
            json!({"role":"admin","reason":"r"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (st, v) = r
        .send(
            Method::PATCH,
            &path,
            &owner,
            json!({"role":"viewer","disabled":true,"password":"another long pass","reason":"offboard"}),
        )
        .await;
    assert_eq!((st, v["role"].as_str()), (StatusCode::OK, Some("viewer")));
    assert!(v["disabled_at"].is_i64());

    // 不能改自己的角色 / 状态；最后一个 owner 受保护（经第二个 owner 验证）。
    let own =
        r.db.call(|c| store::list_admins(c))
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.name == "boss")
            .unwrap();
    assert_eq!(
        r.send(
            Method::PATCH,
            &format!("/api/v1/admins/{}", own.id),
            &owner,
            json!({"disabled":true,"reason":"r"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // 改自己的密码是允许的（只有角色 / 状态不行）。
    let (st, _) = r
        .send(
            Method::PATCH,
            &format!("/api/v1/admins/{}", own.id),
            &owner,
            json!({"password":"a brand new password","reason":"rotate"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let second = r.admin("boss2", Role::Owner).await;
    let (st, v) = r
        .send(
            Method::PATCH,
            &format!("/api/v1/admins/{}", own.id),
            &second,
            json!({"role":"admin","reason":"handover"}),
        )
        .await;
    assert_eq!((st, v["role"].as_str()), (StatusCode::OK, Some("admin")));
    let boss2 =
        r.db.call(|c| store::list_admins(c))
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.name == "boss2")
            .unwrap();
    // boss2 现在是唯一的 owner：另一位 owner 不存在，无法再降级（自改已被拒绝，改用新建 owner 路径验证 409）。
    assert_eq!(boss2.role, Role::Owner);
    let acts = r.audit_actions().await;
    assert_eq!(acts.iter().filter(|a| *a == "admin.create").count(), 1);
    assert_eq!(acts.iter().filter(|a| *a == "admin.update").count(), 3);
}

#[tokio::test]
async fn reserved_ports_and_tunnel_quota_are_enforced_atomically() {
    let r = rig_with(Config {
        reserved_ports: vec![(6010, 6010)],
        max_tunnels_per_client: 2,
        ..Config::default()
    })
    .await;
    let s = r.admin("o", Role::Operator).await;
    let cid = r.client("c1").await;
    let make = |name: &str, remote: i64| json!({"client_id": cid, "name": name, "local_port": 22, "remote_port": remote, "reason": "t"});
    let (st, v) = r
        .send(Method::POST, "/api/v1/tunnels", &s, make("a", 6010))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"].as_str().unwrap().contains("reserved"));
    for (n, p) in [("a", 6011), ("b", 6012)] {
        assert_eq!(
            r.send(Method::POST, "/api/v1/tunnels", &s, make(n, p))
                .await
                .0,
            StatusCode::CREATED
        );
    }
    let (st, v) = r
        .send(Method::POST, "/api/v1/tunnels", &s, make("c", 6013))
        .await;
    assert_eq!((st, err_code(&v)), (StatusCode::CONFLICT, "conflict"));
    // 超限的创建整体回滚：没有多出的隧道，也没有「成功」审计。
    let (_, list) = r.get("/api/v1/tunnels", &s).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let creates = r
        .audit_actions()
        .await
        .iter()
        .filter(|a| *a == "tunnel.create")
        .count();
    assert_eq!(creates, 2);
}

#[tokio::test]
async fn http_options_via_rest_hide_the_password_and_exports_need_admin() {
    let r = rig_with(Config {
        vhost_http: Some(8080),
        ..Config::default()
    })
    .await;
    let op = r.admin("o", Role::Operator).await;
    let admin = r.admin("a", Role::Admin).await;
    let cid = r.client("c1").await;
    let (st, v) = r
        .send(
            Method::POST,
            "/api/v1/tunnels",
            &op,
            json!({
                "client_id": cid, "name": "web", "kind": "http", "local_port": 80,
                "custom_domains": ["a.example.com"],
                "locations": ["/api"], "http_user": "alice", "http_password": "s3cret",
                "route_by_http_user": "", "request_headers": {"X-Env": "prod"},
                "reason": "t"
            }),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert!(!v.to_string().contains("s3cret"), "{v}");
    assert_eq!(
        (
            v["http_user"].as_str(),
            v["http_password_set"].as_bool(),
            v["locations"][0].as_str()
        ),
        (Some("alice"), Some(true), Some("/api"))
    );
    let id = v["id"].as_str().unwrap().to_string();
    // 列表与详情同样不含密码。
    let (_, list) = r.get("/api/v1/tunnels", &op).await;
    assert!(!list.to_string().contains("s3cret"));
    // 不带 http_password 的修改保持原密码；非法选项被拒绝。
    let (st, v) = r
        .send(
            Method::PATCH,
            &format!("/api/v1/tunnels/{id}"),
            &op,
            json!({"http_user":"bob","reason":"t"}),
        )
        .await;
    assert_eq!(
        (st, v["http_password_set"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (st, _) = r
        .send(
            Method::PATCH,
            &format!("/api/v1/tunnels/{id}"),
            &op,
            json!({"locations":["bad"],"reason":"t"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // 导出含 Basic 认证密码，因此需要管理员。
    let path = format!("/api/v1/clients/{cid}/config/export");
    assert_eq!(r.get(&path, &op).await.0, StatusCode::FORBIDDEN);
    let (st, v) = r.get(&path, &admin).await;
    assert_eq!(st, StatusCode::OK);
    let text = v["text"].as_str().unwrap();
    assert!(
        text.contains("httpPassword = \"s3cret\"") && text.contains("httpUser = \"bob\""),
        "{text}"
    );
}

#[tokio::test]
async fn nodes_tokens_and_lifecycle_via_rest() {
    let r = rig_with(Config {
        public_host: "ctl.example.com".into(),
        node_addr: "0.0.0.0:7200".into(),
        edge_tls_fingerprint: "a".repeat(64),
        ..Config::default()
    })
    .await;
    let viewer = r.admin("v", Role::Viewer).await;
    let admin = r.admin("a", Role::Admin).await;

    // 内置节点总在列表第一位；外置节点为空时只有它。
    let (st, v) = r.get("/api/v1/nodes", &viewer).await;
    assert_eq!((st, v.as_array().unwrap().len()), (StatusCode::OK, 1));
    assert_eq!(
        (
            v[0]["builtin"].as_bool(),
            v[0]["id"].as_str(),
            v[0]["online"].as_bool()
        ),
        (Some(true), Some(""), Some(true))
    );

    // 接入新节点：只有 Admin；命令里带节点端口与指纹，令牌明文只出现一次。
    let body = json!({"name": "edge-1", "kind": "node", "reason": "add node"});
    assert_eq!(
        r.send(Method::POST, "/api/v1/enroll-tokens", &viewer, body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (st, v) = r
        .send(Method::POST, "/api/v1/enroll-tokens", &admin, body)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["kind"], "node");
    let direct = v["commands"]["direct"].as_str().unwrap();
    assert!(
        direct.contains("--mode edge") && direct.contains("ctl.example.com:7200"),
        "{direct}"
    );
    assert!(
        direct.contains(&"a".repeat(64)) && direct.contains("--public-host"),
        "{direct}"
    );
    let (_, list) = r.get("/api/v1/enroll-tokens", &admin).await;
    assert!(!list.to_string().contains(v["token"].as_str().unwrap()));
    assert_eq!(list[0]["kind"], "node");
    // 节点端口未启用时拒绝；未知 kind 拒绝。
    let off = rig().await;
    let a2 = off.admin("a", Role::Admin).await;
    let (st, v) = off
        .send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &a2,
            json!({"name":"x","kind":"node","reason":"r"}),
        )
        .await;
    assert_eq!(
        (
            st,
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("RUSTFRP_NODE_ADDR")
        ),
        (StatusCode::BAD_REQUEST, true)
    );
    for bad in [
        json!({"name":"x","kind":"weird","reason":"r"}),
        json!({"name":"x","node_id":"missing","reason":"r"}),
    ] {
        let (st, _) = r
            .send(Method::POST, "/api/v1/enroll-tokens", &admin, bad)
            .await;
        assert!(
            st == StatusCode::BAD_REQUEST || st == StatusCode::NOT_FOUND,
            "{st}"
        );
    }

    // 外置节点（直接入库模拟已接入）：列表、设备令牌指向该节点、改名 / 停用 / 删除保护。
    let info = rustfrp_core::node::NodeInfo {
        public_host: "edge.example.com".into(),
        edge_port: 7000,
        tls_fingerprint: "b".repeat(64),
        ..Default::default()
    };
    let key = "cd".repeat(32);
    r.db.call(move |c| store::create_node(c, "n1", "edge-1", &key, &info, 1))
        .await
        .unwrap();
    let (_, v) = r.get("/api/v1/nodes", &viewer).await;
    assert_eq!(
        (
            v.as_array().unwrap().len(),
            v[1]["id"].as_str(),
            v[1]["online"].as_bool()
        ),
        (2, Some("n1"), Some(false))
    );
    let (st, v) = r
        .send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &admin,
            json!({"name":"laptop","node_id":"n1","reason":"r"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let direct = v["commands"]["direct"].as_str().unwrap();
    assert!(
        direct.contains("edge.example.com:7000") && direct.contains(&"b".repeat(64)),
        "{direct}"
    );
    // 还没上报过对外主机名的节点不能生成设备命令。
    r.db.call(|c| {
        store::create_node(c, "n2", "bare", &"ef".repeat(32), &Default::default(), 1)?;
        Ok(())
    })
    .await
    .unwrap();
    let (st, _) = r
        .send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &admin,
            json!({"name":"x","node_id":"n2","reason":"r"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    assert_eq!(
        r.send(
            Method::PATCH,
            "/api/v1/nodes/n1",
            &viewer,
            json!({"name":"x","reason":"r"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        json!({"reason":"r"}),
        json!({"name":"x"}),
        json!({"name":"","reason":"r"}),
    ] {
        assert_eq!(
            r.send(Method::PATCH, "/api/v1/nodes/n1", &admin, bad)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        r.send(
            Method::PATCH,
            "/api/v1/nodes/nope",
            &admin,
            json!({"name":"x","reason":"r"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (st, v) = r
        .send(
            Method::PATCH,
            "/api/v1/nodes/n1",
            &admin,
            json!({"name":"renamed","disabled":true,"reason":"maintenance"}),
        )
        .await;
    assert_eq!(
        (st, v["name"].as_str(), v["disabled_at"].is_i64()),
        (StatusCode::OK, Some("renamed"), true)
    );
    // 仍有设备时不能删。
    let cid = r.client("c1").await;
    let c2 = cid.clone();
    r.db.call(move |c| store::set_client_node(c, &c2, "n1"))
        .await
        .unwrap();
    let (st, v) = r
        .send(
            Method::DELETE,
            "/api/v1/nodes/n1?reason=bye",
            &admin,
            json!({}),
        )
        .await;
    assert_eq!((st, err_code(&v)), (StatusCode::CONFLICT, "conflict"));
    r.db.call(move |c| store::delete_client(c, &cid, 5))
        .await
        .unwrap();
    assert_eq!(
        r.send(
            Method::DELETE,
            "/api/v1/nodes/n1?reason=bye",
            &admin,
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        r.send(
            Method::DELETE,
            "/api/v1/nodes/n1?reason=bye",
            &admin,
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let acts = r.audit_actions().await;
    for a in ["node_token.create", "node.update", "node.delete"] {
        assert!(acts.iter().any(|x| x == a), "{a}: {acts:?}");
    }
}

#[tokio::test]
async fn client_tags_and_remote_restart_are_validated_and_audited() {
    let r = rig().await;
    let admin = r.admin("a", Role::Admin).await;
    let op = r.admin("o", Role::Operator).await;
    let cid = r.client("c1").await;
    let path = format!("/api/v1/clients/{cid}");
    // 标签：去重 / 校验；Operator 无权修改。
    let (st, v) = r
        .send(
            Method::PATCH,
            &path,
            &admin,
            json!({"tags": ["prod", " web ", "prod"], "reason": "label"}),
        )
        .await;
    assert_eq!(
        (st, v["tags"].as_array().unwrap().len()),
        (StatusCode::OK, 2)
    );
    let (_, v) = r.get(&path, &op).await;
    assert_eq!(v["tags"][1], "web");
    assert_eq!(
        r.send(
            Method::PATCH,
            &path,
            &op,
            json!({"tags": ["x"], "reason": "r"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        json!({"tags": [""], "reason": "r"}),
        json!({"tags": (0..9).map(|i| format!("t{i}")).collect::<Vec<_>>(), "reason": "r"}),
    ] {
        assert_eq!(
            r.send(Method::PATCH, &path, &admin, bad).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    // 远程重启：只对在线设备有意义，需要理由与 Admin。
    let restart = format!("{path}/restart");
    assert_eq!(
        r.send(Method::POST, &restart, &op, json!({"reason": "r"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (st, v) = r
        .send(Method::POST, &restart, &admin, json!({"reason": "r"}))
        .await;
    assert_eq!(
        (st, err_code(&v)),
        (StatusCode::CONFLICT, "conflict"),
        "offline device"
    );
    assert_eq!(
        r.send(Method::POST, &restart, &admin, json!({"reason": ""}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // 让设备「在线」（经 Hub 的节点上报路径），再请求重启。
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let (c2, node) = (cid.clone(), "n1".to_string());
    r.db.call(move |c| {
        let info = rustfrp_core::node::NodeInfo::default();
        store::create_node(c, &node, "edge", &"ab".repeat(32), &info, 1)?;
        store::set_client_node(c, &c2, "n1")
    })
    .await
    .unwrap();
    r.hub.node_attach("n1", tx);
    r.hub.node_client_state("n1", &cid, true, "").await;
    let (st, v) = r
        .send(Method::POST, &restart, &admin, json!({"reason": "upgrade"}))
        .await;
    assert_eq!((st, v["ok"].as_bool()), (StatusCode::OK, Some(true)));
    let seq =
        r.db.call({
            let cid = cid.clone();
            move |c| Ok(store::managed_config(c, &cid)?.restart_seq)
        })
        .await
        .unwrap();
    assert_eq!(seq, 1);
    assert!(r
        .audit_actions()
        .await
        .iter()
        .any(|a| a == "client.restart"));
}

#[tokio::test]
async fn client_connection_limits_signed_updates_and_live_node_migration_work_together() {
    let r = rig().await;
    let admin = r.admin("owner", Role::Admin).await;
    let client_id = r.client("move-me").await;
    let path = format!("/api/v1/clients/{client_id}");
    let (status, body) = r
        .send(
            Method::PATCH,
            &path,
            &admin,
            json!({"max_connections": 8, "reason": "limit"}),
        )
        .await;
    assert_eq!(
        (status, body["max_connections"].as_u64()),
        (StatusCode::OK, Some(8))
    );

    let (old_tx, mut old_rx) = tokio::sync::mpsc::channel(8);
    let (new_tx, mut new_rx) = tokio::sync::mpsc::channel(8);
    let client_for_db = client_id.clone();
    r.db.call(move |c| {
        for (id, host) in [("n1", "old.edge.test"), ("n2", "new.edge.test")] {
            store::create_node(
                c,
                id,
                id,
                &"ab".repeat(32),
                &rustfrp_core::node::NodeInfo {
                    public_host: host.into(),
                    edge_port: 7443,
                    tls_fingerprint: "cd".repeat(32),
                    ..Default::default()
                },
                store::now_ms(),
            )?;
        }
        let tunnel = store::create_tunnel(
            c,
            &store::NewTunnel {
                client_id: client_for_db.clone(),
                name: "ssh".into(),
                kind: "tcp".into(),
                local_ip: "127.0.0.1".into(),
                local_port: 22,
                remote_port: 0,
                enabled: true,
                ..Default::default()
            },
            store::now_ms(),
        )?;
        store::set_client_node(c, &client_for_db, "n1")?;
        Ok(tunnel.id)
    })
    .await
    .unwrap();
    r.hub.node_attach("n1", old_tx);
    r.hub.node_attach("n2", new_tx);
    r.hub.node_client_state("n1", &client_id, true, "").await;

    let (status, body) = r
        .send(
            Method::POST,
            &format!("{path}/update"),
            &admin,
            json!({"version": "v1.2.3", "reason": "security update"}),
        )
        .await;
    assert_eq!(
        (status, body["version"].as_str()),
        (StatusCode::OK, Some("v1.2.3"))
    );
    let update_client = client_id.clone();
    let pending =
        r.db.call(move |c| store::managed_config(c, &update_client))
            .await
            .unwrap();
    assert_eq!(
        (
            pending.max_connections,
            pending.update_seq,
            pending.update_version.as_str()
        ),
        (8, 1, "v1.2.3")
    );

    let (status, body) = r
        .send(
            Method::POST,
            &format!("{path}/migrate"),
            &admin,
            json!({"node_id": "n2", "reason": "closer edge"}),
        )
        .await;
    assert_eq!(
        (status, body["node_id"].as_str()),
        (StatusCode::OK, Some("n2"))
    );
    let mut redirected = false;
    while let Ok(message) = old_rx.try_recv() {
        if matches!(message, rustfrp_core::node::CtlMsg::RedirectClient { client_id: moved, target } if moved == client_id && target.address == "new.edge.test")
        {
            redirected = true;
        }
    }
    assert!(redirected);
    assert!(
        matches!(new_rx.try_recv(), Ok(rustfrp_core::node::CtlMsg::Device(device)) if device.id == client_id && device.config.max_connections == 8)
    );
    let old_snapshot =
        r.db.call(|c| store::list_node_devices(c, "n1"))
            .await
            .unwrap();
    assert_eq!(old_snapshot.len(), 1);
    assert!(old_snapshot[0].config.proxies.is_empty());
    assert_eq!(
        old_snapshot[0]
            .config
            .redirect
            .as_ref()
            .map(|d| d.address.as_str()),
        Some("new.edge.test")
    );
    assert!(r
        .audit_actions()
        .await
        .iter()
        .any(|action| action == "client.move_node"));
}

#[tokio::test]
async fn prometheus_metrics_are_opt_in_and_authenticated() {
    // 默认不提供。
    let off = rig().await;
    let (st, _, _) = off.req(Method::GET, "/metrics", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // 配置了令牌：缺少 / 错误的令牌拒绝，正确的返回文本格式。
    let on = rig_with(Config {
        metrics_token: "0123456789abcdef0123".into(),
        ..Config::default()
    })
    .await;
    let scrape = |auth: Option<&'static str>| {
        let app = on.app.clone();
        async move {
            let mut b = Request::builder().method(Method::GET).uri("/metrics");
            if let Some(a) = auth {
                b = b.header(header::AUTHORIZATION, a);
            }
            let res = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
            let (st, h) = (res.status(), res.headers().clone());
            let body = res.into_body().collect().await.unwrap().to_bytes();
            (st, h, String::from_utf8_lossy(&body).into_owned())
        }
    };
    for bad in [
        None,
        Some("Bearer wrong-token-0123456789"),
        Some("Basic 0123456789abcdef0123"),
    ] {
        let (st, h, _) = scrape(bad).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{bad:?}");
        assert!(h.contains_key(header::WWW_AUTHENTICATE));
    }
    on.client("c1").await;
    let (st, h, body) = scrape(Some("Bearer 0123456789abcdef0123")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(h[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/plain; version=0.0.4"));
    for want in [
        "rustfrp_clients 1\n",
        "rustfrp_clients_online 0\n",
        "# TYPE rustfrp_builtin_bytes_in_total counter\n",
        "rustfrp_nodes_online 0\n",
        "rustfrp_process_uptime_seconds ",
    ] {
        assert!(body.contains(want), "{want}\n{body}");
    }
}

#[tokio::test]
async fn a_disabled_builtin_node_is_hidden_and_refuses_builtin_tokens() {
    let r = rig_with(Config {
        builtin_edge: false,
        node_addr: "0.0.0.0:7001".into(),
        ..Config::default()
    })
    .await;
    let admin = r.admin("a", Role::Admin).await;
    let (_, v) = r.get("/api/v1/nodes", &admin).await;
    assert!(v.as_array().unwrap().is_empty(), "no builtin entry: {v}");
    let (_, v) = r.get("/api/v1/node", &admin).await;
    assert_eq!(v["enabled"], false);
    let (st, v) = r
        .send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &admin,
            json!({"name":"x","reason":"r"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("built-in node is disabled"));
    // 节点令牌不受影响。
    let (st, _) = r
        .send(
            Method::POST,
            "/api/v1/enroll-tokens",
            &admin,
            json!({"name":"x","kind":"node","reason":"r"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
}

#[tokio::test]
async fn visitors_target_stcp_tunnels_and_follow_their_lifecycle() {
    let r = rig().await;
    let op = r.admin("vis-op", Role::Operator).await;
    let viewer = r.admin("vis-viewer", Role::Viewer).await;
    let provider = r.client("provider").await;
    let consumer = r.client("consumer").await;
    // 私密隧道没有公网端口，也不会通过 API 泄露密钥。
    let (st, stcp) = r
        .send(
            Method::POST,
            "/api/v1/tunnels",
            &op,
            json!({"client_id": provider, "name": "ssh", "kind": "stcp", "local_port": 22, "reason": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{stcp}");
    assert!(!stcp.to_string().contains("secret"), "{stcp}");
    let tid = stcp["id"].as_str().unwrap().to_string();
    for (body, want) in [
        (
            json!({"client_id": provider, "name": "p", "kind": "stcp", "local_port": 22, "remote_port": 6000, "reason": "x"}),
            "no public port",
        ),
        (
            json!({"client_id": provider, "name": "g", "kind": "sudp", "local_port": 22, "lb_group": "g", "reason": "x"}),
            "do not apply",
        ),
    ] {
        let (st, v) = r.send(Method::POST, "/api/v1/tunnels", &op, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(
            v["error"]["message"].as_str().unwrap().contains(want),
            "{v}"
        );
    }
    let plain = r
        .send(
            Method::POST,
            "/api/v1/tunnels",
            &op,
            json!({"client_id": provider, "name": "plain", "local_port": 1, "reason": "x"}),
        )
        .await
        .1;

    // 创建访问者：只能指向 stcp / sudp 隧道。
    let body = json!({"client_id": consumer, "name": "to-ssh", "tunnel_id": tid, "bind_port": 2222, "reason": "x"});
    let (st, v) = r
        .send(Method::POST, "/api/v1/visitors", &op, body.clone())
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(
        (v["bind_addr"].as_str(), v["bind_port"].as_u64()),
        (Some("127.0.0.1"), Some(2222))
    );
    let vid = v["id"].as_str().unwrap().to_string();
    for (patch, want, code) in [
        (
            json!({"tunnel_id": plain["id"], "name": "x", "reason": "x"}),
            "stcp or sudp",
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"tunnel_id": "nope", "name": "x", "reason": "x"}),
            "does not exist",
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "dup-port", "tunnel_id": tid, "bind_port": 2222, "reason": "x"}),
            "already uses",
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "to-ssh", "tunnel_id": tid, "reason": "x"}),
            "already uses",
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "bad", "tunnel_id": tid, "bind_port": 70000, "reason": "x"}),
            "bind_port",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut b = body.clone();
        for (k, val) in patch.as_object().unwrap() {
            b[k] = val.clone();
        }
        let (st, v) = r.send(Method::POST, "/api/v1/visitors", &op, b).await;
        assert_eq!(st, code, "{v}");
        assert!(
            v["error"]["message"].as_str().unwrap().contains(want),
            "{v}"
        );
    }
    // 列表；Viewer 可读不可写。
    let (_, list) = r
        .get(&format!("/api/v1/visitors?client_id={consumer}"), &viewer)
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (st, _) = r
        .send(Method::POST, "/api/v1/visitors", &viewer, body.clone())
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 下发给访问者所在设备的配置带上目标隧道 ID 与密钥；提供方拿到同一个密钥。
    let (c1, p1) = (consumer.clone(), provider.clone());
    let (cfg, provider_cfg) =
        r.db.call(move |c| {
            Ok((
                store::managed_config(c, &c1)?,
                store::managed_config(c, &p1)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(cfg.visitors.len(), 1);
    let spec = &cfg.visitors[0];
    assert_eq!(
        (
            spec.server_name.as_str(),
            spec.kind.as_str(),
            spec.bind_port
        ),
        (tid.as_str(), "stcp", 2222)
    );
    assert_eq!(spec.sk.len(), 32);
    let proxy = provider_cfg.proxies.iter().find(|p| p.name == tid).unwrap();
    assert_eq!(proxy.secret.sk, spec.sk);

    // 修改与停用；目标隧道停用后访问者不再下发；删除隧道会连带移除访问者。
    let path = format!("/api/v1/visitors/{vid}");
    let (st, v) = r
        .send(
            Method::PATCH,
            &path,
            &op,
            json!({"bind_port": 0, "bind_addr": "0.0.0.0", "reason": "x"}),
        )
        .await;
    assert_eq!(
        (st, v["bind_addr"].as_str()),
        (StatusCode::OK, Some("0.0.0.0"))
    );
    let tpath = format!("/api/v1/tunnels/{tid}");
    r.send(
        Method::PATCH,
        &tpath,
        &op,
        json!({"enabled": false, "reason": "x"}),
    )
    .await;
    let c2 = consumer.clone();
    let cfg =
        r.db.call(move |c| store::managed_config(c, &c2))
            .await
            .unwrap();
    assert!(cfg.visitors.is_empty());
    r.send(
        Method::PATCH,
        &tpath,
        &op,
        json!({"enabled": true, "reason": "x"}),
    )
    .await;
    let (st, _) = r
        .send(
            Method::PATCH,
            &path,
            &op,
            json!({"enabled": false, "reason": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = r
        .send(
            Method::PATCH,
            &path,
            &op,
            json!({"enabled": true, "reason": "x"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        r.req(
            Method::DELETE,
            &format!("{tpath}?reason=bye"),
            Some(&op),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, list) = r.get("/api/v1/visitors", &viewer).await;
    assert!(list.as_array().unwrap().is_empty(), "{list}");
    // 单独删除访问者、不存在的访问者。
    let (st, v2) = r.send(Method::POST, "/api/v1/tunnels", &op, json!({"client_id": provider, "name": "s2", "kind": "sudp", "local_port": 53, "reason": "x"})).await;
    assert_eq!(st, StatusCode::CREATED, "{v2}");
    let (_, v3) = r
        .send(
            Method::POST,
            "/api/v1/visitors",
            &op,
            json!({"client_id": consumer, "name": "again", "tunnel_id": v2["id"], "reason": "x"}),
        )
        .await;
    let del = format!("/api/v1/visitors/{}?reason=bye", v3["id"].as_str().unwrap());
    assert_eq!(
        r.req(Method::DELETE, &del, Some(&op), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        r.req(Method::DELETE, &del, Some(&op), None).await.0,
        StatusCode::NOT_FOUND
    );
    let actions = r.audit_actions().await;
    for a in ["visitor.create", "visitor.update", "visitor.delete"] {
        assert!(actions.iter().any(|x| x == a), "{a}: {actions:?}");
    }
}
