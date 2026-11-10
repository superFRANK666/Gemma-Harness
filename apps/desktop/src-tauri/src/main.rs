#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use harness_core::Harness;
use serde_json::Value;
use std::sync::Arc;
use tauri::{ipc::Channel, Manager, State};

#[tauri::command]
async fn command(state: State<'_, Arc<Harness>>, request: Value) -> Result<Value, String> {
    state
        .inner()
        .command(request)
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn stream_turn(
    state: State<'_, Arc<Harness>>,
    session_id: String,
    input: String,
    images: Option<Vec<String>>,
    on_event: Channel<Value>,
) -> Result<(), String> {
    state
        .inner()
        .turn_with_images(
            session_id,
            input,
            images.unwrap_or_default(),
            Arc::new(move |event| {
                let _ = on_event.send(event);
            }),
        )
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn stream_tool(
    state: State<'_, Arc<Harness>>,
    request: Value,
    on_event: Channel<Value>,
) -> Result<Value, String> {
    state
        .inner()
        .manual_tool(
            request,
            Arc::new(move |event| {
                let _ = on_event.send(event);
            }),
        )
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn prepare_app_update(state: State<'_, Arc<Harness>>) -> Result<(), String> {
    state.inner().shutdown().await;
    Ok(())
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            if let Ok(resource) = app.path().resource_dir() {
                if resource.join("workers").exists() {
                    std::env::set_var("HARNESS_RESOURCE_DIR", resource);
                }
            }
            app.manage(Harness::open(dir)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            command,
            stream_turn,
            stream_tool,
            prepare_app_update
        ])
        .build(tauri::generate_context!())
        .expect("Cannot initialize Gemma Harness")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                let state = app.state::<Arc<Harness>>().inner().clone();
                tauri::async_runtime::block_on(state.shutdown());
            }
        });
}
