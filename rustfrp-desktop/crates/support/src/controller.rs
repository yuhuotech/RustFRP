//! Controller transport: operation allowlist, origin-bound private cookie jar,
//! bounded bodies and no cross-origin redirect of authenticated requests.

use crate::Error;
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

const CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../rustfrp-protocol/UI_BRIDGE.json"
));
const MAX_REQUEST: usize = 2 * 1024 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestArgs {
    pub id: Option<String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    pub body: Option<Value>,
}

#[derive(Deserialize)]
struct Contract {
    operations: BTreeMap<String, (String, String)>,
}
struct Session {
    url: Url,
    http: Client,
    csrf: String,
    generation: u64,
}
pub struct Selection {
    url: Url,
    http: Client,
}
impl Selection {
    pub fn address(&self) -> String {
        self.url.to_string()
    }
}

pub struct ControllerClient {
    session: Mutex<Option<Session>>,
    permits: Semaphore,
    flights: std::sync::Mutex<BTreeMap<String, CancellationToken>>,
    early_cancels: std::sync::Mutex<BTreeMap<String, Instant>>,
}

struct Flight<'a> {
    owner: &'a ControllerClient,
    id: String,
}
impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if let Ok(mut flights) = self.owner.flights.lock() {
            flights.remove(&self.id);
        }
    }
}

impl Default for ControllerClient {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            permits: Semaphore::new(8),
            flights: std::sync::Mutex::new(BTreeMap::new()),
            early_cancels: std::sync::Mutex::new(BTreeMap::new()),
        }
    }
}

/// The controller address is explicitly selected by a trusted local page.
pub fn controller_url(value: &str) -> Result<Url, Error> {
    if value.len() > 2048 {
        return Err(Error::invalid("Controller 地址过长"));
    }
    let mut url = Url::parse(value).map_err(Error::invalid_context("Controller 地址无效"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid(
            "请输入不含凭据、查询参数和片段的 HTTP(S) Controller 地址",
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn endpoint(base: &Url, operation: &str, args: &RequestArgs) -> Result<(Method, Url, bool), Error> {
    let contract: Contract =
        serde_json::from_str(CONTRACT).map_err(Error::invalid_context("桌面接口契约无效"))?;
    let (method, path) = contract
        .operations
        .get(operation)
        .ok_or_else(|| Error::invalid("不允许的 Controller 操作"))?;
    let method =
        Method::from_bytes(method.as_bytes()).map_err(Error::invalid_context("无效请求方法"))?;
    let write = method != Method::GET;
    let mut url = base.clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(Error::invalid_context("Controller 地址不能构造 API 路径"))?;
        segments.pop_if_empty().push("api").push("v1");
        for segment in path.trim_start_matches('/').split('/') {
            if segment == "{id}" {
                let id = args
                    .id
                    .as_deref()
                    .filter(|id| {
                        !id.is_empty()
                            && id.len() <= 256
                            && *id != "."
                            && *id != ".."
                            && !id.chars().any(char::is_control)
                    })
                    .ok_or_else(|| Error::invalid("资源标识无效"))?;
                segments.push(id);
            } else {
                segments.push(segment);
            }
        }
    }
    if args.query.len() > 8
        || args
            .query
            .iter()
            .any(|(key, value)| key.len() > 64 || value.len() > 1024)
    {
        return Err(Error::invalid("请求参数超过限制"));
    }
    if !args.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&args.query);
    }
    if let Some(body) = &args.body {
        if !body.is_object()
            || serde_json::to_vec(body)
                .map_err(Error::invalid_context("请求正文无效"))?
                .len()
                > MAX_REQUEST
        {
            return Err(Error::invalid("请求正文超过限制或不是对象"));
        }
    }
    if write && !matches!(operation, "login" | "logout" | "previewConfig") {
        let reason = args
            .body
            .as_ref()
            .and_then(|body| body.get("reason"))
            .and_then(Value::as_str)
            .or_else(|| args.query.get("reason").map(String::as_str));
        if reason.is_none_or(|reason| reason.trim().is_empty() || reason.len() > 200) {
            return Err(Error::invalid("写操作必须提供有效的审计理由"));
        }
    }
    Ok((method, url, write))
}

impl ControllerClient {
    pub fn cancel(&self, id: &str) -> Result<bool, Error> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::invalid("无效请求标识"));
        }
        let flights = self
            .flights
            .lock()
            .map_err(Error::io_context("请求取消状态不可用"))?;
        if let Some(cancel) = flights.get(id) {
            cancel.cancel();
            return Ok(true);
        }
        let mut early = self
            .early_cancels
            .lock()
            .map_err(Error::io_context("请求取消状态不可用"))?;
        early.retain(|_, time| time.elapsed() < Duration::from_secs(2));
        if early.len() < 32 {
            early.insert(id.to_string(), Instant::now());
        }
        Ok(false)
    }

    pub async fn request_tracked(
        &self,
        id: String,
        operation: &str,
        args: RequestArgs,
    ) -> Result<Value, Error> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::invalid("无效请求标识"));
        }
        let cancel = CancellationToken::new();
        {
            let mut flights = self
                .flights
                .lock()
                .map_err(Error::io_context("请求状态不可用"))?;
            if flights.len() >= 8 || flights.contains_key(&id) {
                return Err(Error::io("Controller 请求繁忙，请稍后重试"));
            }
            let early = self
                .early_cancels
                .lock()
                .map_err(Error::io_context("请求取消状态不可用"))?
                .remove(&id);
            if early.is_some_and(|time| time.elapsed() < Duration::from_secs(2)) {
                return Err(Error {
                    status: 0,
                    code: "cancelled".into(),
                    message: "请求已取消".into(),
                    uncertain: false,
                });
            }
            flights.insert(id.clone(), cancel.clone());
        }
        let _flight = Flight { owner: self, id };
        let result = tokio::select! {
            result = self.request(operation, args) => result,
            () = cancel.cancelled() => Err(Error { status:0, code:"cancelled".into(), message:"请求已取消".into(), uncertain: !matches!(operation, "me" | "overview" | "traffic" | "node" | "nodes" | "clients" | "client" | "tunnels" | "tunnel" | "visitors" | "enrollTokens" | "apiTokens" | "admins" | "audit" | "logs" | "exportConfig" | "login" | "logout" | "previewConfig") }),
        };
        result
    }

    pub fn prepare(address: &str) -> Result<Selection, Error> {
        let url = controller_url(address)?;
        let http = Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(Duration::from_secs(15))
            .build()
            .map_err(Error::io_context("无法初始化 Controller 安全传输"))?;
        Ok(Selection { url, http })
    }
    pub async fn select(&self, address: &str) -> Result<String, Error> {
        Ok(self.commit(Self::prepare(address)?).await)
    }
    pub async fn commit(&self, selection: Selection) -> String {
        let Selection { url, http } = selection;
        let mut session = self.session.lock().await;
        if session.as_ref().is_some_and(|session| session.url == url) {
            return url.to_string();
        }
        let generation = session.as_ref().map_or(1, |session| session.generation + 1);
        let address = url.as_str().to_string();
        *session = Some(Session {
            url,
            http,
            csrf: String::new(),
            generation,
        });
        address
    }
    pub fn busy(&self) -> bool {
        self.flights
            .try_lock()
            .map_or(true, |flights| !flights.is_empty())
    }
    pub async fn address(&self) -> Option<String> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(|session| session.url.to_string())
    }

    pub async fn request(&self, operation: &str, args: RequestArgs) -> Result<Value, Error> {
        let _permit = self
            .permits
            .try_acquire()
            .map_err(Error::io_context("Controller 请求繁忙，请稍后重试"))?;
        let (http, method, url, csrf, generation, write) = {
            let session = self.session.lock().await;
            let current = session
                .as_ref()
                .ok_or_else(|| Error::invalid("请先选择 Controller"))?;
            let (method, url, write) = endpoint(&current.url, operation, &args)?;
            (
                current.http.clone(),
                method,
                url,
                current.csrf.clone(),
                current.generation,
                write,
            )
        };
        let mut request = http
            .request(method, url)
            .header("accept", "application/json");
        if write {
            request = request
                .header("content-type", "application/json")
                .header("x-csrf-token", csrf);
        }
        if operation == "login" {
            request = request.header("x-rustfrp-admin", "1");
        }
        if let Some(body) = args.body {
            request = request.json(&body);
        }
        let uncertain = write && !matches!(operation, "login" | "logout" | "previewConfig");
        let network = || Error {
            status: 0,
            code: "network".into(),
            message: if uncertain {
                "操作结果未知，请刷新核对实际状态"
            } else {
                "无法连接 Controller，请检查地址、网络和证书"
            }
            .into(),
            uncertain,
        };
        let mut response = request.send().await.map_err(|_| network())?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE as u64)
        {
            return Err(Error::invalid("Controller 响应超过大小限制"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| network())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                return Err(Error::invalid("Controller 响应超过大小限制"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| Error {
                status: status.as_u16(),
                code: "invalid_response".into(),
                message: "Controller 或代理返回了无效 JSON".into(),
                uncertain: uncertain && status.is_server_error(),
            })?
        };
        let mut session = self.session.lock().await;
        let current = session
            .as_mut()
            .filter(|session| session.generation == generation)
            .ok_or_else(|| Error::invalid("Controller 已切换，请重新读取"))?;
        if status.as_u16() == 401 || operation == "logout" {
            current.csrf.clear();
        }
        if !status.is_success() {
            return Err(Error {
                status: status.as_u16(),
                code: value
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("http_error")
                    .into(),
                message: value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Controller 请求失败")
                    .into(),
                uncertain: false,
            });
        }
        if matches!(operation, "login" | "me") {
            current.csrf = value
                .get("csrf")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(object) = value.as_object_mut() {
                object.remove("csrf");
            }
        }
        Ok(value)
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn actual_http_errors_and_body_bounds_do_not_leak_credentials_or_replay_mutations() {
        let client = ControllerClient::default();
        assert!(client.request("me", RequestArgs::default()).await.is_err());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        client
            .select(&format!("http://127.0.0.1:{port}"))
            .await
            .unwrap();
        let error = client
            .request(
                "patchNode",
                RequestArgs {
                    id: Some("edge".into()),
                    body: Some(json!({"reason":"fixture"})),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.uncertain);
        assert_eq!(error.code, "network");
        assert!(
            !client
                .request(
                    "previewConfig",
                    RequestArgs {
                        id: Some("device".into()),
                        body: Some(json!({"text":"config"})),
                        ..Default::default()
                    }
                )
                .await
                .unwrap_err()
                .uncertain
        );
        for (reply, code, uncertain) in [
            (
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into(),
                "",
                false,
            ),
            (
                "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 4\r\nConnection: close\r\n\r\nhtml"
                    .into(),
                "invalid_response",
                true,
            ),
            (
                "HTTP/1.1 400 Bad Request\r\nContent-Length: 4\r\nConnection: close\r\n\r\nhtml"
                    .into(),
                "invalid_response",
                false,
            ),
            (
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into(),
                "http_error",
                false,
            ),
            (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    MAX_RESPONSE + 1
                ),
                "invalid_request",
                false,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 4096];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                let _ = stream.write_all(reply.as_bytes()).await;
            });
            client
                .select(&format!("http://127.0.0.1:{port}"))
                .await
                .unwrap();
            let result = client
                .request(
                    "patchNode",
                    RequestArgs {
                        id: Some("edge".into()),
                        body: Some(json!({"reason":"fixture"})),
                        ..Default::default()
                    },
                )
                .await;
            if code.is_empty() {
                assert_eq!(result.unwrap(), Value::Null);
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, code);
                assert_eq!(error.uncertain, uncertain);
            }
            server.await.unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            let body = vec![b'x'; MAX_RESPONSE + 1];
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
        });
        client
            .select(&format!("http://127.0.0.1:{port}"))
            .await
            .unwrap();
        let error = client
            .request(
                "patchNode",
                RequestArgs {
                    id: Some("edge".into()),
                    body: Some(json!({"reason":"fixture"})),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_request");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn tracked_requests_cancel_before_dispatch_and_bound_concurrent_and_early_cancellation_state(
    ) {
        use std::sync::Arc;
        let client = Arc::new(ControllerClient::default());
        for id in ["", "invalid/id"] {
            assert!(client.cancel(id).is_err());
            assert!(client
                .request_tracked(id.into(), "me", RequestArgs::default())
                .await
                .is_err());
        }
        let long_id = "x".repeat(65);
        assert!(client.cancel(&long_id).is_err());
        assert!(client
            .request_tracked(long_id, "me", RequestArgs::default())
            .await
            .is_err());
        for index in 0..40 {
            assert!(!client.cancel(&format!("early-{index}")).unwrap());
        }
        assert_eq!(client.early_cancels.lock().unwrap().len(), 32);
        assert_eq!(
            client
                .request_tracked("early-0".into(), "me", RequestArgs::default())
                .await
                .unwrap_err()
                .code,
            "cancelled"
        );
        client
            .early_cancels
            .lock()
            .unwrap()
            .insert("old".into(), Instant::now() - Duration::from_secs(3));
        assert_ne!(
            client
                .request_tracked("old".into(), "me", RequestArgs::default())
                .await
                .unwrap_err()
                .code,
            "cancelled"
        );
        let permits = client.permits.acquire_many(8).await.unwrap();
        assert!(client.request("me", RequestArgs::default()).await.is_err());
        drop(permits);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        client
            .select(&format!("http://127.0.0.1:{port}"))
            .await
            .unwrap();
        let waiting = Arc::new(tokio::sync::Notify::new());
        let notice = waiting.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(stream.read(&mut bytes).await.unwrap() > 0);
            notice.notify_one();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let active = client.clone();
        let running = tokio::spawn(async move {
            active
                .request_tracked(
                    "active".into(),
                    "previewConfig",
                    RequestArgs {
                        id: Some("device".into()),
                        body: Some(json!({"text":"config"})),
                        ..Default::default()
                    },
                )
                .await
        });
        waiting.notified().await;
        assert!(client.busy());
        assert!(client
            .request_tracked("active".into(), "me", RequestArgs::default())
            .await
            .is_err());
        assert!(client.cancel("active").unwrap());
        let error = running.await.unwrap().unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert!(!error.uncertain);
        assert!(!client.busy());
        server.abort();
        {
            let mut flights = client.flights.lock().unwrap();
            for index in 0..8 {
                flights.insert(format!("fixture-{index}"), CancellationToken::new());
            }
        }
        assert!(client
            .request_tracked("over-limit".into(), "me", RequestArgs::default())
            .await
            .is_err());
        client.flights.lock().unwrap().clear();
    }

    #[test]
    fn address_and_operation_boundary_reject_credentials_and_arbitrary_requests() {
        assert!(controller_url(&"http://example.com/".repeat(300)).is_err());
        for address in [
            "file:///etc/passwd",
            "https://u:p@example.com",
            "https://user@example.com",
            "https://:password@example.com",
            "https://example.com/?token=x",
            "https://example.com/#x",
            "garbage",
        ] {
            assert!(controller_url(address).is_err(), "{address}");
        }
        let base = controller_url("https://example.com/panel").unwrap();
        assert!(endpoint(&base, "rawHttp", &RequestArgs::default()).is_err());
        assert!(endpoint(&base, "client", &RequestArgs::default()).is_err());
        let args = RequestArgs {
            id: Some("a/b?x".into()),
            ..Default::default()
        };
        let (_, url, write) = endpoint(&base, "client", &args).unwrap();
        assert_eq!(url.path(), "/panel/api/v1/clients/a%2Fb%3Fx");
        assert!(!write);
        let mut query = BTreeMap::new();
        query.insert("page".into(), "2".into());
        let (_, url, _) = endpoint(
            &base,
            "clients",
            &RequestArgs {
                query,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(url.query(), Some("page=2"));
        let mut too_many = BTreeMap::new();
        for index in 0..9 {
            too_many.insert(format!("p{index}"), "1".into());
        }
        assert!(endpoint(
            &base,
            "clients",
            &RequestArgs {
                query: too_many,
                ..Default::default()
            },
        )
        .is_err());
        assert!(endpoint(
            &base,
            "createTunnel",
            &RequestArgs {
                body: Some(json!({"reason":""})),
                ..Default::default()
            }
        )
        .is_err());
        assert!(endpoint(
            &base,
            "createTunnel",
            &RequestArgs {
                body: Some(json!({"reason":"发布服务"})),
                ..Default::default()
            }
        )
        .is_ok());
        assert!(endpoint(
            &base,
            "login",
            &RequestArgs {
                body: Some(json!([])),
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn endpoint_rejects_unbounded_identifiers_queries_and_non_object_bodies() {
        let base = controller_url("https://controller.example/panel").unwrap();
        for id in ["", ".", "..", "line\nbreak"] {
            assert!(endpoint(
                &base,
                "client",
                &RequestArgs {
                    id: Some(id.into()),
                    ..Default::default()
                }
            )
            .is_err());
        }
        assert!(endpoint(
            &base,
            "client",
            &RequestArgs {
                id: Some("x".repeat(257)),
                ..Default::default()
            }
        )
        .is_err());
        let mut long_key = BTreeMap::new();
        long_key.insert("k".repeat(65), "value".into());
        assert!(endpoint(
            &base,
            "clients",
            &RequestArgs {
                query: long_key,
                ..Default::default()
            }
        )
        .is_err());
        let mut long_value = BTreeMap::new();
        long_value.insert("page".into(), "v".repeat(1025));
        assert!(endpoint(
            &base,
            "clients",
            &RequestArgs {
                query: long_value,
                ..Default::default()
            }
        )
        .is_err());
        assert!(endpoint(
            &base,
            "createTunnel",
            &RequestArgs {
                body: Some(json!({"payload":"x".repeat(MAX_REQUEST)})),
                ..Default::default()
            }
        )
        .is_err());
        assert!(endpoint(
            &base,
            "createTunnel",
            &RequestArgs {
                query: BTreeMap::from([("reason".into(), "audited from query".into())]),
                ..Default::default()
            }
        )
        .is_ok());
    }

    #[test]
    fn dropping_a_flight_tolerates_a_poisoned_registry() {
        use std::sync::Arc;
        let client = Arc::new(ControllerClient::default());
        let poisoned = client.clone();
        assert!(std::thread::spawn(move || {
            let _guard = poisoned.flights.lock().unwrap();
            panic!("poison request registry");
        })
        .join()
        .is_err());
        drop(Flight {
            owner: &client,
            id: "finished".into(),
        });
    }

    #[tokio::test]
    async fn invalid_error_json_is_not_uncertain_and_non_object_me_does_not_mutate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (status, body) in [("200 OK", "[]"), ("400 Bad Request", "not json")] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let client = ControllerClient::default();
        client.select(&format!("http://{address}")).await.unwrap();
        assert_eq!(
            client.request("me", RequestArgs::default()).await.unwrap(),
            json!([])
        );
        let error = client
            .request(
                "patchNode",
                RequestArgs {
                    id: Some("node".into()),
                    body: Some(json!({"reason":"test"})),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_response");
        assert!(!error.uncertain);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sessions_hide_credentials_preserve_csrf_and_surface_auth_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in [
                (
                    "200 OK",
                    json!({"csrf":"private-csrf","admin":{"id":"a","role":"owner"}}),
                ),
                ("200 OK", json!({"ok":true})),
                (
                    "401 Unauthorized",
                    json!({"error":{"code":"unauthorized","message":"expired"}}),
                ),
                ("200 OK", json!({"ok":true})),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = vec![0; 8192];
                let n = socket.read(&mut data).await.unwrap();
                requests.push(String::from_utf8_lossy(&data[..n]).to_string());
                let text = body.to_string();
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: rustfrp_session=private-cookie; Path=/; HttpOnly\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        let client = ControllerClient::default();
        assert!(client.request("me", RequestArgs::default()).await.is_err());
        client.select(&format!("http://{address}")).await.unwrap();
        let auth = client
            .request(
                "login",
                RequestArgs {
                    body: Some(json!({"token":"test-management-token"})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(auth.get("csrf").is_none());
        assert_eq!(auth["admin"]["id"], "a");
        // Recreating the WebView selects the same Controller and retains the native session.
        client.select(&format!("http://{address}/")).await.unwrap();
        client
            .request(
                "patchClient",
                RequestArgs {
                    id: Some("id".into()),
                    body: Some(json!({"reason":"测试"})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let error = client
            .request("me", RequestArgs::default())
            .await
            .unwrap_err();
        assert_eq!(error.status, 401);
        assert_eq!(error.code, "unauthorized");
        client
            .request("logout", RequestArgs::default())
            .await
            .unwrap();
        assert!(client
            .session
            .lock()
            .await
            .as_ref()
            .unwrap()
            .csrf
            .is_empty());
        let requests = server.await.unwrap();
        assert!(requests[0].contains("x-rustfrp-admin: 1"));
        assert!(requests[1].contains("x-csrf-token: private-csrf"));
        assert!(requests[1].contains("cookie: rustfrp_session=private-cookie"));
        assert!(!requests[1].contains("origin:"));
        assert_eq!(requests.len(), 4);
    }

    #[tokio::test]
    async fn response_from_a_controller_replaced_mid_request_is_discarded() {
        use std::sync::Arc;
        let client = Arc::new(ControllerClient::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        client.select(&format!("http://{address}")).await.unwrap();
        let received = Arc::new(tokio::sync::Notify::new());
        let notice = received.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            notice.notify_one();
            tokio::time::sleep(Duration::from_millis(20)).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}")
                .await
                .unwrap();
        });
        let running = {
            let client = client.clone();
            tokio::spawn(async move { client.request("me", RequestArgs::default()).await })
        };
        received.notified().await;
        client.select("http://127.0.0.1:1").await.unwrap();
        let error = running.await.unwrap().unwrap_err();
        assert!(error.message.contains("已切换"));
        server.await.unwrap();
    }
}
