use super::*;
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::WebviewWindow;

#[cfg(feature = "native-e2e")]
#[test]
fn native_test_window_restore_consumes_only_a_pending_release() {
    let pending = AtomicBool::new(true);
    assert!(!should_restore_native_test_window(false, &pending));
    assert!(pending.load(Ordering::Relaxed));

    let not_pending = AtomicBool::new(false);
    assert!(!should_restore_native_test_window(true, &not_pending));
    assert!(!not_pending.load(Ordering::Relaxed));

    assert!(should_restore_native_test_window(true, &pending));
    assert!(!pending.load(Ordering::Relaxed));
    assert!(!should_restore_native_test_window(true, &pending));
}

fn invoke(
    window: &WebviewWindow<tauri::test::MockRuntime>,
    command: &str,
    body: Value,
) -> Result<Value, Value> {
    get_ipc_response(
        window,
        tauri::webview::InvokeRequest {
            cmd: command.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: window.url().unwrap(),
            body: tauri::ipc::InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.into(),
        },
    )
    .map(|response| response.deserialize().unwrap())
}

#[test]
fn ipc_uses_typed_policy_preserves_native_errors_and_rejects_untrusted_windows() {
    let folder = tempfile::tempdir().unwrap();
    let mut startup = rustfrp_desktop_support::startup::Startup::isolated(
        folder.path().join("login"),
        std::env::current_exe().unwrap(),
    );
    startup.platform = rustfrp_desktop_support::startup::Platform::Linux;
    let services = Services::with_startup(
        lifecycle::System {
            root: folder.path().to_owned(),
            shell: std::env::current_exe().unwrap(),
            bundled_agent: None,
        },
        "ipc-test",
        startup,
    )
    .unwrap();
    let state = Arc::new(Runtime {
        services,
        clipboard: Default::default(),
        picker: tokio::sync::Semaphore::new(1),
        window: Default::default(),
        clock: Instant::now(),
        tray_active: AtomicBool::new(false),
        #[cfg(feature = "native-e2e")]
        test_release: AtomicBool::new(false),
    });
    let configured_main =
        serde_json::from_value::<tauri::utils::config::WindowConfig>(json!({"label":"main"}))
            .unwrap();
    let mut context = mock_context(noop_assets());
    context
        .config_mut()
        .app
        .windows
        .push(configured_main.clone());
    let app = mock_builder()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            native_calls::controller_request,
            native_calls::controller_cancel,
            native_calls::host_command
        ])
        .build(context)
        .unwrap();
    tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let configured =
        serde_json::from_value::<tauri::utils::config::WindowConfig>(json!({"label":"other"}))
            .unwrap();
    assert!(main_window_config(std::slice::from_ref(&configured)).is_err());
    assert_eq!(
        main_window_config(std::slice::from_ref(&configured_main))
            .unwrap()
            .label,
        "main"
    );
    native_calls::show_main(app.handle());
    assert!(app.get_webview_window("main").is_some());
    native_calls::show_main(app.handle());
    assert!(app.get_webview_window("main").is_some());
    let main = app.get_webview_window("main").unwrap();
    let mut no_state_context = mock_context(noop_assets());
    no_state_context
        .config_mut()
        .app
        .windows
        .push(configured_main);
    let no_state_app = mock_builder().build(no_state_context).unwrap();
    native_calls::show_main(no_state_app.handle());
    assert!(no_state_app.get_webview_window("main").is_some());
    assert!(is_primary_tray_release(
        tauri::tray::MouseButton::Left,
        tauri::tray::MouseButtonState::Up
    ));
    for (button, state) in [
        (
            tauri::tray::MouseButton::Left,
            tauri::tray::MouseButtonState::Down,
        ),
        (
            tauri::tray::MouseButton::Right,
            tauri::tray::MouseButtonState::Up,
        ),
    ] {
        assert!(!is_primary_tray_release(button, state));
    }
    assert_eq!(tray_menu_action("show"), Some(TrayMenuAction::Show));
    assert_eq!(tray_menu_action("exit"), Some(TrayMenuAction::Exit));
    assert_eq!(tray_menu_action("unknown"), None);
    let command =
        |action: &str, args| invoke(&main, "host_command", json!({"action":action,"args":args}));
    assert_eq!(command("info", json!({})).unwrap()["version"], "ipc-test");
    assert_eq!(command("startup", json!({})).unwrap(), false);
    assert_eq!(
        command("setStartup", json!({"enabled":true})).unwrap(),
        true
    );
    assert_eq!(command("startup", json!({})).unwrap(), true);
    assert_eq!(
        command("setStartup", json!({"enabled":false})).unwrap(),
        false
    );
    assert_eq!(
        command("copy", json!({"text":true})).unwrap_err()["code"],
        "invalid_request"
    );
    assert_eq!(
        command("setNotifications", json!({"enabled":false})).unwrap()["enabled"],
        false
    );
    assert_eq!(
        command("notifications", json!({})).unwrap()["enabled"],
        false
    );
    command(
        "windowState",
        json!({"busy":true,"dirty":false,"sensitive":true}),
    )
    .unwrap();
    {
        let mut policy = state.window.lock().unwrap();
        policy.hidden(Duration::ZERO);
        assert!(!policy.can_release(Duration::from_secs(60), true, false));
    }
    let permit = state.picker.try_acquire().unwrap();
    assert_eq!(
        command("pickFile", json!({"kind":"config"})).unwrap_err()["code"],
        "invalid_request"
    );
    drop(permit);
    // MockRuntime's native setters are no-ops; the actual state changes are checked
    // by native_window.py. Here the IPC response and typed dispatch are the contract.
    for action in ["minimize", "hide", "show"] {
        assert_eq!(
            command("window", json!({"action":action})).unwrap(),
            Value::Null
        );
    }
    for action in [
        rustfrp_desktop_support::bridge::WindowAction::Minimize,
        rustfrp_desktop_support::bridge::WindowAction::Hide,
        rustfrp_desktop_support::bridge::WindowAction::Show,
        rustfrp_desktop_support::bridge::WindowAction::Close,
    ] {
        native_host::window(&main, action).unwrap();
    }
    native_host::report_window(
        &state,
        rustfrp_desktop_support::window::UiState {
            busy: false,
            dirty: false,
            sensitive: false,
        },
    )
    .unwrap();
    assert!(state
        .window
        .lock()
        .unwrap()
        .can_release(Duration::from_secs(46), true, false));
    assert_eq!(
        invoke(
            &main,
            "controller_cancel",
            json!({"requestId":"never-dispatched"})
        )
        .unwrap()["cancelled"],
        false
    );
    assert_eq!(
        invoke(
            &main,
            "controller_request",
            json!({"operation":"raw-http","args":{},"requestId":"invalid-operation"})
        )
        .unwrap_err()["code"],
        "invalid_request"
    );
    for (label, url) in [
        ("secondary", tauri::WebviewUrl::App("index.html".into())),
        (
            "external",
            tauri::WebviewUrl::External("https://example.com/".parse().unwrap()),
        ),
    ] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, url)
            .build()
            .unwrap();
        assert_eq!(
            native_calls::trusted(&window).unwrap_err().code,
            "untrusted_window"
        );
        // The framework can reject remote IPC before our command is dispatched.
        assert!(invoke(&window, "host_command", json!({"action":"info","args":{}})).is_err());
        assert!(invoke(
            &window,
            "controller_cancel",
            json!({"requestId":"not-yours"})
        )
        .is_err());
        assert!(invoke(
            &window,
            "controller_request",
            json!({"operation":"me","args":{},"requestId":"not-yours"})
        )
        .is_err());
    }
    command("window", json!({"action":"close"})).unwrap();
    let poisoned = state.clone();
    assert!(std::thread::spawn(move || {
        let _guard = poisoned.window.lock().unwrap();
        panic!("poison the isolated fixture mutex");
    })
    .join()
    .is_err());
    assert_eq!(
        native_host::report_window(&state, rustfrp_desktop_support::window::UiState::default())
            .unwrap_err()
            .code,
        "local_io"
    );
}
