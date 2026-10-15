//! Native OS and Tauri plugin calls behind the shared typed bridge.
use super::{native_host, Runtime};
use rustfrp_desktop_support::{
    bridge::{FileKind, Host, WindowAction},
    controller::RequestArgs,
    window::{should_keep_window, UiState},
    Error,
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tauri::{Manager, WebviewWindow, Window};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;

pub struct NativeHost<R: tauri::Runtime = tauri::Wry> {
    pub window: WebviewWindow<R>,
    pub runtime: Arc<Runtime>,
}

pub(super) fn trusted<R: tauri::Runtime>(window: &WebviewWindow<R>) -> Result<(), Error> {
    let url = window
        .url()
        .map_err(Error::invalid_context("无法验证窗口来源"))?;
    rustfrp_desktop_support::bridge::authorize(window.label(), &url)
}

#[tauri::command]
pub(super) async fn controller_request<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    state: tauri::State<'_, Arc<Runtime>>,
    operation: String,
    args: RequestArgs,
    request_id: String,
) -> Result<Value, Error> {
    trusted(&window)?;
    state
        .services
        .controller
        .request_tracked(request_id, &operation, args)
        .await
}

#[tauri::command]
pub(super) async fn controller_cancel<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    state: tauri::State<'_, Arc<Runtime>>,
    request_id: String,
) -> Result<Value, Error> {
    trusted(&window)?;
    Ok(serde_json::json!({"cancelled":state.services.controller.cancel(&request_id)?}))
}

#[tauri::command]
pub(super) async fn host_command<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    state: tauri::State<'_, Arc<Runtime>>,
    action: String,
    args: Value,
) -> Result<Value, Error> {
    trusted(&window)?;
    let host = NativeHost {
        window,
        runtime: state.inner().clone(),
    };
    rustfrp_desktop_support::bridge::command(&state.services, &host, &action, args).await
}

pub fn main_window<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<WebviewWindow<R>, Box<dyn std::error::Error>> {
    let config = super::main_window_config(&app.config().app.windows)?;
    let builder = tauri::WebviewWindowBuilder::from_config(app, config)?
        .on_navigation(rustfrp_desktop_support::trusted_origin)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny);
    #[cfg(feature = "native-e2e")]
    let builder = builder.on_page_load(|window, payload| {
        if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
            let _ = window.eval(include_str!("native_e2e.js"));
        }
    });
    Ok(builder.build()?)
}

pub fn show_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(state) = app.try_state::<Arc<Runtime>>() {
        if let Ok(mut policy) = state.window.lock() {
            policy.shown();
        }
    }
    if let Some(window) = app
        .get_webview_window("main")
        .or_else(|| main_window(app).ok())
    {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn monitor_window<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    runtime: &Runtime,
    preferences: &Value,
) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(mut policy) = runtime.window.lock() else {
        return;
    };
    let now = runtime.clock.elapsed();
    if window.is_visible().unwrap_or(true) {
        policy.shown();
    } else {
        policy.hidden(now);
    }
    let lightweight = preferences
        .get("lightweight")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let host_busy = !runtime.services.idle() || runtime.services.controller.busy();
    if policy.should_release(
        now,
        lightweight,
        host_busy,
        runtime.tray_active.load(Ordering::Relaxed),
        runtime.services.keep_in_tray(),
    ) && window.destroy().is_ok()
    {
        policy.destroyed();
    }
}

pub fn on_window_event<R: tauri::Runtime>(window: &Window<R>, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let state = window.app_handle().state::<Arc<Runtime>>();
        if should_keep_window(
            state.services.keep_in_tray(),
            state.tray_active.load(Ordering::Relaxed),
        ) {
            api.prevent_close();
            let _ = window.hide();
        }
    }
}

pub fn run_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::RunEvent) {
    if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
        if code.is_none() {
            if let Some(state) = app.try_state::<Arc<Runtime>>() {
                if should_keep_window(
                    state.services.keep_in_tray(),
                    state.tray_active.load(Ordering::Relaxed),
                ) {
                    api.prevent_exit();
                }
            }
        }
    }
}

pub fn connection_notification<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    transition: Option<bool>,
) {
    if let Some(connected) = transition {
        let _ = app
            .notification()
            .builder()
            .title(if connected {
                "RustFRP 已连接"
            } else {
                "RustFRP 连接断开"
            })
            .body(if connected {
                "本机隧道已恢复。"
            } else {
                "本机 Agent 与节点失去连接。"
            })
            .show();
    }
}

impl<R: tauri::Runtime> Host for NativeHost<R> {
    async fn copy(&self, text: String) -> Result<(), Error> {
        let runtime = self.runtime.clone();
        tokio::task::spawn_blocking(move || {
            let mut clipboard = runtime
                .clipboard
                .lock()
                .map_err(Error::io_context("剪贴板状态不可用"))?;
            if clipboard.is_none() {
                *clipboard = Some(
                    arboard::Clipboard::new().map_err(Error::io_context("无法连接系统剪贴板"))?,
                );
            }
            clipboard
                .as_mut()
                .ok_or_else(|| Error::io("剪贴板不可用"))?
                .set_text(text)
                .map_err(Error::io_context("无法写入剪贴板，请手动选择并复制文本"))
        })
        .await
        .map_err(Error::io_context("剪贴板任务失败"))?
    }

    async fn open_url(&self, url: String) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || open::that_detached(url))
            .await
            .map_err(Error::io_context("打开链接任务失败"))?
            .map_err(Error::io_context("无法打开系统浏览器"))
    }

    fn window(&self, action: WindowAction) -> Result<(), Error> {
        native_host::window(&self.window, action)
    }

    fn report_window(&self, state: UiState) -> Result<(), Error> {
        native_host::report_window(&self.runtime, state)
    }

    fn request_notifications(&self) -> Result<(), Error> {
        self.window
            .app_handle()
            .notification()
            .request_permission()
            .map(|_| ())
            .map_err(Error::io_context("无法请求系统通知权限"))
    }

    async fn pick_file(&self, kind: FileKind) -> Result<Option<PathBuf>, Error> {
        let _permit = self
            .runtime
            .picker
            .try_acquire()
            .map_err(Error::invalid_context("文件选择窗口已经打开"))?;
        let (send, receive) = tokio::sync::oneshot::channel();
        let dialog = self
            .window
            .app_handle()
            .dialog()
            .file()
            .set_parent(&self.window);
        match kind {
            FileKind::Config => dialog
                .add_filter("frp 配置", &["toml", "yaml", "yml", "json", "ini"])
                .pick_file(move |path| {
                    let _ = send.send(path);
                }),
            FileKind::Certificate => dialog
                .add_filter("CA 证书", &["pem", "crt", "cer"])
                .pick_file(move |path| {
                    let _ = send.send(path);
                }),
            FileKind::Agent => dialog.pick_file(move |path| {
                let _ = send.send(path);
            }),
            FileKind::Directory => dialog.pick_folder(move |path| {
                let _ = send.send(path);
            }),
        }
        receive
            .await
            .map_err(Error::io_context("文件选择任务失败"))?
            .map(|path| {
                path.into_path()
                    .map_err(Error::invalid_context("请选择本机文件路径"))
            })
            .transpose()
    }

    fn exit_after_update(&self) {
        let app = self.window.app_handle().clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            app.exit(0);
        });
    }
}

pub fn tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "native-e2e")]
    if std::env::var_os("RUSTFRP_NATIVE_E2E_NO_TRAY").is_some() {
        return Err("tray disabled by native E2E".into());
    }
    let show = tauri::menu::MenuItem::with_id(app, "show", "显示 RustFRP", true, None::<&str>)?;
    let exit = tauri::menu::MenuItem::with_id(
        app,
        "exit",
        "退出应用（Agent 继续运行）",
        true,
        None::<&str>,
    )?;
    let menu = tauri::menu::Menu::with_items(app, &[&show, &exit])?;
    let mut builder = tauri::tray::TrayIconBuilder::with_id("rustfrp")
        .tooltip("RustFRP")
        .menu(&menu);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder
        .on_menu_event(|app, event| {
            match super::tray_menu_action(event.id.as_ref()) {
                Some(super::TrayMenuAction::Show) => show_main(app),
                Some(super::TrayMenuAction::Exit) => app.exit(0),
                None => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(event, tauri::tray::TrayIconEvent::Click { button, button_state, .. } if super::is_primary_tray_release(button, button_state)) {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
