#![windows_subsystem = "windows"]

use launcher::{AppState, InstallRequest, Settings, TaskRecord};
use serde_json::Value;
use tauri::{AppHandle, State};
use vesper_launcher as launcher;

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    launcher::get_settings(state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn save_settings(settings: Settings, state: State<'_, AppState>) -> Result<(), String> {
    launcher::save_settings(settings, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn fetch_store(api_base: String) -> Result<launcher::StoreResponse, String> {
    launcher::fetch_store(&api_base).map_err(|e| e.to_string())
}
#[tauri::command]
fn fetch_app_detail(api_base: String, app_id: String) -> Result<Value, String> {
    launcher::fetch_app_detail(&api_base, &app_id).map_err(|e| e.to_string())
}
#[tauri::command]
fn start_install(
    request: InstallRequest,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    launcher::start_install(request, &app, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn list_installed(
    state: State<'_, AppState>,
) -> Result<Vec<launcher::library::InstalledGame>, String> {
    launcher::list_installed(state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn list_tasks(state: State<'_, AppState>) -> Vec<TaskRecord> {
    launcher::list_tasks(state.inner())
}
#[tauri::command]
fn pause_task(task_id: String, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    launcher::pause_task(&task_id, &app, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn check_update(
    install_dir: String,
    state: State<'_, AppState>,
) -> Result<launcher::UpdateCheck, String> {
    launcher::check_update(&install_dir, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn resume_task(task_id: String, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    launcher::resume_task(&task_id, &app, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn rollback_game(install_dir: String) -> Result<(), String> {
    launcher::rollback_game(&install_dir).map_err(|e| e.to_string())
}
#[tauri::command]
fn verify_game(install_dir: String, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    launcher::verify_game(&install_dir, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn repair_game(
    install_dir: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    launcher::repair_game(&install_dir, state.inner(), &app).map_err(|e| e.to_string())
}
#[tauri::command]
fn uninstall_game(
    install_dir: String,
    delete_user_files: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    launcher::uninstall_game(&install_dir, delete_user_files, state.inner())
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn set_game_channel(install_dir: String, channel: String) -> Result<(), String> {
    launcher::set_game_channel(&install_dir, &channel).map_err(|e| e.to_string())
}
#[tauri::command]
fn launch_game(install_dir: String, state: State<'_, AppState>) -> Result<String, String> {
    launcher::launch_installed(&install_dir, state.inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn runtime_allowlist() -> Vec<Value> {
    launcher::runtime_allowlist()
}
#[tauri::command]
fn verify_media(url: String, expected_blake3: String) -> Result<Vec<u8>, String> {
    launcher::verify_media(&url, &expected_blake3).map_err(|e| e.to_string())
}
#[tauri::command]
fn email_start(gateway_base: String, email: String) -> Result<(), String> {
    launcher::email_start(&gateway_base, &email).map_err(|e| e.to_string())
}
#[tauri::command]
fn email_verify(gateway_base: String, email: String, code: String) -> Result<(), String> {
    launcher::email_verify(&gateway_base, &email, &code).map_err(|e| e.to_string())
}
#[tauri::command]
fn fetch_grants(gateway_base: String) -> Result<Value, String> {
    launcher::fetch_grants(&gateway_base).map_err(|e| e.to_string())
}
#[tauri::command]
fn redeem_code(gateway_base: String, app_id: String, code: String) -> Result<Value, String> {
    launcher::redeem_code(&gateway_base, &app_id, &code).map_err(|e| e.to_string())
}
#[tauri::command]
fn logout() -> Result<(), String> {
    launcher::logout().map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(launcher::new_app_state(launcher::load_settings()))
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            fetch_store,
            fetch_app_detail,
            start_install,
            list_installed,
            list_tasks,
            pause_task,
            resume_task,
            check_update,
            rollback_game,
            verify_game,
            repair_game,
            uninstall_game,
            set_game_channel,
            launch_game,
            runtime_allowlist,
            verify_media,
            email_start,
            email_verify,
            fetch_grants,
            redeem_code,
            logout
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vesper");
}
