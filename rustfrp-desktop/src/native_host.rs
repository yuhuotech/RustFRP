//! Testable window policy calls used by the Tauri system adapter.
use super::Runtime;
use rustfrp_desktop_support::{bridge::WindowAction, window::UiState, Error};
use tauri::{Runtime as TauriRuntime, WebviewWindow};

pub fn window<R: TauriRuntime>(
    window: &WebviewWindow<R>,
    action: WindowAction,
) -> Result<(), Error> {
    match action {
        WindowAction::Minimize => window.minimize(),
        WindowAction::Hide => window.hide(),
        WindowAction::Show => window
            .show()
            .and_then(|()| window.unminimize())
            .and_then(|()| window.set_focus()),
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
