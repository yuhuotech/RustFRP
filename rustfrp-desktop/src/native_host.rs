//! Testable window policy calls used by the Tauri system adapter.
use super::Runtime;
use rustfrp_desktop_support::{bridge::WindowAction, window::UiState, Error};
#[cfg(target_os = "macos")]
use std::time::Duration;
use tauri::{Runtime as TauriRuntime, WebviewWindow};

pub fn window<R: TauriRuntime>(
    window: &WebviewWindow<R>,
    action: WindowAction,
) -> Result<(), Error> {
    match action {
        WindowAction::Minimize => window.minimize(),
        WindowAction::Hide => window.hide(),
        WindowAction::Show => {
            window.show().map_err(Error::io_context("窗口显示失败"))?;
            window
                .unminimize()
                .map_err(Error::io_context("窗口恢复失败"))?;
            #[cfg(target_os = "macos")]
            {
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                window
                    .run_on_main_thread(move || {
                        let activated = if let Some(marker) = objc2::MainThreadMarker::new() {
                            #[allow(deprecated)]
                            objc2_app_kit::NSApp(marker).activateIgnoringOtherApps(true);
                            true
                        } else {
                            false
                        };
                        let _ = sender.send(activated);
                    })
                    .map_err(Error::io_context("应用激活请求失败"))?;
                match receiver.recv_timeout(Duration::from_secs(2)) {
                    Ok(true) => {}
                    Ok(false) => return Err(Error::io("无法在主线程激活应用")),
                    Err(_) => return Err(Error::io("等待应用激活超时")),
                }
            }
            window.set_focus()
        }
        WindowAction::Close => window.close(),
    }
    .map_err(Error::io_context("窗口操作失败"))
}

pub fn report_window(runtime: &Runtime, state: UiState) -> Result<(), Error> {
    runtime
        .window
        .lock()
        .map_err(Error::io_context("窗口状态不可用"))?
        .report(state);
    Ok(())
}
