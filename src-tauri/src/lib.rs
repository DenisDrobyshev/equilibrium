mod clock;
mod commands;
mod critic;
mod db;
mod disclosure;
mod model;
mod protocol;
mod safety;
mod session;

pub use db::Store;
pub use disclosure::Disclosure;
pub use protocol::{Event, State, TransitionError};
pub use safety::{Assessment, RiskDetector, RuleBasedDetector};
pub use session::{Branch, Practice, Turn};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(commands::AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::open_vault,
            commands::begin_practice,
            commands::send_message,
            commands::record_problem,
            commands::record_goal,
            commands::record_ratings,
            commands::advance,
            commands::history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
