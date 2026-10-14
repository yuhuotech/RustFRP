//! Test-only native integration driver; not compiled into production builds.
use super::*;
use std::io::Write;

#[tauri::command]
pub async fn native_test_config(window: WebviewWindow) -> Result<Value, Error> {
    trusted(&window)?;
    let variable = |name| std::env::var(name).map_err(|_| Error::invalid("本机 E2E 参数缺失"));
    Ok(
        json!({"controller":"http://127.0.0.1:9674","password":variable("RUSTFRP_E2E_PASSWORD")?,"token":variable("RUSTFRP_NATIVE_E2E_TOKEN")?,"name":variable("RUSTFRP_NATIVE_E2E_NAME")?}),
    )
}
#[tauri::command]
pub async fn native_test_report(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<Runtime>>,
    result: Value,
) -> Result<(), Error> {
    trusted(&window)?;
    let stage = result
        .get("stage")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("E2E 阶段缺失"))?;
    if !matches!(
        stage,
        "local-started" | "warm-cycles" | "sensitive-protected" | "release" | "restored" | "failed"
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
    if matches!(stage, "restored" | "failed") {
        window
            .app_handle()
            .exit(if stage == "restored" { 0 } else { 1 });
    }
    Ok(())
}
