//! Real Tauri desktop shell. Boots `orchestrator-core` + `orchestrator-api`
//! synchronously in `setup()` (via `async_runtime::block_on`, the documented
//! pattern for one-shot async work that must finish before the app is
//! usable — see https://tauri.app), then opens the window declared in
//! `tauri.conf.json`. The API's base URL and per-launch bearer token are
//! handed to the frontend through a real Tauri command (`get_api_config`,
//! see `frontend/src/lib/api.ts`) rather than a build-time env var, since
//! each launch generates a fresh token server-side.

use std::path::PathBuf;

use orchestrator_api::ApiHandle;
use tauri::Manager;

struct ApiConfig {
    base_url: String,
    token: String,
}

#[tauri::command]
fn get_api_config(state: tauri::State<ApiConfig>) -> serde_json::Value {
    serde_json::json!({ "baseUrl": state.base_url, "token": state.token })
}

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_api_config])
        .setup(|app| {
            let (api_handle, base_url, token) = tauri::async_runtime::block_on(async {
                let db_path = data_dir().join("orchestrator.sqlite3");
                std::fs::create_dir_all(data_dir()).expect("create app data dir");
                tracing::info!("opening state db at {}", db_path.display());

                let core = orchestrator_core::Core::open(&db_path).expect("open core db");
                let backups_dir = data_dir().join("backups");
                let snapshots_dir = data_dir().join("snapshots");
                let handle: ApiHandle = orchestrator_api::serve(core, backups_dir, snapshots_dir)
                    .await
                    .expect("start local api");

                let base_url = handle.base_url();
                let token = handle.token.clone();
                (handle, base_url, token)
            });

            tracing::info!("orchestrator-api listening on {}", base_url);

            // Kept alive for the app's whole life — dropping it would let its
            // background server task detach (tokio's `JoinHandle::drop` just
            // stops tracking it rather than aborting it, but there's no
            // reason to rely on that when `app.manage` gives it a real home).
            app.manage(api_handle);
            app.manage(ApiConfig { base_url, token });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn data_dir() -> PathBuf {
    std::env::var_os("ORCHESTRATOR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".orchestrator-data"))
}
