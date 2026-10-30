//! Test-only native integration driver; not compiled into production builds.
use super::*;
use rustfrp_desktop_support::Error;
use std::io::Write;
use tauri::WebviewWindow;

#[tauri::command]
pub async fn native_window_cycle(window: WebviewWindow, delay_ms: u64) -> Result<(), Error> {
    native_calls::trusted(&window)?;
    if delay_ms > 51_000 {
        return Err(Error::invalid("本机 E2E 窗口等待时间超出限制"));
    }
    native_host::window(&window, rustfrp_desktop_support::bridge::WindowAction::Hide)?;
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    native_host::window(&window, rustfrp_desktop_support::bridge::WindowAction::Show)?;
    Ok(())
}

#[tauri::command]
pub async fn native_window_visibility(window: WebviewWindow) -> Result<bool, Error> {
    native_calls::trusted(&window)?;
    window
        .is_visible()
        .map_err(Error::io_context("读取窗口状态失败"))
}

#[tauri::command]
pub async fn native_test_tray_policy(window: WebviewWindow) -> Result<Value, Error> {
    native_calls::trusted(&window)?;
    Ok(json!({
        "show":tray_menu_action("show") == Some(TrayMenuAction::Show),
        "exit":tray_menu_action("exit") == Some(TrayMenuAction::Exit),
        "unknown":tray_menu_action("unknown").is_none(),
        "left_release":is_primary_tray_release(tauri::tray::MouseButton::Left, tauri::tray::MouseButtonState::Up),
        "right_release":is_primary_tray_release(tauri::tray::MouseButton::Right, tauri::tray::MouseButtonState::Up),
    }))
}

#[tauri::command]
pub async fn native_test_config(window: WebviewWindow) -> Result<Value, Error> {
    native_calls::trusted(&window)?;
    let variable = |name| std::env::var(name).map_err(|_| Error::invalid("本机 E2E 参数缺失"));
    Ok(
        json!({"controller":"http://127.0.0.1:9674","adminToken":variable("RUSTFRP_E2E_ADMIN_TOKEN")?,"token":variable("RUSTFRP_NATIVE_E2E_TOKEN")?,"name":variable("RUSTFRP_NATIVE_E2E_NAME")?,"mode":std::env::var("RUSTFRP_NATIVE_E2E_MODE").unwrap_or_else(|_| "full".into())}),
    )
}
#[tauri::command]
pub async fn native_test_report(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Runtime>>,
    result: Value,
) -> Result<(), Error> {
    native_calls::trusted(&window)?;
    let stage = result
        .get("stage")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("E2E 阶段缺失"))?;
    if !matches!(
        stage,
        "local-started"
            | "authenticated"
            | "window-cycle-start"
            | "window-cycles"
            | "warm-cycles"
            | "sensitive-protected"
            | "release"
            | "restored"
            | "isolated-startup"
            | "home-startup"
            | "no-tray-startup"
            | "failed"
    ) || result.to_string().len() > 4096
    {
        return Err(Error::invalid("E2E 报告不符合约定"));
    }
    let path = std::env::var_os("RUSTFRP_NATIVE_E2E_REPORT")
        .map(PathBuf::from)
        .ok_or_else(|| Error::invalid("E2E 输出缺失"))?;
    if !path.is_absolute() {
        return Err(Error::invalid("E2E 输出须为绝对路径"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid("E2E 输出无效"))?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| Error::io("E2E 报告创建失败"))?;
    file.write_all(result.to_string().as_bytes())
        .map_err(|_| Error::io("E2E 报告写入失败"))?;
    use rustfrp_desktop_support::config::Replace;
    let file = file.into_temp_path();
    rustfrp_desktop_support::config::AtomicReplace
        .apply(&file, &path)
        .map_err(|_| Error::io("E2E 报告发布失败"))?;
    println!("Native WebView: {stage}");
    if stage == "release" {
        state.test_release.store(true, Ordering::Relaxed);
    }
    if matches!(
        stage,
        "restored" | "isolated-startup" | "home-startup" | "no-tray-startup" | "failed"
    ) {
        window.app_handle().exit(i32::from(stage == "failed"));
    }
    Ok(())
}
