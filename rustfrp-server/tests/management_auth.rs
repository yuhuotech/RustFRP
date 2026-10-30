//! Single configuration credential, file loading, separation and session rotation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use rustfrp_server::{
    api::{router, AppState, LoginLimiter},
    auth::token_hash,
    config::Config,
    db::Db,
    hub::Hub,
    store,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rustfrp-management-{}", store::new_id()))
}
async fn app(db: &Db, token: &str) -> Router {
    let digest = token_hash(token);
    db.call(move |c| store::bind_management_token(c, &digest, store::now_ms()))
        .await
        .unwrap();
    let (hub, _) = Hub::new(db.clone());
    router(AppState {
        db: db.clone(),
        hub,
        cfg: Arc::new(Config {
            admin_token: token.into(),
            ..Config::default()
        }),
        limiter: Arc::new(LoginLimiter::new(5, 600000)),
        metrics: Arc::default(),
        logs: Arc::default(),
    })
}
async fn login(app: &Router, body: Value) -> (StatusCode, String, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header(header::HOST, "localhost")
                .header(header::ORIGIN, "http://localhost")
                .header("x-rustfrp-admin", "1")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .unwrap_or_default();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn me(app: &Router, cookie: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn management_token_replaces_passwords_and_rotation_invalidates_old_sessions_only() {
    let db = Db::open_memory().unwrap();
    let a = app(&db, "test-management-first").await;
    assert_eq!(
        login(&a, json!({"token":"wrong"})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&a, json!({"name":"admin","password":"admin123456"}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, cookie, body) = login(&a, json!({"token":"test-management-first"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["admin"]["role"], "owner");
    assert!(!body.to_string().contains("test-management-first"));
    assert_eq!(me(&a, &cookie).await, StatusCode::OK);
    let unchanged = app(&db, "test-management-first").await;
    assert_eq!(me(&unchanged, &cookie).await, StatusCode::OK);
    let device = db
        .call(|c| {
            store::create_client(
                c,
                "device-test",
                "device",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "linux",
                "arm64",
                "test",
                1,
            )
        })
        .await
        .unwrap();
    let b = app(&db, "test-management-second").await;
    assert_eq!(me(&b, &cookie).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&b, json!({"token":"test-management-first"})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&b, json!({"token":"test-management-second"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        db.call(move |c| store::get_client(c, &device.id))
            .await
            .unwrap()
            .name,
        "device"
    );
    let count = db
        .call(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM admins WHERE id='__management__'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn device_enrollment_credentials_cannot_log_into_management() {
    let db = Db::open_memory().unwrap();
    let a = app(&db, "management-only").await;
    let device_token = rustfrp_server::auth::new_token().unwrap();
    let digest = token_hash(&device_token);
    db.call(move |c| store::create_enroll_token(c, &digest, "client", "test", 1, 60000))
        .await
        .unwrap();
    assert_eq!(
        login(&a, json!({"token":device_token})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&a, json!({"token":"management-only"})).await.0,
        StatusCode::OK
    );
}

#[test]
fn config_file_overrides_are_explicit_bounded_redacted_and_do_not_accept_placeholder() {
    let file = path();
    std::fs::write(&file, "[admin]\ntoken = 'file-management-token'\n").unwrap();
    let load = |override_token: Option<&str>| {
        Config::from_lookup(|key| match key {
            "RUSTFRP_CONFIG" => Some(file.to_string_lossy().into_owned()),
            "RUSTFRP_ADMIN_TOKEN" => override_token.map(str::to_string),
            _ => None,
        })
    };
    let cfg = load(None).unwrap();
    assert_eq!(cfg.admin_token, "file-management-token");
    cfg.validate_admin().unwrap();
    assert!(!format!("{cfg:?}").contains("file-management-token"));
    assert_eq!(load(Some("env-token")).unwrap().admin_token, "env-token");
    for invalid in ["", "change-me", "has whitespace"] {
        assert!(load(Some(invalid)).unwrap().validate_admin().is_err());
    }
    assert!(load(Some(&"x".repeat(1025)))
        .unwrap()
        .validate_admin()
        .is_err());
    std::fs::write(&file, "[admin]\ntoken = 'private-never-print'\nextra = 1\n").unwrap();
    let error = load(None).unwrap_err();
    assert!(!error.contains("private-never-print"));
    std::fs::write(&file, vec![b' '; 65537]).unwrap();
    assert!(load(None).unwrap_err().contains("64 KiB"));
    std::fs::write(&file, [0xff]).unwrap();
    assert!(load(None).is_err());
    std::fs::remove_file(&file).unwrap();
    assert!(load(None).is_err());
    assert!(matches!(
        rustfrp_server::cli::parse(&["run".into(), "-c".into(), "test.toml".into()]),
        Ok(rustfrp_server::cli::Cmd::RunConfig(_))
    ));
}

#[tokio::test]
async fn placeholder_stops_before_creating_database_or_listeners() {
    let file = path();
    let error = rustfrp_server::app::start(
        Config {
            db_path: file.clone(),
            admin_token: "change-me".into(),
            ..Config::default()
        },
        rustfrp_agent::log::Logger::silent(),
    )
    .await
    .err()
    .unwrap();
    assert!(error.contains("[admin].token"));
    assert!(!file.exists());
}
