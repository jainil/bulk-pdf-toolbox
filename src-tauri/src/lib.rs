pub mod engine;
pub mod model;
pub mod output;
pub mod proc;
pub mod runtime;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use engine::{AddResult, Engine, EventSink, Snapshot};
use model::{JobStatus, Settings};
use runtime::RuntimeInfo;

type Eng<'a> = State<'a, Arc<Engine>>;

struct TauriSink(tauri::AppHandle);

impl EventSink for TauriSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
    fn attention(&self) {
        if let Some(w) = self.0.get_webview_window("main") {
            let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
        }
    }
}

#[tauri::command]
fn get_snapshot(engine: Eng) -> Snapshot {
    engine.snapshot()
}

#[tauri::command]
async fn runtime_info(engine: Eng<'_>) -> Result<RuntimeInfo, ()> {
    Ok(match engine.runtime() {
        Ok(rt) => rt.info().await,
        Err(e) => runtime::missing_info(e),
    })
}

#[tauri::command]
async fn add_paths(engine: Eng<'_>, paths: Vec<String>) -> Result<AddResult, String> {
    let engine = engine.inner().clone();
    let paths = paths.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || engine.add_paths(paths))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_settings(engine: Eng, settings: Settings) {
    engine.set_settings(settings);
}

#[tauri::command]
fn start(engine: Eng) -> Result<(), String> {
    engine.start()
}

#[tauri::command]
fn pause(engine: Eng) {
    engine.pause();
}

#[tauri::command]
fn stop(engine: Eng) {
    engine.stop();
}

#[tauri::command]
fn cancel_jobs(engine: Eng, ids: Vec<u64>) {
    engine.cancel(&ids);
}

#[tauri::command]
fn retry_jobs(engine: Eng, ids: Vec<u64>) {
    engine.retry(&ids);
}

#[tauri::command]
fn retry_failed(engine: Eng) {
    engine.retry_failed();
}

#[tauri::command]
fn remove_jobs(engine: Eng, ids: Vec<u64>) {
    engine.remove(&ids);
}

#[tauri::command]
fn prioritize_jobs(engine: Eng, ids: Vec<u64>) {
    engine.prioritize(&ids);
}

#[tauri::command]
fn clear_jobs(engine: Eng, statuses: Vec<JobStatus>) {
    engine.remove_with_status(&statuses);
}

#[tauri::command]
fn job_log(engine: Eng, id: u64) -> Vec<String> {
    engine.job_log(id)
}

#[tauri::command]
fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle, engine: Eng) {
    engine.shutdown();
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let rt = runtime::locate(app.handle());
            let dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("bulk-pdf-toolbox"));
            let engine = Engine::new(TauriSink(app.handle().clone()), dir, rt);
            engine.spawn_loops();
            app.manage(engine);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let engine = window.state::<Arc<Engine>>();
                if engine.is_busy() {
                    api.prevent_close();
                    let _ = window.emit("app://confirm-quit", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            runtime_info,
            add_paths,
            set_settings,
            start,
            pause,
            stop,
            cancel_jobs,
            retry_jobs,
            retry_failed,
            remove_jobs,
            prioritize_jobs,
            clear_jobs,
            job_log,
            open_path,
            reveal_path,
            quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| match event {
        // Cmd+Q / last window closed while files are processing: ask first.
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            if handle.state::<Arc<Engine>>().is_busy() {
                api.prevent_exit();
                let _ = handle.emit("app://confirm-quit", ());
            }
        }
        RunEvent::Exit => handle.state::<Arc<Engine>>().shutdown(),
        _ => {}
    });
}
