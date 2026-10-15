//! Signed application updates. The WebView can select a source, never a key, URL or executable.
use crate::{
    install,
    release::{self, Release},
    Error,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::Mutex as Gate};
use tokio_util::sync::CancellationToken;

const MAX_PACKAGE: u64 = 1024 * 1024 * 1024;
#[derive(Clone)]
pub struct Config {
    pub repository: String,
    pub key: String,
    pub mirrors: Vec<String>,
}
impl Config {
    pub fn compiled() -> Self {
        Self {
            repository: option_env!("RUSTFRP_RELEASE_REPOSITORY")
                .unwrap_or("")
                .into(),
            key: option_env!("RUSTFRP_RELEASE_PUBLIC_KEY_HEX")
                .unwrap_or("")
                .into(),
            mirrors: option_env!("RUSTFRP_UPDATE_MIRRORS")
                .unwrap_or("https://gh-proxy.com,https://ghfast.top")
                .split(',')
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().trim_end_matches('/').into())
                .collect(),
        }
    }
    fn validate(&self) -> Result<(), Error> {
        release::repository(&self.repository)?;
        release::hex(&self.key, 32)?;
        if self.mirrors.len() > 8 {
            return Err(Error::invalid("更新镜像数量超过限制"));
        }
        for mirror in &self.mirrors {
            https(mirror)?;
        }
        Ok(())
    }
    fn targets(&self, source: &str, url: &str) -> Result<Vec<String>, Error> {
        https(url)?;
        match source {
            "github" => Ok(vec![url.into()]),
            "mainland" if !self.mirrors.is_empty() => self
                .mirrors
                .iter()
                .map(|mirror| {
                    let target = format!("{}/{url}", mirror.trim_end_matches('/'));
                    https(&target)?;
                    Ok(target)
                })
                .collect(),
            _ => Err(Error::invalid("更新源无效或未配置镜像")),
        }
    }
}
fn https(value: &str) -> Result<reqwest::Url, Error> {
    let url = reqwest::Url::parse(value).map_err(Error::invalid_context("更新地址无效"))?;
    if value.len() > 4096
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid("更新仅允许不含凭据的 HTTPS 地址"));
    }
    Ok(url)
}
fn cancelled() -> Error {
    Error {
        status: 0,
        code: "update_cancelled".into(),
        message: "更新已取消".into(),
        uncertain: false,
    }
}
fn live(token: &CancellationToken) -> Result<(), Error> {
    if token.is_cancelled() {
        Err(cancelled())
    } else {
        Ok(())
    }
}
pub trait Source {
    fn bytes(
        &self,
        url: &str,
        limit: usize,
        token: &CancellationToken,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, Error>> + Send;
    fn file(
        &self,
        url: &str,
        path: &Path,
        size: u64,
        token: &CancellationToken,
        progress: &Updates,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}
struct Http {
    client: reqwest::Client,
}
impl Http {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("RustFRP native updater")
                .https_only(true)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(300))
                .pool_max_idle_per_host(1)
                .pool_idle_timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 5 || https(attempt.url().as_str()).is_err() {
                        attempt.stop()
                    } else {
                        attempt.follow()
                    }
                }))
                .build()
                .map_err(Error::io_context("无法创建更新连接"))?,
        })
    }
    async fn response(
        &self,
        url: &str,
        token: &CancellationToken,
    ) -> Result<reqwest::Response, Error> {
        https(url)?;
        let response = tokio::select! { _=token.cancelled()=>return Err(cancelled()),result=self.client.get(url).send()=>result.map_err(Error::io_context("更新源连接失败"))? };
        if !response.status().is_success() {
            return Err(Error::io(format!(
                "更新源返回 HTTP {}",
                response.status().as_u16()
            )));
        }
        Ok(response)
    }
}
impl Source for Http {
    async fn bytes(
        &self,
        url: &str,
        limit: usize,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, Error> {
        let mut response = self.response(url, token).await?;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(Error::invalid("更新响应超过大小限制"));
        }
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {_=token.cancelled()=>return Err(cancelled()),result=response.chunk()=>result.map_err(Error::io_context("读取更新响应失败"))?};
            let Some(chunk) = chunk else { return Ok(bytes) };
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(Error::invalid("更新响应超过大小限制"));
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    async fn file(
        &self,
        url: &str,
        path: &Path,
        size: u64,
        token: &CancellationToken,
        progress: &Updates,
    ) -> Result<(), Error> {
        let mut response = self.response(url, token).await?;
        if size == 0
            || size > MAX_PACKAGE
            || response
                .content_length()
                .is_some_and(|length| length != size)
        {
            return Err(Error::invalid("更新包大小与发布记录不符"));
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut output = options
            .open(path)
            .await
            .map_err(Error::io_context("无法创建私有更新包"))?;
        let mut count = 0u64;
        loop {
            let chunk = tokio::select! {_=token.cancelled()=>return Err(cancelled()),result=response.chunk()=>result.map_err(Error::io_context("下载更新包失败"))?};
            let Some(chunk) = chunk else { break };
            count = count.saturating_add(chunk.len() as u64);
            if count > size {
                return Err(Error::invalid("更新包超过发布大小"));
            }
            output
                .write_all(&chunk)
                .await
                .map_err(Error::io_context("保存更新包失败"))?;
            progress.progress(count, size);
        }
        if count != size {
            return Err(Error::invalid("更新包下载不完整"));
        }
        output
            .sync_all()
            .await
            .map_err(Error::io_context("更新包写入失败"))?;
        live(token)
    }
}
pub trait Installer {
    fn apply(
        &self,
        plan: &install::Plan,
        path: &Path,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}
struct NativeInstaller;
impl Installer for NativeInstaller {
    async fn apply(&self, plan: &install::Plan, path: &Path) -> Result<(), Error> {
        let plan = plan.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || install::handoff(&plan, &path))
            .await
            .map_err(Error::io_context("安装任务失败"))?
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub phase: String,
    pub latest: Option<Release>,
    pub available: bool,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
    pub source: String,
    pub mirror: Option<String>,
    pub disabled: Option<String>,
    pub manual: Option<String>,
}
pub struct Updates {
    config: Config,
    current: String,
    root: PathBuf,
    executable: PathBuf,
    state: Mutex<Status>,
    gate: Gate<()>,
    cancel: Mutex<CancellationToken>,
    plan: Mutex<Option<install::Plan>>,
}
struct Operation<'a>(&'a Updates);
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.0.change(|state| {
            if matches!(state.phase.as_str(), "checking" | "downloading") {
                state.phase = "error".into();
                state.error = Some("更新任务已中断，请重新检查更新".into());
            }
        });
    }
}
impl Updates {
    pub fn new(config: Config, current: &str, root: PathBuf, executable: PathBuf) -> Self {
        let disabled = config
            .validate()
            .and_then(|()| release::version(current).map(|_| ()))
            .err()
            .map(|error| error.message);
        Self {
            config,
            current: current.into(),
            root,
            executable,
            state: Mutex::new(Status {
                phase: "idle".into(),
                latest: None,
                available: false,
                downloaded: 0,
                total: 0,
                error: None,
                source: "github".into(),
                mirror: None,
                disabled,
                manual: None,
            }),
            gate: Gate::new(()),
            cancel: Mutex::new(CancellationToken::new()),
            plan: Mutex::new(None),
        }
    }
    pub fn status(&self) -> Status {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
    pub fn busy(&self) -> bool {
        self.gate.try_lock().is_err() || self.status().phase == "installing"
    }
    fn change(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()));
    }
    fn progress(&self, count: u64, total: u64) {
        self.change(|state| {
            state.downloaded = count;
            state.total = total;
        });
    }
    fn begin(&self, phase: &str) -> Result<CancellationToken, Error> {
        if let Some(reason) = self.status().disabled {
            return Err(Error::invalid(reason));
        }
        let token = CancellationToken::new();
        *self
            .cancel
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = token.clone();
        self.change(|state| {
            state.phase = phase.into();
            state.error = None;
            state.downloaded = 0;
            state.total = 0;
        });
        Ok(token)
    }
    fn finish(&self, result: &Result<(), Error>) {
        self.change(|state| {
            if let Err(error) = result {
                state.phase = if error.code == "update_cancelled" {
                    "cancelled"
                } else {
                    "error"
                }
                .into();
                state.error = Some(error.message.clone());
            } else if state.phase != "installing" {
                state.phase = "idle".into();
            }
        });
    }
    pub fn cancel(&self) -> Result<Status, Error> {
        let token = self
            .cancel
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.status().phase == "installing" {
            return Err(Error::invalid("安装程序已接续，不能取消"));
        }
        token.cancel();
        Ok(self.status())
    }
    async fn get(
        &self,
        source: &impl Source,
        url: &str,
        limit: usize,
        token: &CancellationToken,
    ) -> Result<Vec<u8>, Error> {
        let mut failure = Error::io("没有可用更新源");
        for target in self.config.targets(&self.status().source, url)? {
            live(token)?;
            match source.bytes(&target, limit, token).await {
                Ok(bytes) if bytes.len() <= limit => {
                    self.change(|state| {
                        state.mirror = Some(
                            https(&target)
                                .map(|url| url.origin().ascii_serialization())
                                .unwrap_or_default(),
                        )
                    });
                    return Ok(bytes);
                }
                Ok(_) => return Err(Error::invalid("更新响应超过大小限制")),
                Err(error) if error.code == "update_cancelled" => return Err(error),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }
    pub async fn check(
        &self,
        choice: &str,
        source: &impl Source,
        planner: impl FnOnce(&[String]) -> install::Plan,
    ) -> Result<Status, Error> {
        let _gate = self
            .gate
            .try_lock()
            .map_err(Error::invalid_context("更新操作正在进行"))?;
        if self.status().phase == "installing" {
            return Err(Error::invalid("安装程序已接续"));
        }
        self.config.targets(choice, "https://api.github.com")?;
        let token = self.begin("checking")?;
        let _operation = Operation(self);
        self.change(|state| {
            state.source = choice.into();
            state.latest = None;
            state.available = false;
            state.manual = None;
            state.mirror = None;
        });
        *self
            .plan
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
        let result = async {
            let bytes = self
                .get(
                    source,
                    &format!(
                        "https://api.github.com/repos/{}/releases?per_page=100",
                        self.config.repository
                    ),
                    2 * 1024 * 1024,
                    &token,
                )
                .await?;
            let latest = release::newest(&bytes, &self.config.repository)?;
            live(&token)?;
            let available = latest.as_ref().is_some_and(|latest| {
                release::version(&latest.version).is_ok_and(|version| {
                    release::version(&self.current).is_ok_and(|current| version > current)
                })
            });
            let plan = latest.as_ref().filter(|_| available).map(|latest| {
                planner(
                    &latest
                        .assets
                        .iter()
                        .map(|asset| asset.name.clone())
                        .collect::<Vec<_>>(),
                )
            });
            self.change(|state| {
                state.latest = latest;
                state.available = available;
                state.manual = plan.as_ref().and_then(|plan| plan.manual.clone());
            });
            *self
                .plan
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = plan;
            Ok(())
        }
        .await;
        self.finish(&result);
        result?;
        Ok(self.status())
    }
    pub async fn check_native(&self, choice: &str) -> Result<Status, Error> {
        self.check(choice, &Http::new()?, |assets| {
            install::current(&self.executable, assets)
        })
        .await
    }
    /// Skip the background check when release verification is disabled or another update is active.
    pub async fn check_native_if_idle(&self, choice: &str) {
        if self.status().disabled.is_none() && !self.busy() {
            let _ = self.check_native(choice).await;
        }
    }
    pub async fn install(
        &self,
        source: &impl Source,
        installer: &impl Installer,
    ) -> Result<Status, Error> {
        let _gate = self
            .gate
            .try_lock()
            .map_err(Error::invalid_context("更新操作正在进行"))?;
        if self.status().phase == "installing" {
            return Err(Error::invalid("安装程序已接续"));
        }
        let token = self.begin("downloading")?;
        let _operation = Operation(self);
        let result = async {
            let status = self.status();
            if !status.available {
                return Err(Error::invalid("没有可安装更新，请先检查更新"));
            }
            let latest = status
                .latest
                .ok_or_else(|| Error::invalid("缺少发布信息"))?;
            let plan = self
                .plan
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
                .ok_or_else(|| Error::invalid("未选择安装包"))?;
            let name = plan.asset.as_deref().ok_or_else(|| {
                Error::invalid(plan.manual.clone().unwrap_or_else(|| "请手动更新".into()))
            })?;
            let package = latest
                .assets
                .iter()
                .find(|asset| asset.name == name)
                .ok_or_else(|| Error::invalid("发布缺少平台安装包"))?;
            if package.size == 0 || package.size > MAX_PACKAGE {
                return Err(Error::invalid("平台安装包大小无效"));
            }
            for required in ["SHA256SUMS", "SHA256SUMS.sig", "RELEASE_VERSION"] {
                if !latest.assets.iter().any(|asset| asset.name == required) {
                    return Err(Error::invalid(format!("发布缺少 {required}")));
                }
            }
            let base = format!(
                "https://github.com/{}/releases/download/{}",
                self.config.repository, latest.tag
            );
            let manifest = self
                .get(source, &format!("{base}/SHA256SUMS"), 1024 * 1024, &token)
                .await?;
            let signature = self
                .get(source, &format!("{base}/SHA256SUMS.sig"), 64, &token)
                .await?;
            release::authenticate(&manifest, &signature, &self.config.key)?;
            let marker = self
                .get(source, &format!("{base}/RELEASE_VERSION"), 128, &token)
                .await?;
            release::signed_version(&manifest, &marker, &latest.tag)?;
            let expected = release::manifest_hash(&manifest, name)?;
            live(&token)?;
            std::fs::create_dir_all(&self.root).map_err(Error::io_context("无法创建更新目录"))?;
            let stage = tempfile::Builder::new()
                .prefix(".update-")
                .tempdir_in(&self.root)
                .map_err(Error::io_context("无法创建私有下载目录"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(stage.path(), std::fs::Permissions::from_mode(0o700))
                    .map_err(Error::io_context("无法保护下载目录"))?;
            }
            let path = stage.path().join(name);
            let mut failure = Error::io("更新包下载失败");
            let mut downloaded = false;
            for target in self
                .config
                .targets(&status.source, &format!("{base}/{name}"))?
            {
                live(&token)?;
                self.progress(0, package.size);
                match source
                    .file(&target, &path, package.size, &token, self)
                    .await
                {
                    Ok(()) => {
                        downloaded = true;
                        break;
                    }
                    Err(error) => {
                        let _ = tokio::fs::remove_file(&path).await;
                        if error.code == "update_cancelled" {
                            return Err(error);
                        }
                        failure = error;
                    }
                }
            }
            if !downloaded {
                return Err(failure);
            }
            live(&token)?;
            if hash(&path, package.size).await? != expected {
                return Err(Error::invalid("更新包 SHA256 不符，已拒绝安装"));
            }
            live(&token)?;
            {
                let token = self
                    .cancel
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                live(&token)?;
                self.change(|state| state.phase = "installing".into());
            }
            installer.apply(&plan, &path).await?;
            let _ = stage.keep();
            Ok(())
        }
        .await;
        self.finish(&result);
        result?;
        Ok(self.status())
    }
    pub async fn install_native(&self) -> Result<Status, Error> {
        self.install(&Http::new()?, &NativeInstaller).await
    }
}
async fn hash(path: &Path, expected: u64) -> Result<String, Error> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(Error::io_context("更新包不可读"))?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut digest = Sha256::new();
    let mut total = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .await
            .map_err(Error::io_context("读取更新包失败"))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > expected {
            return Err(Error::invalid("更新包大小不符"));
        }
        if let Some(bytes) = buffer.get(..count) {
            digest.update(bytes);
        }
    }
    if total != expected {
        return Err(Error::invalid("更新包不完整"));
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    #[tokio::test]
    async fn actual_https_stream_enforces_response_limits_partial_downloads_and_cancellation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (cert, key) = rustfrp_core::tls::self_signed_pem(vec!["localhost".into()]).unwrap();
        let identity =
            rustfrp_core::tls::identity_from_pem(cert.as_bytes(), key.as_bytes()).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(
            rustfrp_core::tls::app_server_config(identity).unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else {
                        return;
                    };
                    let mut request = vec![0u8; 4096];
                    let count = stream.read(&mut request).await.unwrap();
                    let text = String::from_utf8_lossy(&request[..count]);
                    let path = text.split_whitespace().nth(1).unwrap_or("/");
                    let reply=match path {"/missing"=>"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n","/oversized"=>"HTTP/1.1 200 OK\r\nContent-Length: 99\r\nConnection: close\r\n\r\n","/streamed"=>"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nabcdef\r\n0\r\n\r\n","/incomplete"=>"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabc","/redirect"=>"HTTP/1.1 302 Found\r\nLocation: http://localhost/unsafe\r\nContent-Length: 0\r\nConnection: close\r\n\r\n","/waiting"=>{tokio::time::sleep(Duration::from_secs(1)).await;return},_=>"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata"};
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        let client = reqwest::Client::builder()
            .https_only(true)
            .add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes()).unwrap())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let http = Http { client };
        let root = tempfile::tempdir().unwrap();
        let (config, _) = fixture();
        let updates = updater(config, root.path());
        let token = CancellationToken::new();
        let url = |path: &str| format!("https://localhost:{port}{path}");
        assert_eq!(http.bytes(&url("/data"), 4, &token).await.unwrap(), b"data");
        assert!(http.bytes(&url("/oversized"), 4, &token).await.is_err());
        assert!(http.bytes(&url("/streamed"), 4, &token).await.is_err());
        assert!(http.bytes(&url("/missing"), 4, &token).await.is_err());
        assert!(http.bytes(&url("/redirect"), 4, &token).await.is_err());
        assert!(http.bytes(&url("/incomplete"), 4, &token).await.is_err());
        let file = root.path().join("package");
        http.file(&url("/data"), &file, 4, &token, &updates)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"data");
        assert_eq!(updates.status().downloaded, 4);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(http
            .file(&url("/data"), &file, 4, &token, &updates)
            .await
            .is_err());
        assert!(http
            .file(
                &url("/data"),
                &root.path().join("zero"),
                0,
                &token,
                &updates
            )
            .await
            .is_err());
        assert!(http
            .file(
                &url("/data"),
                &root.path().join("wrong"),
                5,
                &token,
                &updates
            )
            .await
            .is_err());
        assert!(http
            .file(
                &url("/streamed"),
                &root.path().join("over"),
                4,
                &token,
                &updates
            )
            .await
            .is_err());
        assert!(http
            .file(
                &url("/streamed"),
                &root.path().join("short"),
                8,
                &token,
                &updates
            )
            .await
            .is_err());
        let cancel = token.clone();
        let waiting_url = url("/waiting");
        let waiting = http.bytes(&waiting_url, 4, &token);
        let interrupt = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (result, ()) = tokio::join!(waiting, interrupt);
        assert_eq!(result.unwrap_err().code, "update_cancelled");
        assert!(http
            .file(
                &url("/data"),
                &root.path().join("cancel"),
                4,
                &token,
                &updates
            )
            .await
            .is_err());
        server.abort();
        assert!(Http::new().is_ok());
    }
    struct Feed {
        entries: BTreeMap<String, Vec<u8>>,
        package: Vec<u8>,
        requests: Mutex<Vec<String>>,
        failed: bool,
        file_failed: bool,
    }
    struct Oversized;
    impl Source for Oversized {
        async fn bytes(
            &self,
            _: &str,
            limit: usize,
            _: &CancellationToken,
        ) -> Result<Vec<u8>, Error> {
            Ok(vec![b'x'; limit + 1])
        }

        async fn file(
            &self,
            _: &str,
            _: &Path,
            _: u64,
            _: &CancellationToken,
            _: &Updates,
        ) -> Result<(), Error> {
            Err(Error::io("unexpected fixture download"))
        }
    }
    impl Source for Feed {
        async fn bytes(
            &self,
            url: &str,
            _limit: usize,
            token: &CancellationToken,
        ) -> Result<Vec<u8>, Error> {
            live(token)?;
            self.requests.lock().unwrap().push(url.into());
            if self.failed || url.starts_with("https://broken.example/") {
                return Err(Error::io("fixture connection failure"));
            }
            let name = url.rsplit('/').next().unwrap();
            self.entries
                .get(name)
                .cloned()
                .ok_or_else(|| Error::io("fixture missing asset"))
        }
        async fn file(
            &self,
            url: &str,
            path: &Path,
            size: u64,
            token: &CancellationToken,
            progress: &Updates,
        ) -> Result<(), Error> {
            live(token)?;
            self.requests.lock().unwrap().push(url.into());
            if self.failed || self.file_failed || url.starts_with("https://broken.example/") {
                tokio::fs::write(path, b"partial").await.unwrap();
                return Err(Error::io("fixture failed download"));
            }
            tokio::fs::write(path, &self.package).await.unwrap();
            progress.progress(self.package.len() as u64, size);
            Ok(())
        }
    }
    struct Install {
        count: AtomicUsize,
        fail: bool,
    }
    impl Installer for Install {
        async fn apply(&self, plan: &install::Plan, path: &Path) -> Result<(), Error> {
            assert!(plan.asset.is_some());
            assert_eq!(tokio::fs::read(path).await.unwrap(), b"signed test package");
            self.count.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(Error::io("fixture installer failure"))
            } else {
                Ok(())
            }
        }
    }
    fn fixture() -> (Config, Feed) {
        let key = ring::signature::Ed25519KeyPair::from_pkcs8(
            ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let marker = b"v0.2.0\n";
        let package = b"signed test package";
        let manifest = format!(
            "{:x}  RELEASE_VERSION\n{:x}  package.zip\n",
            Sha256::digest(marker),
            Sha256::digest(package)
        );
        let signature = key.sign(manifest.as_bytes());
        let feed=serde_json::to_vec(&serde_json::json!([{"tag_name":"v0.2.0","assets":[{"name":"package.zip","size":package.len()},{"name":"SHA256SUMS","size":manifest.len()},{"name":"SHA256SUMS.sig","size":64},{"name":"RELEASE_VERSION","size":marker.len()}]}])).unwrap();
        (
            Config {
                repository: "owner/project".into(),
                key: key
                    .public_key()
                    .as_ref()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
                mirrors: vec![
                    "https://broken.example".into(),
                    "https://mirror.example".into(),
                ],
            },
            Feed {
                entries: BTreeMap::from([
                    ("releases?per_page=100".into(), feed),
                    ("SHA256SUMS".into(), manifest.into_bytes()),
                    ("SHA256SUMS.sig".into(), signature.as_ref().to_vec()),
                    ("RELEASE_VERSION".into(), marker.to_vec()),
                ]),
                package: package.to_vec(),
                requests: Mutex::new(Vec::new()),
                failed: false,
                file_failed: false,
            },
        )
    }
    fn updater(config: Config, root: &Path) -> Updates {
        Updates::new(
            config,
            "0.1.0",
            root.join("downloads"),
            root.join("installed/app"),
        )
    }
    fn plan(_: &[String]) -> install::Plan {
        install::Plan {
            asset: Some("package.zip".into()),
            manual: None,
            method: install::Method::Windows {
                executable: "/installed/app.exe".into(),
            },
        }
    }
    #[tokio::test]
    async fn signed_streamed_update_handoffs_only_after_all_checks_and_cannot_install_twice() {
        let root = tempfile::tempdir().unwrap();
        let (config, feed) = fixture();
        let updates = updater(config, root.path());
        let installer = Install {
            count: AtomicUsize::new(0),
            fail: false,
        };
        assert!(!updates.busy());
        assert!(updates.install(&feed, &installer).await.is_err());
        let status = updates.check("github", &feed, plan).await.unwrap();
        assert!(status.available);
        assert!(status.error.is_none());
        assert_eq!(status.latest.unwrap().version, "0.2.0");
        let status = updates.install(&feed, &installer).await.unwrap();
        assert_eq!(status.phase, "installing");
        assert_eq!(status.downloaded, status.total);
        assert_eq!(installer.count.load(Ordering::SeqCst), 1);
        assert!(updates.busy());
        assert!(updates.cancel().is_err());
        assert!(updates.install(&feed, &installer).await.is_err());
        assert!(updates.check("github", &feed, plan).await.is_err());
        let entries = std::fs::read_dir(root.path().join("downloads"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                entries[0]
                    .as_ref()
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
    #[tokio::test]
    async fn signature_version_size_and_hash_failures_never_reach_installer_and_remove_private_downloads(
    ) {
        let root = tempfile::tempdir().unwrap();
        for attack in [
            "signature",
            "version",
            "hash",
            "incomplete",
            "oversized",
            "zero-size",
            "missing",
            "installer",
            "download-failed",
            "download-too-large",
        ] {
            let (config, mut feed) = fixture();
            let updates = updater(config, root.path());
            match attack {
                "signature" => {
                    feed.entries.insert("SHA256SUMS.sig".into(), vec![0; 64]);
                }
                "version" => {
                    feed.entries
                        .insert("RELEASE_VERSION".into(), b"v9.9.9\n".to_vec());
                }
                "hash" => feed.package = b"unsigned package!!!".to_vec(),
                "incomplete" => feed.package.pop().map(|_| ()).unwrap(),
                "oversized" | "missing" => {
                    let mut value: serde_json::Value =
                        serde_json::from_slice(feed.entries.get("releases?per_page=100").unwrap())
                            .unwrap();
                    if attack == "oversized" {
                        value[0]["assets"][0]["size"] = serde_json::json!(MAX_PACKAGE + 1);
                    } else {
                        value[0]["assets"]
                            .as_array_mut()
                            .unwrap()
                            .retain(|asset| asset["name"] != "SHA256SUMS.sig");
                    }
                    feed.entries.insert(
                        "releases?per_page=100".into(),
                        serde_json::to_vec(&value).unwrap(),
                    );
                }
                "zero-size" => {
                    let mut value: serde_json::Value =
                        serde_json::from_slice(feed.entries.get("releases?per_page=100").unwrap())
                            .unwrap();
                    value[0]["assets"][0]["size"] = serde_json::json!(0);
                    feed.entries.insert(
                        "releases?per_page=100".into(),
                        serde_json::to_vec(&value).unwrap(),
                    );
                }
                _ => {}
            }
            if attack == "download-failed" {
                feed.file_failed = true;
            }
            let installer = Install {
                count: AtomicUsize::new(0),
                fail: attack == "installer",
            };
            updates.check("github", &feed, plan).await.unwrap();
            if attack == "download-too-large" {
                feed.package.push(b'!');
            }
            assert!(
                updates.install(&feed, &installer).await.is_err(),
                "{attack}"
            );
            assert_eq!(updates.status().phase, "error");
            assert!(!updates.busy());
            assert_eq!(
                installer.count.load(Ordering::SeqCst),
                usize::from(attack == "installer")
            );
            assert_eq!(
                std::fs::read_dir(root.path().join("downloads"))
                    .map(|entries| entries.count())
                    .unwrap_or(0),
                0
            );
        }
    }
    #[tokio::test]
    async fn mirror_fallback_preserves_source_and_failed_checks_clear_previous_release() {
        let root = tempfile::tempdir().unwrap();
        let (config, mut feed) = fixture();
        let updates = updater(config, root.path());
        assert!(updates
            .check("http://arbitrary", &feed, plan)
            .await
            .is_err());
        updates.check("mainland", &feed, plan).await.unwrap();
        assert_eq!(
            updates.status().mirror.as_deref(),
            Some("https://mirror.example")
        );
        assert_eq!(updates.status().source, "mainland");
        assert!(updates
            .install(
                &feed,
                &Install {
                    count: AtomicUsize::new(0),
                    fail: true
                }
            )
            .await
            .is_err());
        assert!(feed
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.starts_with("https://broken.example/")));
        feed.failed = true;
        assert!(updates.check("github", &feed, plan).await.is_err());
        assert!(!updates.status().available);
        assert!(updates.status().latest.is_none());
        assert!(updates.status().error.is_some());
    }
    #[tokio::test]
    async fn up_to_date_manual_install_and_invalid_build_configuration_remain_explicit() {
        let root = tempfile::tempdir().unwrap();
        let (config, feed) = fixture();
        let updates = Updates::new(
            config.clone(),
            "0.2.0",
            root.path().into(),
            root.path().join("app"),
        );
        assert!(
            !updates
                .check("github", &feed, plan)
                .await
                .unwrap()
                .available
        );
        let updates = updater(config.clone(), root.path());
        let status = updates
            .check("github", &feed, |_| install::Plan {
                asset: None,
                manual: Some("manual fixture".into()),
                method: install::Method::Manual,
            })
            .await
            .unwrap();
        assert_eq!(status.manual.as_deref(), Some("manual fixture"));
        assert!(updates
            .install(
                &feed,
                &Install {
                    count: AtomicUsize::new(0),
                    fail: false
                }
            )
            .await
            .is_err());
        for bad in [
            Config {
                key: String::new(),
                ..config.clone()
            },
            Config {
                repository: "invalid".into(),
                ..config.clone()
            },
            Config {
                mirrors: vec!["http://bad.example".into()],
                ..config.clone()
            },
        ] {
            let updates = updater(bad, root.path());
            assert!(updates.status().disabled.is_some());
            assert!(updates.check("github", &feed, plan).await.is_err());
        }
        assert!(Config {
            mirrors: vec![],
            ..config.clone()
        }
        .targets("mainland", "https://github.com")
        .is_err());
        assert!(Config {
            mirrors: vec!["https://mirror.example".into(); 9],
            ..config
        }
        .validate()
        .is_err());
        for bad in [
            "invalid",
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/#part",
        ] {
            assert!(https(bad).is_err());
        }
        let empty = Feed {
            entries: BTreeMap::from([("releases?per_page=100".into(), b"[]".to_vec())]),
            package: Vec::new(),
            requests: Mutex::new(Vec::new()),
            failed: false,
            file_failed: false,
        };
        let (config, _) = fixture();
        let updates = updater(config, root.path());
        assert!(updates
            .check("github", &empty, plan)
            .await
            .unwrap()
            .latest
            .is_none());
    }

    #[tokio::test]
    async fn bounded_update_fetch_rejects_oversized_responses_and_stops_before_network_when_cancelled(
    ) {
        let root = tempfile::tempdir().unwrap();
        let (config, _) = fixture();
        let updates = updater(config, root.path());
        let token = CancellationToken::new();
        assert_eq!(
            updates
                .get(
                    &Oversized,
                    "https://github.com/owner/project/releases",
                    8,
                    &token
                )
                .await
                .unwrap_err()
                .code,
            "invalid_request"
        );

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            updates
                .get(
                    &Oversized,
                    "https://github.com/owner/project/releases",
                    8,
                    &cancelled
                )
                .await
                .unwrap_err()
                .code,
            "update_cancelled"
        );
    }

    #[tokio::test]
    async fn scheduled_update_checks_skip_disabled_and_busy_installers() {
        let root = tempfile::tempdir().unwrap();
        let (config, _) = fixture();
        let disabled = Updates::new(
            Config {
                key: String::new(),
                ..config.clone()
            },
            "0.1.0",
            root.path().join("disabled"),
            root.path().join("app"),
        );
        assert!(disabled.status().disabled.is_some());
        disabled.check_native_if_idle("invalid").await;
        assert_eq!(disabled.status().phase, "idle");

        let updates = updater(config, root.path());
        let gate = updates.gate.lock().await;
        assert!(updates.busy());
        updates.check_native_if_idle("invalid").await;
        assert_eq!(updates.status().phase, "idle");
        drop(gate);
        assert!(!updates.busy());
        updates.check_native_if_idle("invalid").await;
        assert_eq!(updates.status().phase, "idle");
    }

    #[tokio::test]
    async fn native_installer_keeps_manual_plans_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::write(&package, b"verified bytes").unwrap();
        let plan = install::Plan {
            asset: None,
            manual: Some("manual update required".into()),
            method: install::Method::Manual,
        };
        let error = NativeInstaller.apply(&plan, &package).await.unwrap_err();
        assert_eq!(error.code, "invalid_request");
    }
    struct Waiting {
        started: tokio::sync::Notify,
    }
    impl Source for Waiting {
        async fn bytes(
            &self,
            _: &str,
            _: usize,
            token: &CancellationToken,
        ) -> Result<Vec<u8>, Error> {
            self.started.notify_one();
            token.cancelled().await;
            Err(cancelled())
        }
        async fn file(
            &self,
            _: &str,
            _: &Path,
            _: u64,
            _: &CancellationToken,
            _: &Updates,
        ) -> Result<(), Error> {
            unreachable!()
        }
    }
    #[tokio::test]
    async fn cancellation_interrupts_network_check_and_busy_gate_prevents_duplicate_operations() {
        let root = tempfile::tempdir().unwrap();
        let (config, feed) = fixture();
        let updates = Arc::new(updater(config, root.path()));
        let source = Arc::new(Waiting {
            started: tokio::sync::Notify::new(),
        });
        let running = {
            let updates = updates.clone();
            let source = source.clone();
            tokio::spawn(async move { updates.check("github", source.as_ref(), plan).await })
        };
        source.started.notified().await;
        assert!(updates.busy());
        assert!(updates.check("github", &feed, plan).await.is_err());
        assert!(updates
            .install(
                &feed,
                &Install {
                    count: AtomicUsize::new(0),
                    fail: false
                }
            )
            .await
            .is_err());
        updates.cancel().unwrap();
        assert_eq!(running.await.unwrap().unwrap_err().code, "update_cancelled");
        assert_eq!(updates.status().phase, "cancelled");
        assert!(!updates.busy());
        assert!(
            updates
                .check("github", &feed, plan)
                .await
                .unwrap()
                .available
        );
    }
}
