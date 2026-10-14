//! E2E adapter exercises the exact portable commands used by the trusted WebView.
use rustfrp_desktop_support::{lifecycle::System, runtime::Runtime};
use serde_json::{json, Value};
use std::{io::Read, path::PathBuf};

async fn probe(input: Value) -> Result<Value, Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        input
            .get("root")
            .and_then(Value::as_str)
            .ok_or("missing isolated root")?,
    );
    if !root.is_absolute() {
        return Err("root must be absolute".into());
    }
    let runtime = Runtime::new(
        System {
            root,
            shell: PathBuf::from(
                input
                    .get("shell")
                    .and_then(Value::as_str)
                    .ok_or("missing shell")?,
            ),
            bundled_agent: input
                .get("bundle")
                .and_then(Value::as_str)
                .map(PathBuf::from),
        },
        "e2e",
    )?;
    if let Some(local) = input.get("local") {
        runtime
            .command("savePreferences", json!({"local":local}))
            .await?;
    }
    match input.get("action").and_then(Value::as_str) {
        Some("start") => {
            runtime
                .command(
                    "localStart",
                    json!({"enrollToken":input.get("token").and_then(Value::as_str).unwrap_or("")}),
                )
                .await?;
        }
        Some("stop") => {
            runtime.command("localStop", json!({})).await?;
        }
        Some("read") => {}
        _ => return Err("unknown probe action".into()),
    }
    let status = runtime.command("localStatus", json!({})).await?;
    if status.is_null() {
        return Ok(json!({"status":null}));
    }
    let tunnels = runtime.command("localTunnels", json!({})).await?;
    let logs = runtime.command("localLogs", json!({})).await?;
    let tail = runtime
        .command(
            "localLogs",
            json!({"after":logs.get("cursor").and_then(Value::as_u64).unwrap_or(0)}),
        )
        .await?;
    let diagnostic = if input.get("diagnostics") == Some(&Value::Bool(true)) {
        runtime.command("exportDiagnostics", json!({})).await?
    } else {
        Value::Null
    };
    Ok(json!({"status":status,"tunnels":tunnels,"logs":logs,"tail":tail,"diagnostic":diagnostic}))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut source = String::new();
    std::io::stdin().take(16385).read_to_string(&mut source)?;
    if source.len() > 16384 {
        return Err("probe input exceeds limit".into());
    }
    let input = serde_json::from_str(&source)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(probe(input))?;
    println!("{result}");
    Ok(())
}
