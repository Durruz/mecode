#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mecode_core::{Config, Hub};
use tauri::WebviewWindowBuilder;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = mecode_server::default_data_dir();
            let config = Config::load_or_create(&data_dir)?;
            let hub = Hub::new(config, &data_dir)?;
            let (address, _server) = tauri::async_runtime::block_on(mecode_server::serve(hub, 0))?;

            WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
                .initialization_script(format!("window.MECODE_API = 'http://{address}';"))
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Mecode");
}
