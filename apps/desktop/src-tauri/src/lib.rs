mod adapter;
mod app;
mod canary;
mod keychain;
mod wire;

use tauri::Manager;
use tauri_plugin_global_shortcut::ShortcutState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let check: Vec<String> = std::env::args().skip_while(|a| a.as_str() != "--check").skip(1).collect();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(|app, _, e| if e.state == ShortcutState::Pressed { app::shortcut(app) }).build())
        .setup(move |a| {
            a.manage(app::App::new(a.handle().clone()));
            if !check.is_empty() {
                let handle = a.handle().clone();
                std::thread::spawn(move || {
                    let _ = canary::log_line(&format!("{} started", check.join(" ")));
                    let _ = canary::log_check(&check, &canary::run_check(&check));
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::attach,
            app::get_session_state,
            app::select_folder,
            app::inputs,
            app::loopback_status,
            app::start_lecture,
            app::stop,
            app::snapshot,
            app::polish,
            app::cancel,
            app::open_page,
            app::key_status,
            app::save_key,
            app::import_key_from_env,
            app::spend_summary,
            app::check_config,
            app::check_report,
            app::exit_app,
            app::hide_window_for,
            app::capture_windows,
            app::capture_preview,
            app::capture_select,
            app::capture_watch,
            app::capture_saved_region,
            app::check_deck,
            app::check_record,
            app::capture_now,
            app::import_slides,
            app::open_screen_settings,
            app::microphone,
            app::open_microphone_settings,
            app::use_input,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
