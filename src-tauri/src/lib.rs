pub mod commands;
pub mod core;
pub mod models;
pub mod providers;

pub use commands::register_commands;
pub use core::app_state::{setup_app_state, AppState};

#[cfg(all(test, target_os = "windows", target_env = "gnu"))]
#[link(
    name = "sceneweaver_test_resources",
    kind = "static",
    modifiers = "+whole-archive"
)]
extern "C" {}

pub fn run() {
    let builder = tauri::Builder::default();

    // Enabled only by the explicit `webdriver-e2e` build feature. Release
    // bundles never expose this test server.
    #[cfg(feature = "webdriver-e2e")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());

    let context = tauri::generate_context!();
    #[cfg(all(debug_assertions, feature = "webdriver-e2e"))]
    let context = {
        let mut context = context;
        if let Ok(port) = std::env::var("SCENEWEAVER_E2E_VITE_PORT") {
            let port = port.parse::<u16>().expect("invalid E2E Vite port");
            context.config_mut().build.dev_url = Some(
                format!("http://127.0.0.1:{port}")
                    .parse()
                    .expect("valid loopback development URL"),
            );
        }
        context
    };

    builder
        .setup(|app| {
            setup_app_state(app)?;
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(register_commands())
        .run(context)
        .expect("error while running tauri application");
}
