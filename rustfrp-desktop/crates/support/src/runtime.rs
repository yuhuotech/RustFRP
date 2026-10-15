//! Portable IPC business logic; native window and system calls stay in the shell.
use crate::{
    config,
    controller::ControllerClient,
    diagnostics,
    lifecycle::{self, Config, System},
    local, settings, startup, updates, Error,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::sync::{Mutex, Semaphore};

pub struct Runtime {
    pub controller: ControllerClient,
    pub updates: updates::Updates,
    preferences: Mutex<Value>,
    local_gate: Mutex<()>,
    local_reads: Semaphore,
    system: System,
    version: String,
    startup: Option<startup::Startup>,
}
impl Runtime {
    pub fn new(system: System, version: &str) -> Result<Self, Error> {
        Ok(Self {
            controller: ControllerClient::default(),
            updates: updates::Updates::new(
                updates::Config::compiled(),
                version,
                system.root.join("updates"),
                system.shell.clone(),
            ),
            preferences: Mutex::new(settings::load(&system.root)?),
            local_gate: Mutex::new(()),
            local_reads: Semaphore::new(2),
            system,
            version: version.into(),
            startup: None,
        })
    }
    pub fn with_startup(
        system: System,
        version: &str,
        startup: startup::Startup,
    ) -> Result<Self, Error> {
        let mut runtime = Self::new(system, version)?;
        runtime.startup = Some(startup);
        Ok(runtime)
    }
    pub async fn preferences(&self) -> Value {
        self.preferences.lock().await.clone()
    }
    pub fn keep_in_tray(&self) -> bool {
        self.preferences
            .try_lock()
            .ok()
            .and_then(|value| value.get("close_to_tray").and_then(Value::as_bool))
            .unwrap_or(true)
    }
    pub fn idle(&self) -> bool {
        self.local_gate.try_lock().is_ok() && !self.updates.busy()
    }
    async fn configuration(&self) -> Result<Config, Error> {
        let preferences = self.preferences.lock().await;
        Config::from_preferences(&preferences, &self.system.root)
    }
    async fn persist(&self, value: Value) -> Result<Value, Error> {
        let root = self.system.root.clone();
        let saved = value.clone();
        tokio::task::spawn_blocking(move || settings::save(&root, &saved))
            .await
            .map_err(Error::io_context("偏好保存任务失败"))??;
        *self.preferences.lock().await = value.clone();
        Ok(value)
    }
    pub async fn command(&self, action: &str, args: Value) -> Result<Value, Error> {
        if !args.is_object()
            || serde_json::to_vec(&args)
                .map_err(Error::invalid_context("桌面参数无效"))?
                .len()
                > 2 * 1024 * 1024
        {
            return Err(Error::invalid("桌面参数超过限制"));
        }
        let _read = if matches!(
            action,
            "localStatus" | "localTunnels" | "localLogs" | "exportDiagnostics"
        ) {
            Some(
                self.local_reads
                    .try_acquire()
                    .map_err(Error::invalid_context("本机读取正在进行，请稍后刷新"))?,
            )
        } else {
            None
        };
        match action {
            "info" => Ok(
                json!({"desktop":true,"version":self.version,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"local_defaults":{"binary":lifecycle::default_binary(&self.system.root),"stateDir":self.system.root.join("agent"),"name":"本机"}}),
            ),
            "preferences" => Ok(self.preferences().await),
            "updateStatus" => serde_json::to_value(self.updates.status())
                .map_err(Error::io_context("更新状态无效")),
            "checkUpdate" => {
                let preferences = self.preferences().await;
                let source = preferences
                    .get("update_source")
                    .and_then(Value::as_str)
                    .unwrap_or("github");
                serde_json::to_value(self.updates.check_native(source).await?)
                    .map_err(Error::io_context("更新状态无效"))
            }
            "installUpdate" => serde_json::to_value(self.updates.install_native().await?)
                .map_err(Error::io_context("更新状态无效")),
            "cancelUpdate" => serde_json::to_value(self.updates.cancel()?)
                .map_err(Error::io_context("更新状态无效")),
            "savePreferences" => {
                let _operation = self
                    .local_gate
                    .try_lock()
                    .map_err(Error::invalid_context("本机操作正在进行，请稍后再试"))?;
                let current = self.preferences().await;
                let mut next = current.clone();
                let object = next
                    .as_object_mut()
                    .ok_or_else(|| Error::invalid("桌面偏好格式无效"))?;
                for (key, value) in args
                    .as_object()
                    .ok_or_else(|| Error::invalid("偏好参数无效"))?
                {
                    object.insert(key.clone(), value.clone());
                }
                settings::validate(&next)?;
                if args.get("local").is_some() && args.get("local") != current.get("local") {
                    let previous = Config::from_preferences(&current, &self.system.root)?;
                    if local::status(&previous.discovery(), &previous.identity())
                        .await?
                        .is_some()
                    {
                        return Err(Error::invalid(
                            "请先停止本机 Agent，再修改连接、身份目录或程序路径",
                        ));
                    }
                    Config::from_preferences(&next, &self.system.root)?.validate()?;
                }
                self.persist(next).await
            }
            "selectController" => {
                let _operation = self
                    .local_gate
                    .try_lock()
                    .map_err(Error::invalid_context("桌面设置操作正在进行，请稍后再试"))?;
                let url = argument(&args, "url")?;
                let prepared = ControllerClient::prepare(url)?;
                let normalized = prepared.address();
                let mut next = self.preferences().await;
                next.as_object_mut()
                    .ok_or_else(|| Error::invalid("桌面偏好格式无效"))?
                    .insert("controller_url".into(), json!(normalized));
                self.persist(next).await?;
                self.controller.commit(prepared).await;
                Ok(json!({"url":normalized}))
            }
            "readConfig" | "verifyConfig" | "saveConfig" => {
                let _operation = self
                    .local_gate
                    .try_lock()
                    .map_err(Error::invalid_context("本机操作正在进行，请稍后再试"))?;
                let configuration = self.configuration().await?;
                if configuration.config_file.is_empty() {
                    return Err(Error::invalid("请先选择独立模式配置文件"));
                }
                let path = PathBuf::from(configuration.config_file);
                let action = action.to_string();
                let text = if action == "readConfig" {
                    String::new()
                } else {
                    argument(&args, "text")?.to_string()
                };
                let original = args
                    .get("original_digest")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let preview = args
                    .get("preview_digest")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let accept = args
                    .get("accept_warnings")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                tokio::task::spawn_blocking(move || match action.as_str() {
                    "readConfig" => config::read(&path),
                    "verifyConfig" => config::verify(&path, &text),
                    _ => config::save(
                        &path,
                        &text,
                        &original,
                        &preview,
                        accept,
                        &config::AtomicReplace,
                    ),
                })
                .await
                .map_err(Error::io_context("本机配置任务失败"))?
            }
            "localStart" | "localStop" => {
                let _operation = self
                    .local_gate
                    .try_lock()
                    .map_err(Error::invalid_context("本机操作正在进行，请稍后再试"))?;
                let configuration = self.configuration().await?;
                if action == "localStart" {
                    let token = args
                        .get("enrollToken")
                        .map(|value| {
                            value
                                .as_str()
                                .ok_or_else(|| Error::invalid("接入令牌须为文本"))
                        })
                        .transpose()?
                        .unwrap_or("");
                    lifecycle::start(&self.system, &configuration, token).await
                } else {
                    lifecycle::stop(&self.system, &configuration).await?;
                    Ok(Value::Null)
                }
            }
            "localStatus" => {
                let configuration = self.configuration().await?;
                Ok(
                    local::status(&configuration.discovery(), &configuration.identity())
                        .await?
                        .unwrap_or(Value::Null),
                )
            }
            "startup" | "setStartup" => {
                let _operation = self
                    .local_gate
                    .try_lock()
                    .map_err(Error::invalid_context("系统设置正在应用，请稍后再试"))?;
                let enabled = if action == "setStartup" {
                    Some(
                        args.get("enabled")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| Error::invalid("自启动设置须为布尔值"))?,
                    )
                } else {
                    None
                };
                let executable = self.system.shell.clone();
                let supplied = self.startup.clone();
                tokio::task::spawn_blocking(move || {
                    let startup = supplied.map_or_else(|| startup::current(&executable), Ok)?;
                    let enabled = match enabled {
                        Some(enabled) => startup.set(enabled, &startup::TaskScheduler)?,
                        None => startup.enabled(&startup::TaskScheduler)?,
                    };
                    Ok(json!(enabled))
                })
                .await
                .map_err(Error::io_context("自启动任务失败"))?
            }
            "localTunnels" => {
                let configuration = self.configuration().await?;
                local::tunnels(&configuration.discovery(), &configuration.identity()).await
            }
            "localLogs" => {
                let configuration = self.configuration().await?;
                let after = args
                    .get("after")
                    .map(|value| {
                        value
                            .as_u64()
                            .filter(|value| *value <= 9_007_199_254_740_991)
                            .ok_or_else(|| Error::invalid("日志游标无效"))
                    })
                    .transpose()?;
                local::request(
                    &configuration.discovery(),
                    &configuration.identity(),
                    "logs",
                    after,
                    None,
                )
                .await
            }
            "exportDiagnostics" => {
                let configuration = self.configuration().await?;
                let status = local::request(
                    &configuration.discovery(),
                    &configuration.identity(),
                    "status",
                    None,
                    None,
                )
                .await?;
                let logs = local::request(
                    &configuration.discovery(),
                    &configuration.identity(),
                    "logs",
                    None,
                    None,
                )
                .await?;
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(Error::io_context("系统时间无效"))?
                    .as_secs();
                let value = diagnostics::contents(&status, &logs, at)?;
                let path = PathBuf::from(configuration.state_dir);
                let path =
                    tokio::task::spawn_blocking(move || diagnostics::export(&path, &value, at))
                        .await
                        .map_err(Error::io_context("诊断导出任务失败"))??;
                Ok(json!({"path":path}))
            }
            _ => Err(Error::invalid("不支持的桌面操作")),
        }
    }
}
fn argument<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid(format!("缺少有效参数：{key}")))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn injected_startup_roundtrips_through_commands_without_replacing_the_users_definition() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("startup-home");
        let legacy = home.join(".config/autostart/rustfrp.desktop");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"existing application").unwrap();
        let mut startup = startup::Startup::isolated(home, root.path().join("app"));
        startup.platform = startup::Platform::Linux;
        let isolated = startup.path();
        let app = Runtime::with_startup(
            System {
                root: root.path().join("preferences"),
                shell: root.path().join("app"),
                bundled_agent: None,
            },
            "startup-test",
            startup,
        )
        .unwrap();
        assert_eq!(app.command("startup", json!({})).await.unwrap(), false);
        assert_eq!(
            app.command("setStartup", json!({"enabled":true}))
                .await
                .unwrap(),
            true
        );
        assert_eq!(app.command("startup", json!({})).await.unwrap(), true);
        assert!(isolated.is_file());
        assert_eq!(
            app.command("setStartup", json!({"enabled":false}))
                .await
                .unwrap(),
            false
        );
        assert_eq!(app.command("startup", json!({})).await.unwrap(), false);
        assert!(!isolated.exists());
        assert_eq!(std::fs::read(legacy).unwrap(), b"existing application");
    }
    #[tokio::test]
    async fn failed_controller_preference_commit_preserves_transport_and_os_update_commands_remain_typed(
    ) {
        let root = tempfile::tempdir().unwrap();
        let app = runtime(root.path());
        assert!(app.controller.address().await.is_none());
        app.command("selectController", json!({"url":"http://127.0.0.1:1234"}))
            .await
            .unwrap();
        let preferences = root.path().join("preferences.json");
        std::fs::remove_file(&preferences).unwrap();
        std::fs::create_dir(&preferences).unwrap();
        assert!(app
            .command("selectController", json!({"url":"http://127.0.0.1:5678"}))
            .await
            .is_err());
        assert_eq!(
            app.controller.address().await.as_deref(),
            Some("http://127.0.0.1:1234/")
        );
        assert_eq!(
            app.preferences().await["controller_url"],
            "http://127.0.0.1:1234/"
        );
        assert!(app.command("updateStatus", json!({})).await.unwrap()["disabled"].is_string());
        assert!(app.command("checkUpdate", json!({})).await.is_err());
        assert!(app.command("installUpdate", json!({})).await.is_err());
        assert_eq!(
            app.command("cancelUpdate", json!({})).await.unwrap()["phase"],
            "idle"
        );
        assert!(app.command("setStartup", json!({})).await.is_err());
        let gate = app.local_gate.lock().await;
        assert!(!app.idle());
        assert!(app
            .command("savePreferences", json!({"theme":"light"}))
            .await
            .is_err());
        assert!(app
            .command("selectController", json!({"url":"http://127.0.0.1"}))
            .await
            .is_err());
        assert!(app
            .command("setStartup", json!({"enabled":false}))
            .await
            .is_err());
        drop(gate);
        let reads = app.local_reads.acquire_many(2).await.unwrap();
        assert!(app.command("localStatus", json!({})).await.is_err());
        drop(reads);
        assert!(app.idle());
    }
    fn runtime(root: &std::path::Path) -> Runtime {
        Runtime::new(
            System {
                root: root.into(),
                shell: root.join("desktop"),
                bundled_agent: None,
            },
            "test-version",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn local_read_commands_share_authenticated_status_logs_and_tunnels_without_leaking_secrets(
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let root = tempfile::tempdir().unwrap();
        let app = runtime(root.path());
        let state = root.path().join("agent-state");
        let binary = root.path().join("rustfrp-agent");
        app.command(
            "savePreferences",
            json!({"local":{"binary":binary,"stateDir":state,"controller":"edge.example:7000","name":"native"}}),
        )
        .await
        .unwrap();

        let discovery = state.join("control.json");
        std::fs::create_dir_all(state).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            &discovery,
            json!({"version":1,"port":port,"token":"a".repeat(64),"controller":"edge.example:7000"}).to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&discovery, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let status = json!({"version":1,"pid":42,"connected":true,"revision":3,"proxies":1,"agent_version":"0.1.0","traffic":{"bytes_in":0,"bytes_out":12,"connections":1}});
        let tunnel = json!({"revision":3,"items":[{"id":"tcp","kind":"tcp","local_ip":"127.0.0.1","local_port":8080,"remote_port":6000}],"next_offset":null});
        let logs = json!({"items":["agent ready","token=private"],"cursor":43,"gap":false});
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..6 {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let reply = match request["command"].as_str().unwrap() {
                    "status" => &status,
                    "tunnels" => &tunnel,
                    "logs" => &logs,
                    command => panic!("unexpected local command: {command}"),
                };
                reader
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
                requests.push(request);
            }
            requests
        });

        assert_eq!(
            app.command("localStatus", json!({})).await.unwrap()["pid"],
            42
        );
        assert_eq!(
            app.command("localTunnels", json!({})).await.unwrap()["items"][0]["id"],
            "tcp"
        );
        assert_eq!(
            app.command("localLogs", json!({"after":42})).await.unwrap()["cursor"],
            43
        );
        let exported = app.command("exportDiagnostics", json!({})).await.unwrap();
        let path = PathBuf::from(exported["path"].as_str().unwrap());
        let report = std::fs::read_to_string(path).unwrap();
        assert!(report.contains("agent ready"));
        assert!(report.contains("[redacted]"));
        assert!(!report.contains("token=private"));
        let attempted = app
            .command(
                "savePreferences",
                json!({"local":{"binary":binary,"stateDir":root.path().join("other-state"),"controller":"other.example:7000","name":"native"}}),
            )
            .await
            .unwrap_err();
        assert!(attempted.message.contains("先停止本机 Agent"));
        assert_eq!(
            app.preferences().await["local"]["controller"],
            "edge.example:7000"
        );

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 6);
        assert_eq!(requests[0]["token"], "a".repeat(64));
        assert_eq!(requests[2]["after"], 42);

        app.command(
            "savePreferences",
            json!({"local":{"binary":binary,"stateDir":root.path().join("other-state"),"controller":"other.example:7000","name":"native"}}),
        )
        .await
        .unwrap();
        assert_eq!(
            app.preferences().await["local"]["controller"],
            "other.example:7000"
        );

        let active_state = root.path().join("other-state");
        std::fs::create_dir_all(&active_state).unwrap();
        let active_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let active_port = active_listener.local_addr().unwrap().port();
        let active_discovery = active_state.join("control.json");
        std::fs::write(
            &active_discovery,
            json!({"version":1,"port":active_port,"token":"b".repeat(64),"controller":"other.example:7000"}).to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&active_discovery, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let active = tokio::spawn(async move {
            let (socket, _) = active_listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            let result = reader
                .get_mut()
                .write_all(
                    b"{\"version\":1,\"pid\":7,\"connected\":true,\"revision\":0,\"proxies\":0}\n",
                )
                .await;
            assert!(result.is_ok());
        });
        let error = app
            .command(
                "savePreferences",
                json!({"local":{"binary":binary,"stateDir":root.path().join("third-state"),"controller":"third.example:7000","name":"native"}}),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("先停止本机 Agent"));
        assert_eq!(
            app.preferences().await["local"]["controller"],
            "other.example:7000"
        );
        active.await.unwrap();

        assert_eq!(
            app.command("localStatus", json!({})).await.unwrap(),
            Value::Null
        );
        assert!(app.command("notACommand", json!({})).await.is_err());
        assert!(app.command("info", json!([])).await.is_err());
        assert!(app
            .command("info", json!({"large":"x".repeat(2 * 1024 * 1024)}))
            .await
            .is_err());
    }
    #[tokio::test]
    async fn preferences_commands_are_private_persisted_and_do_not_store_secrets_or_unknown_capabilities(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let app = runtime(directory.path());
        let info = app.command("info", json!({})).await.unwrap();
        assert_eq!(info["version"], "test-version");
        assert_eq!(
            info["local_defaults"]["stateDir"],
            directory.path().join("agent").to_string_lossy().as_ref()
        );
        assert!(app.keep_in_tray());
        app.command(
            "savePreferences",
            json!({"theme":"dark","close_to_tray":false}),
        )
        .await
        .unwrap();
        assert!(!app.keep_in_tray());
        let preferences = app.preferences.lock().await;
        assert!(app.keep_in_tray());
        drop(preferences);
        assert_eq!(
            app.command("preferences", json!({})).await.unwrap()["theme"],
            "dark"
        );
        assert_eq!(
            runtime(directory.path()).preferences().await["theme"],
            "dark"
        );
        for value in [
            json!({"password":"dummy"}),
            json!({"local":{"enrollToken":"dummy"}}),
            json!([]),
            json!({"huge":"a".repeat(2*1024*1024)}),
        ] {
            assert!(app.command("savePreferences", value).await.is_err());
        }
        assert!(app
            .command("shell", json!({"command":"anything"}))
            .await
            .is_err());
        assert!(app.command("selectController", json!({})).await.is_err());
        assert!(app
            .command(
                "selectController",
                json!({"url":"https://user:password@host"})
            )
            .await
            .is_err());
        let selected = app
            .command("selectController", json!({"url":"http://127.0.0.1:1234"}))
            .await
            .unwrap();
        assert_eq!(selected["url"], "http://127.0.0.1:1234/");
        assert_eq!(app.preferences().await["controller_url"], selected["url"]);
        assert!(app.command("readConfig", json!({})).await.is_err());
        assert_eq!(
            app.command("localStatus", json!({})).await.unwrap(),
            Value::Null
        );
        assert!(app.command("localLogs", json!({"after":-1})).await.is_err());
        assert!(app
            .command("localLogs", json!({"after":9_007_199_254_740_992u64}))
            .await
            .is_err());
        assert!(app.command("localTunnels", json!({})).await.is_err());
        assert!(app.command("exportDiagnostics", json!({})).await.is_err());
        assert!(app
            .command("localStart", json!({"enrollToken":3}))
            .await
            .is_err());
        assert!(app.command("localStop", json!({})).await.is_err());
    }
    #[tokio::test]
    async fn ipc_full_configuration_verification_requires_both_digests_and_preserves_file_on_errors(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let app = runtime(directory.path());
        let path = directory.path().join("frpc.toml");
        app.command("savePreferences", json!({"local":{"binary":directory.path().join("agent"),"stateDir":directory.path().join("state"),"configFile":path}})).await.unwrap();
        let current = app.command("readConfig", json!({})).await.unwrap();
        assert_eq!(current["exists"], false);
        assert!(app.command("verifyConfig", json!({})).await.is_err());
        assert!(app
            .command("verifyConfig", json!({"text":"invalid syntax"}))
            .await
            .is_err());
        let text = "serverAddr='127.0.0.1'\nserverPort=7000\n[[proxies]]\nname='tcp'\ntype='tcp'\nlocalPort=8080\nremotePort=9000\n";
        let preview = app
            .command("verifyConfig", json!({"text":text}))
            .await
            .unwrap();
        assert_eq!(preview["proxies"], 1);
        assert!(app.command("saveConfig", json!({"text":text,"preview_digest":"wrong","original_digest":"","accept_warnings":true})).await.is_err());
        assert!(!path.exists());
        app.command("saveConfig", json!({"text":text,"preview_digest":preview["digest"],"original_digest":"","accept_warnings":true})).await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(app.command("saveConfig", json!({"text":text,"preview_digest":preview["digest"],"original_digest":"","accept_warnings":true})).await.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let gate = app.local_gate.lock().await;
        assert!(app
            .command("verifyConfig", json!({"text":text}))
            .await
            .unwrap_err()
            .message
            .contains("正在进行"));
        assert!(app.command("localStart", json!({})).await.is_err());
        assert!(app
            .command("savePreferences", json!({"theme":"light"}))
            .await
            .is_err());
        drop(gate);
    }
}
