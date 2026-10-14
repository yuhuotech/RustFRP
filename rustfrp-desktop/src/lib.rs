//! System WebView and OS adapters; IPC business behavior lives in the support crate.
#![forbid(unsafe_code)]
use rustfrp_desktop_support::{
    controller::RequestArgs, lifecycle, runtime::Runtime as Services, Error,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
#[cfg(feature = "native-e2e")]
mod native_e2e;

struct Runtime {
    services: Services,
    clipboard: std::sync::Mutex<Option<arboard::Clipboard>>,
    picker: tokio::sync::Semaphore,
    window: std::sync::Mutex<rustfrp_desktop_support::window::WindowPolicy>,
    clock: Instant,
    tray_active: AtomicBool,
    #[cfg(feature = "native-e2e")]
    test_release: AtomicBool,
}

fn main_window(app: &tauri::AppHandle) -> Result<WebviewWindow, Box<dyn std::error::Error>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == "main")
        .ok_or("主窗口配置缺失")?;
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
fn show_main(app: &tauri::AppHandle) {
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
fn tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
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
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "exit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                tauri::tray::TrayIconEvent::Click {
                    button: tauri::tray::MouseButton::Left,
                    button_state: tauri::tray::MouseButtonState::Up,
                    ..
                }
            ) {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
fn trusted(window: &WebviewWindow) -> Result<(), Error> {
    let url = window
        .url()
        .map_err(|_| Error::invalid("无法验证窗口来源"))?;
    if window.label() != "main" || !rustfrp_desktop_support::trusted_origin(&url) {
        return Err(Error {
            status: 403,
            code: "untrusted_window".into(),
            message: "该窗口无权调用桌面能力".into(),
            uncertain: false,
        });
    }
    Ok(())
}
#[tauri::command]
async fn controller_request(
    window: WebviewWindow,
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
async fn controller_cancel(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Runtime>>,
    request_id: String,
) -> Result<Value, Error> {
    trusted(&window)?;
    Ok(json!({"cancelled":state.services.controller.cancel(&request_id)?}))
}
#[tauri::command]
async fn host_command(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Runtime>>,
    action: String,
    args: Value,
) -> Result<Value, Error> {
    trusted(&window)?;
    if !args.is_object()
        || serde_json::to_vec(&args)
            .map_err(|_| Error::invalid("桌面参数无效"))?
            .len()
            > 2 * 1024 * 1024
    {
        return Err(Error::invalid("桌面参数超过限制"));
    }
    match action.as_str() {
        "copy" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("复制内容须为文本"))?;
            if text.len() > 1024 * 1024 {
                return Err(Error::invalid("复制内容超过 1 MiB"));
            }
            let text = text.to_string();
            let runtime = state.inner().clone();
            tokio::task::spawn_blocking(move || {
                let mut clipboard = runtime
                    .clipboard
                    .lock()
                    .map_err(|_| Error::io("剪贴板状态不可用"))?;
                if clipboard.is_none() {
                    *clipboard = Some(
                        arboard::Clipboard::new().map_err(|_| Error::io("无法连接系统剪贴板"))?,
                    );
                }
                clipboard
                    .as_mut()
                    .ok_or_else(|| Error::io("剪贴板不可用"))?
                    .set_text(text)
                    .map_err(|_| Error::io("无法写入剪贴板，请手动选择并复制文本"))
            })
            .await
            .map_err(|_| Error::io("剪贴板任务失败"))??;
            Ok(Value::Null)
        }
        "openUrl" => {
            let url = rustfrp_desktop_support::external_url(
                args.get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("缺少链接"))?,
            )?;
            tokio::task::spawn_blocking(move || open::that_detached(url))
                .await
                .map_err(|_| Error::io("打开链接任务失败"))?
                .map_err(|_| Error::io("无法打开系统浏览器"))?;
            Ok(Value::Null)
        }
        "window" => {
            match args.get("action").and_then(Value::as_str) {
                Some("minimize") => window.minimize(),
                Some("hide") => window.hide(),
                Some("show") => window
                    .show()
                    .and_then(|()| window.unminimize())
                    .and_then(|()| window.set_focus()),
                Some("close") => window.close(),
                _ => return Err(Error::invalid("不允许的窗口操作")),
            }
            .map_err(|_| Error::io("窗口操作失败"))?;
            Ok(Value::Null)
        }
        "windowState" => {
            let ui: rustfrp_desktop_support::window::UiState =
                serde_json::from_value(args).map_err(|_| Error::invalid("窗口状态无效"))?;
            state
                .window
                .lock()
                .map_err(|_| Error::io("窗口状态不可用"))?
                .report(ui);
            Ok(Value::Null)
        }
        "notifications" => Ok(
            json!({"enabled":state.services.preferences().await.get("notifications").and_then(Value::as_bool).unwrap_or(true),"system_managed":true}),
        ),
        "setNotifications" => {
            let enabled = args
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::invalid("通知设置须为布尔值"))?;
            if enabled {
                window
                    .app_handle()
                    .notification()
                    .request_permission()
                    .map_err(|_| Error::io("无法请求系统通知权限"))?;
            }
            state
                .services
                .command("savePreferences", json!({"notifications":enabled}))
                .await?;
            Ok(json!({"enabled":enabled,"system_managed":true}))
        }
        "pickFile" => {
            let _permit = state
                .picker
                .try_acquire()
                .map_err(|_| Error::invalid("文件选择窗口已经打开"))?;
            let kind = args.get("kind").and_then(Value::as_str).unwrap_or("config");
            let (send, receive) = tokio::sync::oneshot::channel();
            let dialog = window.app_handle().dialog().file().set_parent(&window);
            match kind {
                "config" => dialog
                    .add_filter("frp 配置", &["toml", "yaml", "yml", "json", "ini"])
                    .pick_file(move |path| {
                        let _ = send.send(path);
                    }),
                "certificate" => dialog
                    .add_filter("CA 证书", &["pem", "crt", "cer"])
                    .pick_file(move |path| {
                        let _ = send.send(path);
                    }),
                "agent" => dialog.pick_file(move |path| {
                    let _ = send.send(path);
                }),
                "directory" => dialog.pick_folder(move |path| {
                    let _ = send.send(path);
                }),
                _ => return Err(Error::invalid("不支持的文件选择类型")),
            }
            let path = receive.await.map_err(|_| Error::io("文件选择任务失败"))?;
            path.map(|path| {
                path.into_path()
                    .map(|path| json!(path))
                    .map_err(|_| Error::invalid("请选择本机文件路径"))
            })
            .transpose()
            .map(|path| path.unwrap_or(Value::Null))
        }
        _ => {
            let result = state.services.command(&action, args).await?;
            if action == "installUpdate"
                && result.get("phase").and_then(Value::as_str) == Some("installing")
            {
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    app.exit(0);
                });
            }
            Ok(result)
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new("--agent-supervisor")) {
        let path = arguments.next().ok_or("后台服务文件缺失")?;
        if arguments.next().is_some() {
            return Err("后台服务参数无效".into());
        }
        lifecycle::run_supervisor(&PathBuf::from(path))?;
        return Ok(());
    }
    let startup_hidden = std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--startup-to-tray");
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_stack_size(256 * 1024)
        .enable_all()
        .build()?;
    tauri::async_runtime::set(async_runtime.handle().clone());
    let isolated = std::env::var_os("RUSTFRP_DESKTOP_STATE_DIR").is_some();
    let root = if let Some(path) = std::env::var_os("RUSTFRP_DESKTOP_STATE_DIR") {
        let root = PathBuf::from(path);
        if !root.is_absolute() {
            return Err("桌面状态目录必须是绝对路径".into());
        }
        root
    } else {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .ok_or("无法定位当前用户状态目录")?
            .join(".rustfrp/native-desktop")
    };
    if !isolated {
        rustfrp_desktop_support::migration::current(&root)?;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main(app)
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let resources = app.path().resource_dir()?;
            let executable = std::env::current_exe()?;
            let services = Services::new(
                lifecycle::System {
                    root: root.clone(),
                    shell: executable.clone(),
                    bundled_agent: Some(lifecycle::bundled_agent(
                        &executable,
                        &resources,
                        rustfrp_desktop_support::startup::Platform::current(),
                    )),
                },
                env!("CARGO_PKG_VERSION"),
            )?;
            app.manage(Arc::new(Runtime {
                services,
                clipboard: std::sync::Mutex::new(None),
                picker: tokio::sync::Semaphore::new(1),
                window: std::sync::Mutex::new(Default::default()),
                clock: Instant::now(),
                tray_active: AtomicBool::new(false),
                #[cfg(feature = "native-e2e")]
                test_release: AtomicBool::new(false),
            }));
            let window = main_window(app.handle())?;
            let state = app.state::<Arc<Runtime>>().inner().clone();
            let updates = state.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(12)).await;
                loop {
                    if updates.services.updates.status().disabled.is_none()
                        && !updates.services.updates.busy()
                    {
                        let prefs = updates.services.preferences().await;
                        let source = prefs
                            .get("update_source")
                            .and_then(Value::as_str)
                            .unwrap_or("github");
                        let _ = updates.services.updates.check_native(source).await;
                    }
                    tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
                }
            });
            let has_tray = tray(app).is_ok();
            state.tray_active.store(has_tray, Ordering::Relaxed);
            if startup_hidden && has_tray {
                window.hide()?;
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut connection =
                    rustfrp_desktop_support::window::ConnectionNotifications::default();
                let mut tick = 0_u8;
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let prefs = state.services.preferences().await;
                    if let Some(window) = handle.get_webview_window("main") {
                        if let Ok(mut policy) = state.window.lock() {
                            if window.is_visible().unwrap_or(true) {
                                policy.shown();
                            } else {
                                policy.hidden(state.clock.elapsed());
                            }
                            if state.tray_active.load(Ordering::Relaxed)
                                && state.services.keep_in_tray()
                                && policy.can_release(
                                    state.clock.elapsed(),
                                    prefs
                                        .get("lightweight")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(true),
                                    !state.services.idle() || state.services.controller.busy(),
                                )
                                && window.destroy().is_ok()
                            {
                                policy.destroyed();
                            }
                        }
                    }
                    tick = (tick + 1) % 5;
                    #[cfg(feature = "native-e2e")]
                    if handle.get_webview_window("main").is_none()
                        && state.test_release.swap(false, Ordering::Relaxed)
                    {
                        show_main(&handle);
                    }
                    if tick == 0 {
                        let current = state
                            .services
                            .command("localStatus", json!({}))
                            .await
                            .ok()
                            .and_then(|status| status.get("connected").and_then(Value::as_bool));
                        if let Some(connected) = connection.observe(
                            current,
                            prefs
                                .get("notifications")
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                        ) {
                            let _ = handle
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
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.app_handle().state::<Arc<Runtime>>();
                if state.services.keep_in_tray() && state.tray_active.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler({
            #[cfg(not(feature = "native-e2e"))]
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool =
                tauri::generate_handler![controller_request, controller_cancel, host_command];
            #[cfg(feature = "native-e2e")]
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                controller_request,
                controller_cancel,
                host_command,
                native_e2e::native_test_config,
                native_e2e::native_test_report
            ];
            handler
        })
        .build(tauri::generate_context!())?
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    if let Some(state) = app.try_state::<Arc<Runtime>>() {
                        if state.services.keep_in_tray()
                            && state.tray_active.load(Ordering::Relaxed)
                        {
                            api.prevent_exit();
                        }
                    }
                }
            }
        });
    Ok(())
}
