//! Port Forward — Tauri v2 application entry point.

mod tunnel_commands;

use tauri::Manager;
use tauri_plugin_sql::{Migration, MigrationKind};

/// SQLite migrations for the captured-request store (MAN-10 §3).
fn migrations() -> Vec<Migration> {
    vec![Migration {
        version: 1,
        description: "create_requests_table",
        sql: include_str!("../migrations/0001_init.sql"),
        kind: MigrationKind::Up,
    }]
}

/// Database URL used by both the Rust side and `tauri-plugin-sql` on the frontend.
pub const DB_URL: &str = "sqlite:portforward.db";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations(DB_URL, migrations())
                .build(),
        )
        .manage(tunnel_commands::TunnelState::default())
        .invoke_handler(tauri::generate_handler![
            tunnel_commands::start_tunnel,
            tunnel_commands::stop_tunnel,
            tunnel_commands::tunnel_status,
        ])
        .setup(|app| {
            // Surface the resolved app-data dir where a vendored cloudflared may live.
            if let Ok(dir) = app.path().app_data_dir() {
                println!("[port-forward] app data dir: {}", dir.display());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Port Forward");
}
