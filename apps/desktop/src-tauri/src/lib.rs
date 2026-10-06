mod adapter;
mod app;
mod canary;
mod keychain;
mod quit;
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
            #[cfg(unix)]
            watch_signals(a.handle().clone());
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
            app::pause,
            app::resume,
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
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| match event {
            // The last window closing, or `exit`: a running lecture is saved first, then the app exits.
            tauri::RunEvent::ExitRequested { api, .. } => {
                if let Some(until) = handle.try_state::<app::App>().and_then(|a| a.quit_deadline()) {
                    api.prevent_exit();
                    let h = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let app = h.state::<app::App>();
                        app::save_for_quit(&app, until).await;
                        h.exit(0); // asks again; nothing is running now, or the time is up, so it goes through
                    });
                }
            }
            // A native quit (Cmd-Q, an Apple Event) arrives only here and cannot be cancelled: the main thread waits for
            // the lecture to save, until the same deadline. The lecture runs on Tauri's runtime, not on this thread.
            tauri::RunEvent::Exit => {
                if let Some(app) = handle.try_state::<app::App>() {
                    if let Some(until) = app.quit_deadline() {
                        tauri::async_runtime::block_on(app::save_for_quit(&app, until));
                    }
                }
            }
            _ => {}
        });
}

/// SIGTERM, SIGHUP and SIGINT: nothing in Tauri handles them, so the process would end with the recording unsaved.
/// The first saves a running lecture and exits; a second, while that is going on, exits at once.
#[cfg(unix)]
fn watch_signals(handle: tauri::AppHandle) {
    use tokio::signal::unix::{signal, SignalKind};
    tauri::async_runtime::spawn(async move {
        let (Ok(mut term), Ok(mut hup), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::hangup()), signal(SignalKind::interrupt())) else { return };
        let mut seen = 0;
        loop {
            tokio::select! {
                _ = term.recv() => {}
                _ = hup.recv() => {}
                _ = int.recv() => {}
            }
            seen += 1;
            if seen > 1 {
                std::process::exit(1);
            }
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                let app = h.state::<app::App>();
                if let Some(until) = app.quit_deadline() {
                    app::save_for_quit(&app, until).await;
                }
                h.exit(0);
            });
        }
    });
}
