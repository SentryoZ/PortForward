//! Port Forward — Tauri v2 application entry point.

mod interceptor_commands;
// Public so the headless command-layer test (tests/tauri_commands.rs) can
// register the exact same tunnel command surface the app exposes.
pub mod tunnel_commands;

use tauri::Manager;
use tauri_plugin_sql::{Migration, MigrationKind};

/// SQLite migrations for the captured-request store (MAN-10 §3).
///
/// The SQL is the single canonical copy embedded by `interceptor-core`, so the
/// proxy's writes and the frontend's connection can never disagree on the
/// schema.
fn migrations() -> Vec<Migration> {
    vec![Migration {
        version: 1,
        description: "create_requests_table",
        sql: interceptor_core::store::SCHEMA_SQL,
        kind: MigrationKind::Up,
    }]
}

/// Database URL used by both the Rust side and `tauri-plugin-sql` on the frontend.
pub const DB_URL: &str = "sqlite:portforward.db";

/// File name (relative to the app-config dir) backing [`DB_URL`].
pub const DB_FILE_NAME: &str = "portforward.db";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations(DB_URL, migrations())
                .build(),
        )
        .manage(tunnel_commands::TunnelState::default())
        .manage(interceptor_commands::InterceptorState::default())
        .invoke_handler(tauri::generate_handler![
            tunnel_commands::start_tunnel,
            tunnel_commands::stop_tunnel,
            tunnel_commands::tunnel_status,
            interceptor_commands::start_interceptor,
            interceptor_commands::stop_interceptor,
            interceptor_commands::interceptor_status,
            interceptor_commands::get_captured_request,
            interceptor_commands::captured_request_count,
        ])
        .setup(|app| {
            // Surface the resolved app-data dir where a vendored cloudflared may live.
            if let Ok(dir) = app.path().app_data_dir() {
                println!("[port-forward] app data dir: {}", dir.display());
            }
            // Surface the DB path so curl/evidence tooling can find the store.
            if let Ok(dir) = app.path().app_config_dir() {
                println!(
                    "[port-forward] request store: {}",
                    dir.join(DB_FILE_NAME).display()
                );
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Port Forward");
}
