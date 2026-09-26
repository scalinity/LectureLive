mod adapter;
mod app;
mod canary;
mod keychain;
mod wire;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let check: Vec<String> = std::env::args().skip_while(|a| a.as_str() != "--check").skip(1).collect();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
