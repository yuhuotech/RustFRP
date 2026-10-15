//! Agent lifecycle belongs to a background job, independent of any WebView.
use crate::{
    config::{self, Replace},
    local, Error,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    future::Future,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub binary: String,
    #[serde(default)]
    pub controller: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state_dir: String,
    #[serde(default)]
    pub config_file: String,
    #[serde(default)]
    pub ca_file: String,
    #[serde(default)]
    pub tls_pin: String,
}

fn absolute(value: &str, label: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 2048
        || value.contains('\0')
        || !Path::new(value).is_absolute()
    {
        return Err(Error::invalid(format!("{label}须使用有效绝对路径")));
    }
    Ok(())
}
impl Config {
    pub fn from_preferences(preferences: &Value, root: &Path) -> Result<Self, Error> {
        let mut value: Self = serde_json::from_value(
            preferences
                .get("local")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(Error::invalid_context("本机连接配置无效"))?;
        if value.binary.is_empty() {
            value.binary = default_binary(root).to_string_lossy().into_owned();
        }
        if value.state_dir.is_empty() {
            value.state_dir = root.join("agent").to_string_lossy().into_owned();
        }
        if value.name.is_empty() {
            value.name = "本机".into();
        }
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), Error> {
        absolute(&self.binary, "Agent 程序")?;
        absolute(&self.state_dir, "身份与状态目录")?;
        if self.discovery().as_os_str().len() > 1024 {
            return Err(Error::invalid("本机状态目录路径过长"));
        }
        if !self.config_file.is_empty() {
            absolute(&self.config_file, "frpc 配置文件")?;
            return Ok(());
        }
        let valid_address = if let Some(rest) = self.controller.strip_prefix('[') {
            rest.split_once(']').is_some_and(|(host, tail)| {
                host.parse::<std::net::Ipv6Addr>().is_ok()
                    && (tail.is_empty()
                        || tail
                            .strip_prefix(':')
                            .and_then(|port| port.parse::<u16>().ok())
                            .is_some_and(|port| port > 0))
            })
        } else {
            let (host, port) = match self.controller.rsplit_once(':') {
                Some((host, port)) => (host, port.parse::<u16>().unwrap_or(0)),
                None => (self.controller.as_str(), 7000),
            };
            host.as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
                && port > 0
        };
        if self.controller.len() > 300 || !valid_address {
            return Err(Error::invalid(
                "请输入有效 Edge 地址，例如 frp.example.com:7000，不带 http://",
            ));
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err(Error::invalid("设备名称须为 1–64 个可打印字符"));
        }
        if !self.ca_file.is_empty() {
            absolute(&self.ca_file, "可信 CA 文件")?;
        }
        if !self.tls_pin.is_empty()
            && (self.tls_pin.len() != 64 || !self.tls_pin.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Error::invalid("证书指纹须为 64 位 SHA-256"));
        }
        Ok(())
    }
    pub fn identity(&self) -> String {
        if self.config_file.is_empty() {
            self.controller.clone()
        } else {
            format!("standalone:{}", self.config_file)
        }
    }
    pub fn discovery(&self) -> PathBuf {
        Path::new(&self.state_dir).join("control.json")
    }
    fn job_path(&self) -> PathBuf {
        self.discovery().with_extension("desktop-job.json")
    }
    pub fn arguments(&self) -> Vec<String> {
        let mut args = vec![
            "--local-control".into(),
            self.discovery().to_string_lossy().into_owned(),
            "--mode".into(),
            "client".into(),
        ];
        if self.config_file.is_empty() {
            args.extend([
                "--controller".into(),
                self.controller.clone(),
                "--state-dir".into(),
                self.state_dir.clone(),
                "--name".into(),
                self.name.clone(),
            ]);
            if !self.ca_file.is_empty() {
                args.extend(["--ca-file".into(), self.ca_file.clone()]);
            }
            if !self.tls_pin.is_empty() {
                args.extend(["--tls-pin".into(), self.tls_pin.clone()]);
            }
        } else {
            args.extend(["-c".into(), self.config_file.clone()]);
        }
        args
    }
}
pub fn default_binary(root: &Path) -> PathBuf {
    root.join("bin").join(if cfg!(windows) {
        "rustfrp-agent.exe"
    } else {
        "rustfrp-agent"
    })
}
/// Linux portable releases retain app/rustfrp_app and carry resources next to that executable.
/// Installed packages use the resource directory supplied by the system WebView shell.
pub fn bundled_agent(
    executable: &Path,
    resources: &Path,
    platform: crate::startup::Platform,
) -> PathBuf {
    let filename = if platform == crate::startup::Platform::Windows {
        "rustfrp-agent.exe"
    } else {
        "rustfrp-agent"
    };
    if platform == crate::startup::Platform::Linux {
        if let Some(directory) = executable
            .parent()
            .filter(|directory| directory.file_name().is_some_and(|name| name == "app"))
        {
            return directory.join("resources/bin").join(filename);
        }
    }
    resources.join("bin").join(filename)
}
pub fn job_label(config: &Config) -> String {
    format!(
        "com.rustfrp.desktop.{}",
        config::hash(config.discovery().to_string_lossy().as_bytes())
    )
}

/// Typed operations make enrollment, idempotence and stop behavior testable.
pub trait Backend: Sync {
    fn status(&self, config: &Config) -> impl Future<Output = Result<Option<Value>, Error>> + Send;
    fn prepare(&self, config: &Config) -> impl Future<Output = Result<(), Error>> + Send;
    fn enrolled(&self, config: &Config) -> impl Future<Output = Result<bool, Error>> + Send;
    fn launch(
        &self,
        config: &Config,
        transient_token: Option<&str>,
    ) -> impl Future<Output = Result<(), Error>> + Send;
    fn stop(&self, config: &Config) -> impl Future<Output = Result<(), Error>> + Send;
    fn unregister(&self, config: &Config) -> impl Future<Output = Result<(), Error>> + Send;
    fn pause(&self) -> impl Future<Output = ()> + Send;
}

async fn wait_start_with_timeout(
    backend: &impl Backend,
    config: &Config,
    enrollment: bool,
    timeout: Duration,
) -> Result<Value, Error> {
    let work = async {
        loop {
            if let Some(status) = backend.status(config).await? {
                if !enrollment
                    || (status.get("connected") == Some(&Value::Bool(true))
                        && backend.enrolled(config).await?)
                {
                    return Ok(status);
                }
            }
            backend.pause().await;
        }
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| Error {
            uncertain: true,
            ..Error::io("Agent 启动尚未确认，请刷新状态后检查接入令牌、证书和程序版本")
        })?
}
async fn wait_start(
    backend: &impl Backend,
    config: &Config,
    enrollment: bool,
) -> Result<Value, Error> {
    wait_start_with_timeout(backend, config, enrollment, Duration::from_secs(15)).await
}
async fn wait_stop_with_timeout(
    backend: &impl Backend,
    config: &Config,
    timeout: Duration,
) -> Result<(), Error> {
    let work = async {
        while backend.status(config).await?.is_some() {
            backend.pause().await;
        }
        Ok(())
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| Error {
            uncertain: true,
            ..Error::io("Agent 仍在退出，请刷新状态确认")
        })?
}
async fn wait_stop(backend: &impl Backend, config: &Config) -> Result<(), Error> {
    wait_stop_with_timeout(backend, config, Duration::from_secs(10)).await
}
pub async fn start(backend: &impl Backend, config: &Config, token: &str) -> Result<Value, Error> {
    config.validate()?;
    if let Some(status) = backend.status(config).await? {
        return Ok(status);
    }
    backend.prepare(config).await?;
    let enrollment = config.config_file.is_empty() && !backend.enrolled(config).await?;
    if enrollment
        && (token.trim().is_empty() || token.len() > 2048 || token.chars().any(char::is_control))
    {
        return Err(Error::invalid(
            "首次接入需要有效的一次性令牌；令牌不会保存在桌面偏好或后台服务中",
        ));
    }
    if enrollment {
        backend.launch(config, Some(token.trim())).await?;
        if let Err(error) = wait_start(backend, config, true).await {
            // Only our own freshly launched, authenticated process may be stopped.
            if backend.status(config).await.ok().flatten().is_some() {
                let _ = backend.stop(config).await;
            }
            return Err(error);
        }
        backend.stop(config).await?;
        wait_stop(backend, config).await?;
    }
    backend.launch(config, None).await?;
    wait_start(backend, config, false).await
}
pub async fn stop(backend: &impl Backend, config: &Config) -> Result<(), Error> {
    config.validate()?;
    if backend.status(config).await?.is_some() {
        backend.stop(config).await?;
    }
    backend.unregister(config).await?;
    wait_stop(backend, config).await
}

fn private_directory(path: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(Error::invalid("状态目录不能是文件或符号链接")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(Error::io_context("无法创建本机状态目录"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(Error::io_context("无法保护本机状态目录"))?;
            }
            Ok(())
        }
        Err(_) => Err(Error::io("无法检查本机状态目录")),
    }
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid("后台文件路径无效"))?;
    private_directory(parent)?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err(Error::invalid("后台文件不能是目录或符号链接"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(Error::io_context("无法创建私有后台文件"))?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(Error::io_context("无法保存后台文件"))?;
    config::AtomicReplace
        .apply(temporary.path(), path)
        .map_err(Error::io_context("无法应用后台文件"))?;
    Ok(())
}

fn read_private(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, Error> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::io("无法读取后台状态文件")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit as u64 {
        return Err(Error::invalid("后台状态文件无效或超过容量"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::invalid("后台状态文件必须仅当前用户可读写"));
        }
    }
    let file = std::fs::File::open(path).map_err(Error::io_context("无法打开后台状态文件"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io_context("读取后台状态失败"))?;
    if bytes.len() > limit {
        return Err(Error::invalid("后台状态超过容量"));
    }
    Ok(Some(bytes))
}
fn enrolled(config: &Config) -> Result<bool, Error> {
    let Some(bytes) = read_private(&Path::new(&config.state_dir).join("identity.json"), 16384)?
    else {
        return Ok(false);
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(Error::invalid_context(
        "本机身份文件损坏，请保留原文件并使用新的状态目录",
    ))?;
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("本机身份缺少密钥"))?;
    if rustfrp_core::identity::DeviceKey::from_hex(key).is_err() {
        return Err(Error::invalid("本机身份密钥无效"));
    }
    match value.get("client_id") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::String(id))
            if !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) =>
        {
            Ok(true)
        }
        _ => Err(Error::invalid("本机身份的客户端标识无效")),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    version: u8,
    enabled: bool,
    config: Config,
}
fn save_job(config: &Config, enabled: bool) -> Result<(), Error> {
    private_write(
        &config.job_path(),
        &serde_json::to_vec(&Job {
            version: 1,
            enabled,
            config: config.clone(),
        })
        .map_err(Error::invalid_context("后台服务编码失败"))?,
    )
}
fn load_job(path: &Path) -> Result<Job, Error> {
    absolute(&path.to_string_lossy(), "后台服务文件")?;
    let bytes = read_private(path, 16384)?.ok_or_else(|| Error::invalid("后台服务定义不存在"))?;
    let job: Job =
        serde_json::from_slice(&bytes).map_err(Error::invalid_context("后台服务定义无效"))?;
    job.config.validate()?;
    if job.version != 1 || job.config.job_path() != path {
        return Err(Error::invalid("后台服务身份不匹配"));
    }
    Ok(job)
}
/// The launchd definition never includes the one-time enrollment token.
pub fn mac_plist(config: &Config) -> Result<String, Error> {
    config.validate()?;
    let xml = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    let mut arguments = format!("<string>{}</string>", xml(&config.binary));
    for argument in config.arguments() {
        arguments.push_str(&format!("<string>{}</string>", xml(&argument)));
    }
    Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{arguments}</array><key>EnvironmentVariables</key><dict><key>RUSTFRP_SUPERVISED</key><string>1</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>2</integer></dict></plist>", job_label(config)))
}

fn detached(executable: &Path, arguments: &[String]) -> Result<(), Error> {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .env_remove("RUSTFRP_SUPERVISED")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000200 | 0x00000008);
    }
    command.spawn().map_err(Error::io_context(
        "无法启动后台 Agent，请检查程序路径与执行权限",
    ))?;
    Ok(())
}
#[cfg(target_os = "macos")]
fn mac_uid() -> Result<String, Error> {
    let output = crate::process::run("/usr/bin/id", &["-u".into()], Duration::from_secs(5))?;
    let uid = output.stdout;
    if !output.success || uid.trim().parse::<u32>().is_err() {
        return Err(Error::invalid("用户会话无效"));
    }
    Ok(uid.trim().into())
}
#[cfg(target_os = "macos")]
fn unregister_mac(config: &Config) -> Result<(), Error> {
    let output = crate::process::run(
        "/bin/launchctl",
        &[
            "bootout".into(),
            format!("gui/{}/{}", mac_uid()?, job_label(config)),
        ],
        Duration::from_secs(5),
    )?;
    if !output.success && output.code != Some(3) {
        return Err(Error::io("无法移除本机后台服务，请稍后刷新"));
    }
    Ok(())
}
fn register_background(config: &Config, shell: &Path) -> Result<(), Error> {
    save_job(config, true)?;
    #[cfg(target_os = "macos")]
    {
        let _ = shell;
        unregister_mac(config)?;
        let path = config.discovery().with_extension("launchd.plist");
        private_write(&path, mac_plist(config)?.as_bytes())?;
        let output = crate::process::run(
            "/bin/launchctl",
            &[
                "bootstrap".into(),
                format!("gui/{}", mac_uid()?),
                path.to_string_lossy().into_owned(),
            ],
            Duration::from_secs(5),
        )?;
        if !output.success {
            save_job(config, false)?;
            return Err(Error::io(
                "无法交给 macOS 后台运行，请检查 Agent 程序与用户会话",
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        detached(
            shell,
            &[
                "--agent-supervisor".into(),
                config.job_path().to_string_lossy().into_owned(),
            ],
        )
    }
}
fn unregister_background(config: &Config) -> Result<(), Error> {
    if config.job_path().exists() {
        save_job(config, false)?;
    }
    #[cfg(target_os = "macos")]
    {
        unregister_mac(config)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct System {
    pub root: PathBuf,
    pub shell: PathBuf,
    pub bundled_agent: Option<PathBuf>,
}
impl System {
    fn prepare_sync(&self, config: &Config) -> Result<(), Error> {
        private_directory(Path::new(&config.state_dir))?;
        if !Path::new(&config.binary).exists() {
            if Path::new(&config.binary) != default_binary(&self.root) {
                return Err(Error::invalid("自定义 Agent 程序不存在，请选择有效程序"));
            }
            let bundle = self
                .bundled_agent
                .as_ref()
                .ok_or_else(|| Error::io("发布包缺少随包 Agent，请重新安装或选择 Agent 程序"))?;
            install_bundled(bundle, Path::new(&config.binary))?;
        }
        let metadata = std::fs::symlink_metadata(&config.binary)
            .map_err(Error::io_context("Agent 程序不可读"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::invalid("Agent 程序必须是普通文件"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(Error::invalid("Agent 程序没有执行权限"));
            }
        }
        if !config.config_file.is_empty() {
            let value = config::read(Path::new(&config.config_file))?;
            if value.get("exists") != Some(&Value::Bool(true)) {
                return Err(Error::invalid("独立配置文件不存在，请先编辑并保存"));
            }
            config::verify(
                Path::new(&config.config_file),
                value.get("text").and_then(Value::as_str).unwrap_or(""),
            )?;
        } else if !config.ca_file.is_empty() && !Path::new(&config.ca_file).is_file() {
            return Err(Error::invalid("可信 CA 文件不存在"));
        }
        Ok(())
    }
}
impl Backend for System {
    async fn status(&self, config: &Config) -> Result<Option<Value>, Error> {
        local::status(&config.discovery(), &config.identity()).await
    }
    async fn prepare(&self, config: &Config) -> Result<(), Error> {
        let this = self.clone();
        let config = config.clone();
        tokio::task::spawn_blocking(move || this.prepare_sync(&config))
            .await
            .map_err(Error::io_context("启动准备任务失败"))?
    }
    async fn enrolled(&self, config: &Config) -> Result<bool, Error> {
        let config = config.clone();
        tokio::task::spawn_blocking(move || enrolled(&config))
            .await
            .map_err(Error::io_context("读取身份任务失败"))?
    }
    async fn launch(&self, config: &Config, token: Option<&str>) -> Result<(), Error> {
        let config = config.clone();
        let shell = self.shell.clone();
        let token = token.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            if let Some(token) = token {
                let mut args = config.arguments();
                args.extend(["--enroll-token".into(), token]);
                detached(Path::new(&config.binary), &args)
            } else {
                register_background(&config, &shell)
            }
        })
        .await
        .map_err(Error::io_context("后台启动任务失败"))?
    }
    async fn stop(&self, config: &Config) -> Result<(), Error> {
        local::request(&config.discovery(), &config.identity(), "stop", None, None).await?;
        Ok(())
    }
    async fn unregister(&self, config: &Config) -> Result<(), Error> {
        let config = config.clone();
        tokio::task::spawn_blocking(move || unregister_background(&config))
            .await
            .map_err(Error::io_context("后台停止任务失败"))?
    }
    async fn pause(&self) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Install only a missing default binary; a custom or upgraded binary is preserved.
pub fn install_bundled(source: &Path, destination: &Path) -> Result<(), Error> {
    let metadata =
        std::fs::symlink_metadata(source).map_err(Error::io_context("随包 Agent 不可读"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > 32 * 1024 * 1024
    {
        return Err(Error::invalid("随包 Agent 文件无效或超过 32 MiB"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| Error::invalid("Agent 安装路径无效"))?;
    private_directory(parent)?;
    let mut source =
        std::fs::File::open(source).map_err(Error::io_context("无法打开随包 Agent"))?;
    let mut stage =
        tempfile::NamedTempFile::new_in(parent).map_err(Error::io_context("无法暂存随包 Agent"))?;
    std::io::copy(&mut (&mut source).take(32 * 1024 * 1024 + 1), &mut stage)
        .and_then(|_| stage.as_file().sync_all())
        .map_err(Error::io_context("无法复制随包 Agent"))?;
    if stage
        .as_file()
        .metadata()
        .map_err(Error::io_context("无法检查 Agent 大小"))?
        .len()
        > 32 * 1024 * 1024
    {
        return Err(Error::invalid("随包 Agent 在复制期间超过容量"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        stage
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(Error::io_context("无法设置 Agent 执行权限"))?;
    }
    stage
        .persist_noclobber(destination)
        .map_err(Error::io_context("无法安装 Agent，目标已存在或目录不可写"))?;
    Ok(())
}

pub struct RunResult {
    pub code: Option<i32>,
    pub lived: Duration,
}
pub trait SupervisorHost {
    fn enabled(&mut self) -> Result<bool, Error>;
    fn run_agent(&mut self) -> Result<RunResult, Error>;
    fn pause(&mut self, duration: Duration);
}
pub fn supervise(host: &mut impl SupervisorHost) -> Result<(), Error> {
    let mut failures = 0u32;
    while host.enabled()? {
        let result = host.run_agent();
        if result.as_ref().is_ok_and(|r| r.code == Some(0)) {
            return Ok(());
        }
        if !host.enabled()? {
            return Ok(());
        }
        let update = result.as_ref().is_ok_and(|r| r.code == Some(75));
        if result
            .as_ref()
            .is_ok_and(|r| r.lived >= Duration::from_secs(60))
        {
            failures = 0;
        }
        failures = failures.saturating_add(1).min(5);
        host.pause(if update {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1u64 << failures.saturating_sub(1))
        });
    }
    Ok(())
}
struct ProcessSupervisor {
    path: PathBuf,
    config: Config,
}
impl SupervisorHost for ProcessSupervisor {
    fn enabled(&mut self) -> Result<bool, Error> {
        let job = load_job(&self.path)?;
        if job.config.arguments() != self.config.arguments()
            || job.config.binary != self.config.binary
        {
            return Err(Error::invalid("后台配置变化，请停止旧 Agent 后重新启动"));
        }
        Ok(job.enabled)
    }
    fn run_agent(&mut self) -> Result<RunResult, Error> {
        let start = std::time::Instant::now();
        let status = Command::new(&self.config.binary)
            .args(self.config.arguments())
            .env("RUSTFRP_SUPERVISED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(Error::io_context("无法运行后台 Agent"))?;
        Ok(RunResult {
            code: status.code(),
            lived: start.elapsed(),
        })
    }
    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}
pub fn run_supervisor(path: &Path) -> Result<(), Error> {
    let job = load_job(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock_path = path.with_extension("desktop-job.lock");
    if std::fs::symlink_metadata(&lock_path)
        .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
    {
        return Err(Error::invalid("后台锁文件无效"));
    }
    let lock = options
        .open(lock_path)
        .map_err(Error::io_context("无法打开后台锁"))?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(_) => return Err(Error::io("无法锁定后台服务")),
    }
    let mut host = ProcessSupervisor {
        path: path.into(),
        config: job.config,
    };
    supervise(&mut host)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};
    fn cfg(directory: &Path) -> Config {
        Config {
            binary: directory.join("agent").to_string_lossy().into_owned(),
            state_dir: directory.join("state").to_string_lossy().into_owned(),
            controller: "host:7000".into(),
            name: "桌面".into(),
            ..Default::default()
        }
    }
    fn running(connected: bool) -> Value {
        json!({"version":1,"pid":42,"connected":connected,"revision":0,"proxies":0})
    }
    #[test]
    #[cfg(unix)]
    fn real_supervisor_restarts_exit_75_preserves_environment_and_refuses_duplicate_or_changed_jobs(
    ) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        private_directory(Path::new(&config.state_dir)).unwrap();
        let sentinel = directory.path().join("runs");
        std::fs::write(&config.binary,format!("#!/bin/sh\n[ \"$RUSTFRP_SUPERVISED\" = 1 ] || exit 0\nif [ -f '{}' ]; then printf 'second' >> '{}'; exit 0; fi\nprintf 'first' > '{}'; exit 75\n",sentinel.display(),sentinel.display(),sentinel.display())).unwrap();
        std::fs::set_permissions(&config.binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        save_job(&config, true).unwrap();
        run_supervisor(&config.job_path()).unwrap();
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "firstsecond");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(config.job_path().with_extension("desktop-job.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        run_supervisor(&config.job_path()).unwrap();
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "firstsecond");
        fs2::FileExt::unlock(&lock).unwrap();
        let mut host = ProcessSupervisor {
            path: config.job_path(),
            config: config.clone(),
        };
        assert!(host.enabled().unwrap());
        let mut changed = config.clone();
        changed.name = "changed".into();
        save_job(&changed, true).unwrap();
        assert!(host.enabled().is_err());
        save_job(&config, false).unwrap();
        run_supervisor(&config.job_path()).unwrap();
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "firstsecond");
        std::fs::remove_file(&config.binary).unwrap();
        assert!(host.run_agent().is_err());
        let lock_path = config.job_path().with_extension("desktop-job.lock");
        std::fs::remove_file(&lock_path).unwrap();
        std::os::unix::fs::symlink(&sentinel, &lock_path).unwrap();
        assert!(run_supervisor(&config.job_path()).is_err());
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "firstsecond");
        std::fs::remove_file(lock_path).unwrap();
        private_write(&config.job_path(), b"invalid").unwrap();
        assert!(run_supervisor(&config.job_path()).is_err());
        assert!(detached(Path::new("/missing/rustfrp-agent"), &[]).is_err());
    }
    #[test]
    fn configuration_bounds_and_private_files_do_not_accept_corruption_or_non_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let config = cfg(root);
        assert_eq!(
            bundled_agent(
                Path::new("/opt/rustfrp/app/rustfrp_app"),
                Path::new("/usr/lib/rustfrp_app"),
                crate::startup::Platform::Linux
            ),
            Path::new("/opt/rustfrp/app/resources/bin/rustfrp-agent")
        );
        assert_eq!(
            bundled_agent(
                Path::new("/usr/bin/rustfrp_app"),
                Path::new("/usr/lib/rustfrp_app"),
                crate::startup::Platform::Linux
            ),
            Path::new("/usr/lib/rustfrp_app/bin/rustfrp-agent")
        );
        assert_eq!(
            bundled_agent(
                Path::new("/"),
                Path::new("/Resources"),
                crate::startup::Platform::Mac
            ),
            Path::new("/Resources/bin/rustfrp-agent")
        );
        assert_eq!(
            bundled_agent(
                Path::new("/app/rustfrp_app.exe"),
                Path::new("/Resources"),
                crate::startup::Platform::Windows
            ),
            Path::new("/Resources/bin/rustfrp-agent.exe")
        );
        for address in [
            "host",
            "host.example:443",
            "127.0.0.1:7000",
            "[::1]",
            "[2001:db8::1]:7000",
        ] {
            let mut candidate = config.clone();
            candidate.controller = address.into();
            candidate.validate().unwrap();
        }
        for address in [
            "",
            "http://host",
            "host:0",
            "host:bad",
            "host:65536",
            "-host:80",
            "a/b",
            "[not-ip]:80",
            "[::1]:0",
            "[::1]extra",
            "[::1",
            "::1:80",
        ] {
            let mut candidate = config.clone();
            candidate.controller = address.into();
            assert!(candidate.validate().is_err(), "{address}");
        }
        for name in ["", " ", "invalid\n", &"中".repeat(65)] {
            let mut candidate = config.clone();
            candidate.name = name.into();
            assert!(candidate.validate().is_err());
        }
        for (field, value) in [
            ("binary", "relative"),
            ("stateDir", ""),
            ("caFile", "relative"),
            ("tlsPin", "not-a-pin"),
            ("configFile", "relative"),
        ] {
            let mut value_json = serde_json::to_value(&config).unwrap();
            value_json[field] = json!(value);
            let candidate: Config = serde_json::from_value(value_json).unwrap();
            assert!(candidate.validate().is_err());
        }
        let mut candidate = config.clone();
        candidate.state_dir = format!("/{}", "s".repeat(1100));
        assert!(candidate.validate().is_err());
        candidate.state_dir = format!("/{}", "s".repeat(2050));
        assert!(candidate.validate().is_err());
        assert!(Config::from_preferences(&json!({"local":{"unknown":1}}), root).is_err());
        assert!(private_write(Path::new("/"), b"data").is_err());
        assert!(private_write(root, b"data").is_err());
        assert!(private_directory(&root.join("blocked/nested")).is_ok());
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"original").unwrap();
        assert!(private_directory(&blocker).is_err());
        assert!(private_directory(&blocker.join("child")).is_err());
        assert!(private_write(&blocker.join("child"), b"other").is_err());
        assert!(read_private(root, 20).is_err());
        let large = root.join("large");
        std::fs::write(&large, b"123456").unwrap();
        assert!(read_private(&large, 5).is_err());
        private_directory(Path::new(&config.state_dir)).unwrap();
        let identity = Path::new(&config.state_dir).join("identity.json");
        std::fs::write(&identity, vec![b' '; 16385]).unwrap();
        assert!(enrolled(&config).is_err());
        let mut job = json!({"version":2,"enabled":true,"config":config});
        private_write(&config.job_path(), job.to_string().as_bytes()).unwrap();
        assert!(load_job(&config.job_path()).is_err());
        job["version"] = json!(1);
        job["config"]["stateDir"] = json!(root.join("other"));
        private_write(&config.job_path(), job.to_string().as_bytes()).unwrap();
        assert!(load_job(&config.job_path()).is_err());
        std::fs::write(&large, b"bundle").unwrap();
        assert!(install_bundled(&root.join("missing"), &root.join("dest")).is_err());
        assert!(install_bundled(root, &root.join("dest")).is_err());
        assert!(install_bundled(&large, &blocker.join("dest")).is_err());
        let file = std::fs::File::create(&large).unwrap();
        file.set_len(32 * 1024 * 1024 + 1).unwrap();
        assert!(install_bundled(&large, &root.join("dest")).is_err());
        assert_eq!(std::fs::read(blocker).unwrap(), b"original");
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn actual_system_preparation_rejects_missing_bundles_invalid_executables_and_configs_without_starting_jobs(
    ) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let system = System {
            root: root.into(),
            shell: root.join("shell"),
            bundled_agent: None,
        };
        let mut config = cfg(root);
        assert!(system.prepare(&config).await.is_err());
        assert!(!config.job_path().exists());
        config.binary = default_binary(root).to_string_lossy().into_owned();
        assert!(system.prepare(&config).await.is_err());
        let bundle = root.join("bundle");
        std::fs::write(&bundle, b"agent").unwrap();
        let system = System {
            bundled_agent: Some(bundle),
            ..system
        };
        system.prepare(&config).await.unwrap();
        assert_eq!(std::fs::read(&config.binary).unwrap(), b"agent");
        assert!(!config.job_path().exists());
        std::fs::set_permissions(&config.binary, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(system.prepare(&config).await.is_err());
        std::fs::set_permissions(&config.binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        config.ca_file = root.join("missing.pem").to_string_lossy().into_owned();
        assert!(system.prepare(&config).await.is_err());
        std::fs::write(&config.ca_file, b"certificate").unwrap();
        system.prepare(&config).await.unwrap();
        config.config_file = root.join("frpc.toml").to_string_lossy().into_owned();
        assert!(system.prepare(&config).await.is_err());
        std::fs::write(&config.config_file, b"not =").unwrap();
        assert!(system.prepare(&config).await.is_err());
        std::fs::write(
            &config.config_file,
            b"serverAddr='localhost'\nserverPort=7000\n",
        )
        .unwrap();
        system.prepare(&config).await.unwrap();
        assert!(!system.enrolled(&config).await.unwrap());
        let discovery = config.discovery();
        assert!(system.status(&config).await.unwrap().is_none());
        assert!(system.stop(&config).await.is_err());
        assert!(!discovery.exists());
        let linked = root.join("linked-agent");
        std::os::unix::fs::symlink(&config.binary, &linked).unwrap();
        config.binary = linked.to_string_lossy().into_owned();
        assert!(system.prepare(&config).await.is_err());
        config.binary = root.to_string_lossy().into_owned();
        assert!(system.prepare(&config).await.is_err());
        assert!(!config.job_path().exists());
    }
    struct Fake {
        states: Mutex<VecDeque<Result<Option<Value>, Error>>>,
        identities: Mutex<VecDeque<bool>>,
        calls: Mutex<Vec<String>>,
        preparation: bool,
    }
    impl Fake {
        fn new(states: Vec<Option<Value>>, identities: Vec<bool>) -> Self {
            Self {
                states: Mutex::new(states.into_iter().map(Ok).collect()),
                identities: Mutex::new(identities.into()),
                calls: Mutex::new(Vec::new()),
                preparation: true,
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn push(&self, call: &str) {
            self.calls.lock().unwrap().push(call.into());
        }
    }
    impl Backend for Fake {
        async fn status(&self, _: &Config) -> Result<Option<Value>, Error> {
            self.push("status");
            self.states
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected status request"))
        }
        async fn prepare(&self, _: &Config) -> Result<(), Error> {
            self.push("prepare");
            if self.preparation {
                Ok(())
            } else {
                Err(Error::io("missing bundle"))
            }
        }
        async fn enrolled(&self, _: &Config) -> Result<bool, Error> {
            self.push("enrolled");
            Ok(self.identities.lock().unwrap().pop_front().unwrap())
        }
        async fn launch(&self, _: &Config, token: Option<&str>) -> Result<(), Error> {
            self.push(if token == Some("dummy-token") {
                "enroll"
            } else if token.is_none() {
                "background"
            } else {
                panic!("unexpected token")
            });
            Ok(())
        }
        async fn stop(&self, _: &Config) -> Result<(), Error> {
            self.push("stop");
            Ok(())
        }
        async fn unregister(&self, _: &Config) -> Result<(), Error> {
            self.push("unregister");
            Ok(())
        }
        async fn pause(&self) {
            self.push("pause");
        }
    }
    struct WaitingBackend {
        status: Option<Value>,
    }
    impl Backend for WaitingBackend {
        async fn status(&self, _: &Config) -> Result<Option<Value>, Error> {
            Ok(self.status.clone())
        }
        async fn prepare(&self, _: &Config) -> Result<(), Error> {
            Ok(())
        }
        async fn enrolled(&self, _: &Config) -> Result<bool, Error> {
            Ok(false)
        }
        async fn launch(&self, _: &Config, _: Option<&str>) -> Result<(), Error> {
            Ok(())
        }
        async fn stop(&self, _: &Config) -> Result<(), Error> {
            Ok(())
        }
        async fn unregister(&self, _: &Config) -> Result<(), Error> {
            Ok(())
        }
        async fn pause(&self) {
            tokio::task::yield_now().await;
        }
    }
    #[tokio::test]
    async fn lifecycle_waits_report_uncertain_timeout_without_blocking_the_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        let pending = WaitingBackend { status: None };
        let start = wait_start_with_timeout(&pending, &config, false, Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(start.uncertain);
        assert!(start.message.contains("尚未确认"));

        let stopping = WaitingBackend {
            status: Some(running(false)),
        };
        let stop = wait_stop_with_timeout(&stopping, &config, Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(stop.uncertain);
        assert!(stop.message.contains("仍在退出"));
    }
    #[tokio::test]
    async fn first_enrollment_is_transient_and_background_service_never_gets_the_token() {
        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        let backend = Fake::new(
            vec![
                None,
                Some(running(false)),
                Some(running(true)),
                Some(running(true)),
                None,
                None,
                Some(running(true)),
            ],
            vec![false, true],
        );
        assert_eq!(
            start(&backend, &config, " dummy-token ").await.unwrap(),
            running(true)
        );
        assert_eq!(
            backend.calls(),
            [
                "status",
                "prepare",
                "enrolled",
                "enroll",
                "status",
                "pause",
                "status",
                "enrolled",
                "stop",
                "status",
                "pause",
                "status",
                "background",
                "status",
                "pause",
                "status"
            ]
        );
        assert!(!config
            .arguments()
            .iter()
            .any(|arg| arg == "dummy-token" || arg == "--enroll-token"));
    }
    #[tokio::test]
    async fn existing_identity_and_standalone_start_without_credentials_and_repeated_start_is_idempotent(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let mut config = cfg(directory.path());
        let already = Fake::new(vec![Some(running(true))], vec![]);
        start(&already, &config, "").await.unwrap();
        assert_eq!(already.calls(), ["status"]);
        let existing = Fake::new(vec![None, Some(running(false))], vec![true]);
        assert_eq!(
            start(&existing, &config, "ignored").await.unwrap(),
            running(false)
        );
        assert_eq!(
            existing.calls(),
            ["status", "prepare", "enrolled", "background", "status"]
        );
        config.config_file = directory
            .path()
            .join("frpc.toml")
            .to_string_lossy()
            .into_owned();
        config.controller = "".into();
        config.tls_pin = "bad inactive field".into();
        let standalone = Fake::new(vec![None, Some(running(false))], vec![]);
        start(&standalone, &config, "").await.unwrap();
        assert_eq!(
            standalone.calls(),
            ["status", "prepare", "background", "status"]
        );
        assert_eq!(
            config.identity(),
            format!("standalone:{}", config.config_file)
        );
        assert!(config.arguments().contains(&"-c".into()));
        assert!(!config.arguments().contains(&"--controller".into()));
    }
    #[tokio::test]
    async fn failed_status_never_starts_a_duplicate_and_start_failures_do_not_register_a_job() {
        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        let backend = Fake::new(vec![], vec![]);
        backend
            .states
            .lock()
            .unwrap()
            .push_back(Err(Error::io("timeout")));
        assert!(start(&backend, &config, "dummy-token").await.is_err());
        assert_eq!(backend.calls(), ["status"]);
        let mut missing = Fake::new(vec![None], vec![]);
        missing.preparation = false;
        assert!(start(&missing, &config, "dummy-token").await.is_err());
        assert_eq!(missing.calls(), ["status", "prepare"]);
        for token in ["", "a\nb", &"x".repeat(2049)] {
            let backend = Fake::new(vec![None], vec![false]);
            assert!(start(&backend, &config, token).await.is_err());
            assert!(!backend.calls().contains(&"enroll".into()));
            assert!(!backend.calls().contains(&"background".into()));
        }
        let failed = Fake::new(vec![None, None, Some(running(false))], vec![false]);
        failed
            .states
            .lock()
            .unwrap()
            .insert(2, Err(Error::io("enrollment failure")));
        assert!(start(&failed, &config, "dummy-token").await.is_err());
        assert!(failed.calls().contains(&"stop".into()));
        assert!(!failed.calls().contains(&"background".into()));
    }
    #[tokio::test]
    async fn stop_uses_authenticated_control_and_unregisters_before_waiting_without_killing_a_pid()
    {
        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        let backend = Fake::new(
            vec![Some(running(true)), Some(running(false)), None],
            vec![],
        );
        stop(&backend, &config).await.unwrap();
        assert_eq!(
            backend.calls(),
            ["status", "stop", "unregister", "status", "pause", "status"]
        );
        let stopped = Fake::new(vec![None, None], vec![]);
        stop(&stopped, &config).await.unwrap();
        assert_eq!(stopped.calls(), ["status", "unregister", "status"]);
    }
    #[test]
    fn configuration_and_launchd_preserve_paths_tls_and_xml_without_shell_interpretation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut config = cfg(root);
        config.name = "中文 & <name> \" '".into();
        config.ca_file = root.join("trusted & CA.pem").to_string_lossy().into_owned();
        config.tls_pin = "a".repeat(64);
        config.validate().unwrap();
        let args = config.arguments();
        assert!(args.contains(&config.ca_file));
        assert!(args.contains(&config.name));
        assert!(args.contains(&config.tls_pin));
        let plist = mac_plist(&config).unwrap();
        assert!(plist.contains("&amp;"));
        assert!(plist.contains("&lt;"));
        assert!(plist.contains("&quot;"));
        assert!(plist.contains("&apos;"));
        assert!(plist.contains("SuccessfulExit"));
        assert!(plist.contains("<key>RUSTFRP_SUPERVISED</key><string>1</string>"));
        assert!(!plist.contains("--enroll-token"));
        assert!(plist.contains(&job_label(&config)));
        let defaults = Config::from_preferences(&json!({}), root).unwrap();
        assert_eq!(Path::new(&defaults.binary), default_binary(root));
        assert_eq!(defaults.name, "本机");
        assert!(Config::from_preferences(&json!({"local":{"token":"bad"}}), root).is_err());
        for bad in [
            "",
            "https://host",
            "host:0",
            "host:65536",
            "host:bad",
            "-host",
            "host:1:2",
            "host/path",
        ] {
            let mut value = config.clone();
            value.controller = bad.into();
            assert!(value.validate().is_err(), "{bad}");
        }
        for bad in ["", "relative/agent"] {
            let mut value = config.clone();
            value.binary = bad.into();
            assert!(value.validate().is_err());
        }
        let mut bad = config.clone();
        bad.tls_pin = "z".repeat(64);
        assert!(bad.validate().is_err());
        bad.tls_pin = "a".repeat(63);
        assert!(bad.validate().is_err());
        bad = config.clone();
        bad.name = "中".repeat(65);
        assert!(bad.validate().is_err());
        bad.name = "a\nb".into();
        assert!(bad.validate().is_err());
        bad.name = "中".repeat(64);
        bad.validate().unwrap();
        bad = config.clone();
        bad.config_file = "relative.toml".into();
        assert!(bad.validate().is_err());
        bad.config_file.clear();
        bad.ca_file = "relative.pem".into();
        assert!(bad.validate().is_err());
    }
    #[test]
    fn identity_jobs_and_bundle_installation_are_private_bounded_and_preserve_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        private_directory(Path::new(&config.state_dir)).unwrap();
        assert!(!enrolled(&config).unwrap());
        let identity = Path::new(&config.state_dir).join("identity.json");
        let key = rustfrp_core::identity::DeviceKey::generate()
            .unwrap()
            .to_hex();
        private_write(
            &identity,
            json!({"key":key,"client_id":null}).to_string().as_bytes(),
        )
        .unwrap();
        assert!(!enrolled(&config).unwrap());
        private_write(
            &identity,
            json!({"key":key,"client_id":"client-1"})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(enrolled(&config).unwrap());
        for invalid in [
            json!({}),
            json!({"key":"x".repeat(64)}),
            json!({"key":key,"client_id":3}),
            json!({"key":key,"client_id":""}),
        ] {
            private_write(&identity, invalid.to_string().as_bytes()).unwrap();
            assert!(enrolled(&config).is_err());
        }
        private_write(&identity, b"invalid").unwrap();
        assert!(enrolled(&config).is_err());
        save_job(&config, true).unwrap();
        let job = load_job(&config.job_path()).unwrap();
        assert!(job.enabled);
        assert_eq!(job.config.identity(), config.identity());
        save_job(&config, false).unwrap();
        assert!(!load_job(&config.job_path()).unwrap().enabled);
        let source = directory.path().join("bundle");
        std::fs::write(&source, b"agent-binary").unwrap();
        let destination = directory.path().join("bin/agent");
        install_bundled(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"agent-binary");
        assert!(install_bundled(&source, &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"agent-binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(config.job_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&destination)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        std::fs::write(&source, []).unwrap();
        assert!(install_bundled(&source, &directory.path().join("other")).is_err());
        assert!(load_job(&directory.path().join("missing")).is_err());
        assert!(load_job(Path::new("relative")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&destination, directory.path().join("symlink")).unwrap();
            assert!(install_bundled(
                &directory.path().join("symlink"),
                &directory.path().join("other")
            )
            .is_err());
            assert!(read_private(&directory.path().join("symlink"), 16384).is_err());
        }
    }
    struct FakeSupervisor {
        enabled: VecDeque<bool>,
        exits: VecDeque<Result<RunResult, Error>>,
        pauses: Vec<Duration>,
        runs: usize,
    }
    impl SupervisorHost for FakeSupervisor {
        fn enabled(&mut self) -> Result<bool, Error> {
            Ok(self.enabled.pop_front().unwrap())
        }
        fn run_agent(&mut self) -> Result<RunResult, Error> {
            self.runs += 1;
            self.exits.pop_front().unwrap()
        }
        fn pause(&mut self, duration: Duration) {
            self.pauses.push(duration);
        }
    }
    #[test]
    fn supervisor_recovers_update_and_failure_with_bounded_backoff_and_stops_on_success() {
        let mut host = FakeSupervisor {
            enabled: vec![true; 15].into(),
            exits: VecDeque::new(),
            pauses: vec![],
            runs: 0,
        };
        host.exits.push_back(Ok(RunResult {
            code: Some(75),
            lived: Duration::ZERO,
        }));
        for _ in 0..4 {
            host.exits.push_back(Err(Error::io("launch failed")));
        }
        host.exits.push_back(Ok(RunResult {
            code: None,
            lived: Duration::from_secs(60),
        }));
        host.exits.push_back(Ok(RunResult {
            code: Some(0),
            lived: Duration::ZERO,
        }));
        supervise(&mut host).unwrap();
        assert_eq!(host.runs, 7);
        assert_eq!(
            host.pauses,
            [
                Duration::from_millis(100),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(1)
            ]
        );
        let mut disabled = FakeSupervisor {
            enabled: vec![false].into(),
            exits: VecDeque::new(),
            pauses: vec![],
            runs: 0,
        };
        supervise(&mut disabled).unwrap();
        assert_eq!(disabled.runs, 0);
        let mut stop = FakeSupervisor {
            enabled: vec![true, false].into(),
            exits: vec![Ok(RunResult {
                code: Some(75),
                lived: Duration::ZERO,
            })]
            .into(),
            pauses: vec![],
            runs: 0,
        };
        supervise(&mut stop).unwrap();
        assert!(stop.pauses.is_empty());
    }

    #[test]
    fn a_second_supervisor_exits_cleanly_when_the_owned_job_lock_is_held() {
        use fs2::FileExt;

        let directory = tempfile::tempdir().unwrap();
        let config = cfg(directory.path());
        save_job(&config, true).unwrap();
        let job = config.job_path();
        let lock_path = job.with_extension("desktop-job.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        assert!(run_supervisor(&job).is_ok());
    }
}
