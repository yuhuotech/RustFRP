fn main() -> Result<(), Box<dyn std::error::Error>> {
    let commands: &'static [&str] = if std::env::var_os("CARGO_FEATURE_NATIVE_E2E").is_some() {
        &[
            "controller_request",
            "controller_cancel",
            "host_command",
            "native_test_config",
            "native_test_report",
            "native_test_tray_policy",
            "native_window_cycle",
            "native_window_visibility",
        ]
    } else {
        &["controller_request", "controller_cancel", "host_command"]
    };
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )?;
    Ok(())
}
