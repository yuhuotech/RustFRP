//! System WebView and OS adapters; IPC business behavior lives in the support crate.
#![forbid(unsafe_code)]
#![cfg_attr(coverage, feature(coverage_attribute))]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
use rustfrp_desktop_support::{lifecycle, runtime::Runtime as Services};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::Manager;
mod native_calls;
#[cfg(feature = "native-e2e")]
mod native_e2e;
mod native_host;
#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests;

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

fn main_window_config(
    windows: &[tauri::utils::config::WindowConfig],
) -> Result<&tauri::utils::config::WindowConfig, &'static str> {
    windows
        .iter()
        .find(|config| config.label == "main")
        .ok_or("主窗口配置缺失")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayMenuAction {
    Show,
    Exit,
}

fn tray_menu_action(action: &str) -> Option<TrayMenuAction> {
    match action {
        "show" => Some(TrayMenuAction::Show),
        "exit" => Some(TrayMenuAction::Exit),
        _ => None,
    }
}
fn is_primary_tray_release(
    button: tauri::tray::MouseButton,
    state: tauri::tray::MouseButtonState,
) -> bool {
    button == tauri::tray::MouseButton::Left && state == tauri::tray::MouseButtonState::Up
}

#[cfg(feature = "native-e2e")]
fn should_restore_native_test_window(window_missing: bool, test_release: &AtomicBool) -> bool {
    window_missing && test_release.swap(false, Ordering::Relaxed)
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
            native_calls::show_main(app)
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let resources = app.path().resource_dir()?;
            let executable = std::env::current_exe()?;
            let system = lifecycle::System {
                root: root.clone(),
                shell: executable.clone(),
                bundled_agent: Some(lifecycle::bundled_agent(
                    &executable,
                    &resources,
                    rustfrp_desktop_support::startup::Platform::current(),
                )),
            };
            let version =
                option_env!("RUSTFRP_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
            let services = if isolated {
                Services::with_startup(
                    system,
                    version,
                    rustfrp_desktop_support::startup::Startup::isolated(
                        root.join("startup-home"),
                        executable,
                    ),
                )?
            } else {
                Services::new(system, version)?
            };
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
            let window = native_calls::main_window(app.handle())?;
            let state = app.state::<Arc<Runtime>>().inner().clone();
            let updates = state.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(12)).await;
                loop {
                    let prefs = updates.services.preferences().await;
                    let source = prefs
                        .get("update_source")
                        .and_then(Value::as_str)
                        .unwrap_or("github");
                    updates.services.updates.check_native_if_idle(source).await;
                    tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
                }
            });
            let has_tray = native_calls::tray(app).is_ok();
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
                    native_calls::monitor_window(&handle, &state, &prefs);
                    tick = (tick + 1) % 5;
                    #[cfg(feature = "native-e2e")]
                    if should_restore_native_test_window(
                        handle.get_webview_window("main").is_none(),
                        &state.test_release,
                    ) {
                        native_calls::show_main(&handle);
                    }
                    if tick == 0 {
                        let current = state
                            .services
                            .command("localStatus", json!({}))
                            .await
                            .ok()
                            .and_then(|status| status.get("connected").and_then(Value::as_bool));
                        let transition = connection.observe(
                            current,
                            prefs
                                .get("notifications")
                                .and_then(Value::as_bool)
                                .unwrap_or(true),
                        );
                        native_calls::connection_notification(&handle, transition);
                    }
                }
            });
            Ok(())
        })
        .on_window_event(native_calls::on_window_event)
        .invoke_handler({
            #[cfg(not(feature = "native-e2e"))]
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                native_calls::controller_request,
                native_calls::controller_cancel,
                native_calls::host_command
            ];
            #[cfg(feature = "native-e2e")]
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                native_calls::controller_request,
                native_calls::controller_cancel,
                native_calls::host_command,
                native_e2e::native_test_config,
                native_e2e::native_test_report,
                native_e2e::native_test_tray_policy,
                native_e2e::native_window_cycle,
                native_e2e::native_window_visibility
            ];
            handler
        })
        .build(tauri::generate_context!())?
        .run(native_calls::run_event);
    Ok(())
}
