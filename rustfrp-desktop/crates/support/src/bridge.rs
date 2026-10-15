//! Typed desktop IPC policy. OS adapters receive validated operations only.
use crate::{external_url, runtime::Runtime, trusted_origin, window::UiState, Error};
use reqwest::Url;
use serde_json::{json, Value};
use std::{future::Future, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    Hide,
    Show,
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Config,
    Certificate,
    Agent,
    Directory,
}

pub trait Host: Sync {
    fn copy(&self, text: String) -> impl Future<Output = Result<(), Error>> + Send;
    fn open_url(&self, url: String) -> impl Future<Output = Result<(), Error>> + Send;
    fn window(&self, action: WindowAction) -> Result<(), Error>;
    fn report_window(&self, state: UiState) -> Result<(), Error>;
    fn request_notifications(&self) -> Result<(), Error>;
    fn pick_file(
        &self,
        kind: FileKind,
    ) -> impl Future<Output = Result<Option<PathBuf>, Error>> + Send;
    fn exit_after_update(&self);
}

pub trait Services: Sync {
    fn preferences(&self) -> impl Future<Output = Value> + Send;
    fn command(
        &self,
        action: &str,
        args: Value,
    ) -> impl Future<Output = Result<Value, Error>> + Send;
}
impl Services for Runtime {
    async fn preferences(&self) -> Value {
        self.preferences().await
    }
    async fn command(&self, action: &str, args: Value) -> Result<Value, Error> {
        self.command(action, args).await
    }
}

pub fn authorize(label: &str, url: &Url) -> Result<(), Error> {
    if label != "main" || !trusted_origin(url) {
        return Err(Error {
            status: 403,
            code: "untrusted_window".into(),
            message: "该窗口无权调用桌面能力".into(),
            uncertain: false,
        });
    }
    Ok(())
}

pub async fn command(
    services: &impl Services,
    host: &impl Host,
    action: &str,
    args: Value,
) -> Result<Value, Error> {
    if !args.is_object() || args.to_string().len() > 2 * 1024 * 1024 {
        return Err(Error::invalid("桌面参数超过限制"));
    }
    match action {
        "copy" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("复制内容须为文本"))?;
            if text.len() > 1024 * 1024 {
                return Err(Error::invalid("复制内容超过 1 MiB"));
            }
            host.copy(text.to_owned()).await?;
            Ok(Value::Null)
        }
        "openUrl" => {
            let url = external_url(
                args.get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("缺少链接"))?,
            )?;
            host.open_url(url).await?;
            Ok(Value::Null)
        }
        "window" => {
            let action = match args.get("action").and_then(Value::as_str) {
                Some("minimize") => WindowAction::Minimize,
                Some("hide") => WindowAction::Hide,
                Some("show") => WindowAction::Show,
                Some("close") => WindowAction::Close,
                _ => return Err(Error::invalid("不允许的窗口操作")),
            };
            host.window(action)?;
            Ok(Value::Null)
        }
        "windowState" => {
            let state =
                serde_json::from_value(args).map_err(Error::invalid_context("窗口状态无效"))?;
            host.report_window(state)?;
            Ok(Value::Null)
        }
        "notifications" => Ok(json!({
            "enabled": services.preferences().await.get("notifications").and_then(Value::as_bool).unwrap_or(true),
            "system_managed": true
        })),
        "setNotifications" => {
            let enabled = args
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::invalid("通知设置须为布尔值"))?;
            if enabled {
                host.request_notifications()?;
            }
            services
                .command("savePreferences", json!({"notifications":enabled}))
                .await?;
            Ok(json!({"enabled":enabled,"system_managed":true}))
        }
        "pickFile" => {
            let kind = match args.get("kind").and_then(Value::as_str).unwrap_or("config") {
                "config" => FileKind::Config,
                "certificate" => FileKind::Certificate,
                "agent" => FileKind::Agent,
                "directory" => FileKind::Directory,
                _ => return Err(Error::invalid("不支持的文件选择类型")),
            };
            Ok(host
                .pick_file(kind)
                .await?
                .map_or(Value::Null, |path| json!(path)))
        }
        _ => {
            let result = services.command(action, args).await?;
            if action == "installUpdate"
                && result.get("phase").and_then(Value::as_str) == Some("installing")
            {
                host.exit_after_update();
            }
            Ok(result)
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Adapter {
        calls: Mutex<Vec<String>>,
        fail: bool,
        cancel_picker: bool,
    }
    impl Adapter {
        fn record(&self, action: String) -> Result<(), Error> {
            self.calls.lock().unwrap().push(action);
            if self.fail {
                Err(Error::io("OS operation failed"))
            } else {
                Ok(())
            }
        }
    }
    impl Host for Adapter {
        async fn copy(&self, text: String) -> Result<(), Error> {
            self.record(format!("copy:{text}"))
        }
        async fn open_url(&self, url: String) -> Result<(), Error> {
            self.record(format!("url:{url}"))
        }
        fn window(&self, action: WindowAction) -> Result<(), Error> {
            self.record(format!("window:{action:?}"))
        }
        fn report_window(&self, state: UiState) -> Result<(), Error> {
            self.record(format!(
                "state:{}/{}/{}",
                state.busy, state.dirty, state.sensitive
            ))
        }
        fn request_notifications(&self) -> Result<(), Error> {
            self.record("permission".into())
        }
        async fn pick_file(&self, kind: FileKind) -> Result<Option<PathBuf>, Error> {
            self.record(format!("picker:{kind:?}"))?;
            Ok((!self.cancel_picker).then(|| PathBuf::from("/picked/file")))
        }
        fn exit_after_update(&self) {
            self.calls.lock().unwrap().push("exit".into());
        }
    }
    #[derive(Default)]
    struct Service {
        preferences: Mutex<Value>,
        fail: bool,
        installing: bool,
    }
    impl Services for Service {
        async fn preferences(&self) -> Value {
            self.preferences.lock().unwrap().clone()
        }
        async fn command(&self, action: &str, args: Value) -> Result<Value, Error> {
            if self.fail {
                return Err(Error::io("business failure"));
            }
            match action {
                "savePreferences" => {
                    *self.preferences.lock().unwrap() = args.clone();
                    Ok(args)
                }
                "installUpdate" => Ok(json!({
                    "phase": if self.installing { "installing" } else { "idle" }
                })),
                "updateStatus" => Ok(json!({"phase":"idle"})),
                _ => Err(Error::invalid("unknown operation")),
            }
        }
    }
    #[test]
    fn only_main_packaged_origins_have_native_authority() {
        let local = Url::parse("tauri://localhost/").unwrap();
        authorize("main", &local).unwrap();
        assert_eq!(authorize("remote", &local).unwrap_err().status, 403);
        assert_eq!(
            authorize("main", &Url::parse("https://example.com").unwrap())
                .unwrap_err()
                .code,
            "untrusted_window"
        );
    }
    #[tokio::test]
    async fn native_operations_are_typed_bounded_and_await_observable_results() {
        let services = Service {
            preferences: Mutex::new(json!({})),
            installing: true,
            ..Default::default()
        };
        let host = Adapter::default();
        assert_eq!(
            command(&services, &host, "copy", json!({"text":"中文"}))
                .await
                .unwrap(),
            Value::Null
        );
        command(
            &services,
            &host,
            "openUrl",
            json!({"url":"https://rustfrp.com"}),
        )
        .await
        .unwrap();
        for action in ["minimize", "hide", "show", "close"] {
            command(&services, &host, "window", json!({"action":action}))
                .await
                .unwrap();
        }
        command(
            &services,
            &host,
            "windowState",
            json!({"busy":true,"dirty":false,"sensitive":true}),
        )
        .await
        .unwrap();
        assert_eq!(
            command(&services, &host, "notifications", json!({}))
                .await
                .unwrap()["enabled"],
            true
        );
        command(
            &services,
            &host,
            "setNotifications",
            json!({"enabled":false}),
        )
        .await
        .unwrap();
        assert_eq!(
            command(&services, &host, "notifications", json!({}))
                .await
                .unwrap()["enabled"],
            false
        );
        assert!(!host.calls.lock().unwrap().contains(&"permission".into()));
        command(
            &services,
            &host,
            "setNotifications",
            json!({"enabled":true}),
        )
        .await
        .unwrap();
        for kind in ["config", "certificate", "agent", "directory"] {
            assert_eq!(
                command(&services, &host, "pickFile", json!({"kind":kind}))
                    .await
                    .unwrap(),
                json!("/picked/file")
            );
        }
        let cancel = Adapter {
            cancel_picker: true,
            ..Default::default()
        };
        assert_eq!(
            command(&services, &cancel, "pickFile", json!({}))
                .await
                .unwrap(),
            Value::Null
        );
        assert_eq!(
            command(&services, &host, "updateStatus", json!({}))
                .await
                .unwrap()["phase"],
            "idle"
        );
        assert!(!host.calls.lock().unwrap().contains(&"exit".into()));
        command(&services, &host, "installUpdate", json!({}))
            .await
            .unwrap();
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls.first().unwrap(), "copy:中文");
        assert!(calls.contains(&"url:https://rustfrp.com/".into()));
        assert!(calls.contains(&"state:true/false/true".into()));
        assert!(calls.contains(&"permission".into()));
        assert_eq!(calls.last().unwrap(), "exit");
    }
    #[tokio::test]
    async fn failures_and_bad_arguments_never_report_success_or_trigger_update_exit() {
        let services = Service::default();
        let host = Adapter::default();
        for (action, args) in [
            ("copy", json!([])),
            ("copy", json!({"text":true})),
            ("copy", json!({"text":"x".repeat(1024 * 1024 + 1)})),
            ("copy", json!({"text":"x".repeat(2 * 1024 * 1024)})),
            ("openUrl", json!({})),
            ("openUrl", json!({"url":"file:///etc/passwd"})),
            ("window", json!({"action":"raw"})),
            ("windowState", json!({"busy":"yes"})),
            ("setNotifications", json!({"enabled":"true"})),
            ("pickFile", json!({"kind":"raw"})),
            ("rawShell", json!({})),
        ] {
            assert_eq!(
                command(&services, &host, action, args)
                    .await
                    .unwrap_err()
                    .code,
                "invalid_request"
            );
        }
        assert!(host.calls.lock().unwrap().is_empty());
        let failed = Adapter {
            fail: true,
            ..Default::default()
        };
        for (action, args) in [
            ("copy", json!({"text":"x"})),
            ("openUrl", json!({"url":"https://example.com"})),
            ("window", json!({"action":"show"})),
            (
                "windowState",
                json!({"busy":false,"dirty":false,"sensitive":false}),
            ),
            ("setNotifications", json!({"enabled":true})),
            ("pickFile", json!({"kind":"agent"})),
        ] {
            assert_eq!(
                command(&services, &failed, action, args)
                    .await
                    .unwrap_err()
                    .code,
                "local_io"
            );
        }
        let failed_service = Service {
            fail: true,
            ..Default::default()
        };
        assert!(command(
            &failed_service,
            &host,
            "setNotifications",
            json!({"enabled":false})
        )
        .await
        .is_err());
        assert!(command(&failed_service, &host, "installUpdate", json!({}))
            .await
            .is_err());
        assert!(host.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_exit_is_requested_only_after_the_installer_takes_over() {
        let services = Service::default();
        let host = Adapter::default();
        assert_eq!(
            command(&services, &host, "installUpdate", json!({}))
                .await
                .unwrap()["phase"],
            "idle"
        );
        assert!(!host.calls.lock().unwrap().contains(&"exit".into()));
    }
}
