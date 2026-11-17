//! Window-independent Vesper client logic and persisted state.
#![deny(missing_docs)]

/// Account session storage backed by platform services.
pub mod credentials;
/// Client event reporting for UI and headless callers.
pub mod events;
/// Gateway authentication, grants, redemption and paid keys.
pub mod gateway;
mod http;
/// Verified installation, update, repair, rollback and removal.
pub mod install;
/// Installed game discovery.
pub mod library;
/// Downloaded media verification.
pub mod media;
/// Signed metadata verification, caching and source selection.
pub mod metadata;
mod platform;
/// Allowlisted runtime prerequisite rules.
pub mod runtime;
/// Settings loading and persistence.
pub mod settings;
/// Shared client state and construction.
pub mod state;
/// Store catalogue queries.
pub mod store;
/// Persistent tasks, pause/resume and progress reporting.
pub mod tasks;

pub use events::{EventSink, NoopEventSink};
pub use gateway::{email_start, email_verify, fetch_grants, logout, redeem_code};
pub use install::{
    check_update, install_from_index, repair_from_index, repair_game, rollback_game,
    set_game_channel, uninstall_game, verify_game, InstallRequest, UpdateCheck,
};
pub use library::list_installed;
pub use media::verify_media;
/// Launch a game using the platform process service.
pub use platform::host::{host_platforms, resolve_channel};
pub use platform::process::launch_game;
pub use runtime::{runtime_allowlist, validate_runtime_rules};
pub use settings::{get_settings, load_settings, save_settings, Settings};
pub use state::{new_app_state, AppState};
pub use store::{fetch_app_detail, fetch_store, AppSummary, StoreResponse};
pub use tasks::{list_tasks, pause_task, resume_task, start_install, TaskRecord};
