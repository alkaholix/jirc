//! jIRC backend entry point.
//!
//! Wires up the Tauri shell, the IRC connection manager (managed state), and
//! the `invoke` command surface. Protocol handling lives in [`irc`].

mod commands;
mod config;
mod irc;
mod script;
mod storage;

use irc::ConnectionManager;
use script::ScriptEngine;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Install the rustls crypto provider (ring) for TLS connections.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jirc_lib=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ConnectionManager::new())
        .manage(ScriptEngine::new())
        .manage(script::socket::SocketManager::new())
        .manage(script::timer::TimerManager::new())
        .manage(irc::state::StateStore::new())
        .setup(|app| {
            // Rename the legacy `com.jirc.app` data folder to `jIRC` (once) before
            // anything reads profiles/scripts/logs.
            storage::migrate_legacy_app_dir(app.handle());
            // Materialise the data subfolders under the jIRC folder (scripts/ is
            // created when scripts load; dcc/ for received transfers).
            let _ = storage::dcc_dir(app.handle());
            let engine = app.state::<ScriptEngine>();
            // Install the real socket backend so /socklisten/$sock(...) work.
            engine.set_sockets(std::sync::Arc::new(script::socket::EngineSockets::new(
                app.handle().clone(),
            )));
            script::load_persisted(app.handle(), &engine);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::core_version,
            commands::open_help,
            commands::open_url,
            commands::open_detached_window,
            commands::focus_window,
            commands::dock_window,
            commands::close_detached,
            commands::exit_app,
            commands::dns_lookup,
            commands::irc_connect,
            commands::irc_disconnect,
            commands::irc_send_raw,
            commands::irc_send_message,
            commands::irc_join,
            commands::irc_part,
            commands::irc_set_nick,
            commands::irc_whois,
            commands::irc_list_connections,
            commands::ircx_enable,
            commands::ircx_whisper,
            commands::ircx_access,
            commands::ircx_prop_get,
            commands::ircx_prop_set,
            commands::ircx_create,
            commands::ircx_listx,
            commands::ircx_knock,
            storage::profiles_load,
            storage::profiles_save,
            storage::profiles_delete,
            storage::keyring_available,
            storage::data_location,
            storage::set_data_location,
            storage::log_append,
            storage::log_read,
            script::scripts_list,
            script::script_add_examples,
            script::script_read,
            script::script_write,
            script::script_delete,
            script::script_run_alias,
            script::script_run_input,
            script::script_run_command,
            script::script_run_dialog,
            script::script_sockets,
            script::script_popups,
            script::script_run_popup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running jIRC");
}
