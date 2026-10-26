//! Paxile tray app owns a persistent, runtime-controlled engine child.

mod config;
mod runtime;
mod transaction;
mod updater_policy;
#[cfg(windows)]
mod window_frame;

use std::{
    path::PathBuf,
    process::Child,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use paxile_policy::Config;
use serde::Serialize;
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::{Builder as AutostartBuilder, ManagerExt};

#[derive(Default)]
struct AppState {
    config: Mutex<Config>,
    silent_start: Mutex<bool>,
    backend: Mutex<Option<Child>>,
    logs: Mutex<runtime::LogBuffer>,
    core_error: Mutex<Option<String>>,
    core_status: Mutex<runtime::CoreStatus>,
    update_lock: Mutex<()>,
    updater_operation: tokio::sync::Mutex<()>,
    exit_requested: AtomicBool,
    pending_update: Mutex<updater_policy::Pending<tauri_plugin_updater::Update>>,
}

#[derive(Serialize)]
struct UpdateInfo {
    available: bool,
    version: Option<String>,
    body: Option<String>,
    current_version: String,
    update_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "event", content = "data")]
enum UpdateProgress {
    Started { content_length: Option<u64> },
    Progress { chunk_length: usize },
    Finished,
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<UpdateInfo, String> {
    use tauri_plugin_updater::UpdaterExt;
    let operation_state = app.state::<AppState>();
    let _operation = operation_state
        .updater_operation
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    operation_state
        .pending_update
        .lock()
        .map_err(|_| "Updater state poisoned")?
        .clear();
    let current = env!("CARGO_PKG_VERSION").to_owned();
    let update = app
        .updater_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let mut pending = state
        .pending_update
        .lock()
        .map_err(|_| "Updater state poisoned")?;
    pending.clear();
    if let Some(update) = update {
        if update.download_url.scheme() != "https"
            || update.download_url.host_str() != Some("github.com")
            || !update
                .download_url
                .path()
                .starts_with("/KamiyaShiroba/Paxile/releases/download/")
        {
            return Err("Update artifact must be an HTTPS Paxile GitHub release".into());
        }
        let version = update.version.clone();
        let body = update.body.clone();
        let id = pending.replace(update);
        Ok(UpdateInfo {
            available: true,
            version: Some(version),
            body,
            current_version: current,
            update_id: Some(id),
        })
    } else {
        Ok(UpdateInfo {
            available: false,
            version: None,
            body: None,
            current_version: current,
            update_id: None,
        })
    }
}

#[tauri::command]
async fn install_update(
    app: AppHandle,
    window: tauri::WebviewWindow,
    update_id: String,
    confirmed: bool,
    on_event: tauri::ipc::Channel<UpdateProgress>,
) -> Result<(), String> {
    use tauri_plugin_updater::Update;
    validate_settings_drag(window.label())?;
    let operation_app = app.clone();
    let operation_state = operation_app.state::<AppState>();
    let _operation = operation_state
        .updater_operation
        .try_lock()
        .map_err(|_| "An update operation is already in progress")?;
    let update: Update = {
        let state = app.state::<AppState>();
        let mut pending = state
            .pending_update
            .lock()
            .map_err(|_| "Updater state poisoned")?;
        pending.take(&update_id, confirmed)?
    };
    let mut started = false;
    runtime::log(
        &app,
        "info",
        &format!("Downloading signed update {}", update.version),
    );
    let bytes = update
        .download(
            |chunk, total| {
                if !started {
                    let _ = on_event.send(UpdateProgress::Started {
                        content_length: total,
                    });
                    started = true;
                }
                let _ = on_event.send(UpdateProgress::Progress {
                    chunk_length: chunk,
                });
            },
            || {
                let _ = on_event.send(UpdateProgress::Finished);
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    // Download verifies the signature before the owned core is stopped.
    blocking(move || {
        let state = app.state::<AppState>();
        // stop acquires update_lock itself and serializes against startup/config edits.
        if state.exit_requested.swap(true, Ordering::SeqCst) {
            return Err("Paxile is shutting down".into());
        }
        runtime::stop(&app);
        runtime::log(&app, "info", "Signed update verified; starting installer");
        if let Err(error) = update.install(bytes) {
            state.exit_requested.store(false, Ordering::SeqCst);
            runtime::log(
                &app,
                "error",
                &format!("Update installer failed: {error}; restart Paxile to resume core"),
            );
            return Err(error.to_string());
        }
        Ok(())
    })
    .await
}

fn selected_program_name(path: Option<&std::path::Path>) -> Result<Option<String>, String> {
    path.map(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or_else(|| "Selected file has no valid UTF-8 filename".to_owned())
    })
    .transpose()
}

fn should_start_silently(_autostart: bool, preference: bool) -> bool {
    preference
}

fn probe_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(resource_dir) = app.path().resource_dir() {
        for bundled in [
            resource_dir.join("paxile-probe.exe"),
            resource_dir.join("target/runtime-control/debug/paxile-probe.exe"),
            resource_dir.join("target/reconnect/debug/paxile-probe.exe"),
        ] {
            if bundled.is_file() && bundled.with_file_name("paxile_spy.dll").is_file() {
                return Some(bundled);
            }
        }
    }
    let mut dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for _ in 0..7 {
        for candidate in [
            dir.join("paxile-probe.exe"),
            dir.join("target/runtime-control/debug/paxile-probe.exe"),
            dir.join("target/reconnect/debug/paxile-probe.exe"),
            dir.join("target/debug/paxile-probe.exe"),
        ] {
            if candidate.is_file() && candidate.with_file_name("paxile_spy.dll").is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[tauri::command]
fn get_logs(state: State<AppState>) -> Vec<runtime::LogEntry> {
    state.logs.lock().unwrap().entries.iter().cloned().collect()
}
#[tauri::command]
fn get_core_status(state: State<AppState>) -> runtime::CoreStatus {
    runtime::cached_status(&state)
}
#[derive(Serialize)]
struct AvailablePreset {
    name: String,
    display_name: String,
    executables: Vec<String>,
    non_text_classes: Vec<String>,
}
#[tauri::command]
fn get_available_presets() -> Vec<AvailablePreset> {
    Config::default()
        .presets
        .into_iter()
        .map(|p| AvailablePreset {
            display_name: "CLIP STUDIO PAINT".into(),
            name: p.name,
            executables: p.executables,
            non_text_classes: p.non_text_classes,
        })
        .collect()
}
#[derive(Serialize)]
struct Status {
    enabled: Option<bool>,
    profile: String,
    preset_count: usize,
    whitelist_count: usize,
}

#[tauri::command]
fn get_config(state: State<AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
async fn save_config(app: AppHandle, config: Config) -> Result<(), String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let _update = state.update_lock.lock().unwrap();
        commit_config(&app, &state, config, true)
    })
    .await
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
}

fn commit_config(
    app: &AppHandle,
    state: &AppState,
    next: Config,
    reload: bool,
) -> Result<(), String> {
    if state.exit_requested.load(Ordering::SeqCst) {
        return Err("Paxile is shutting down".into());
    }
    let previous = state.config.lock().unwrap().clone();
    let result = transaction::commit(
        &previous,
        &next,
        |config| runtime::update(app, reload, config),
        |config| config::save(app, config),
    );
    if result.is_ok() {
        *state.config.lock().unwrap() = next;
    } else if let Err(ref error) = result {
        runtime::log(app, "error", error);
    }
    result
}

#[tauri::command]
fn get_status(state: State<AppState>) -> Status {
    let enabled = runtime::cached_status(&state).requested_enabled;
    let c = state.config.lock().unwrap();
    Status {
        enabled,
        profile: c.profile.clone(),
        preset_count: c.presets.len(),
        whitelist_count: c.allow_exe.len(),
    }
}

#[tauri::command]
async fn set_enabled(app: AppHandle, enabled: bool) -> Result<bool, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let _update = state.update_lock.lock().unwrap();
        let mut next = state.config.lock().unwrap().clone();
        next.enabled = enabled;
        commit_config(&app, &state, next, false)?;
        Ok(enabled)
    })
    .await
}

#[tauri::command]
async fn get_autostart(app: AppHandle) -> Result<bool, String> {
    blocking(move || Ok(app.autolaunch().is_enabled().unwrap_or(false))).await
}

#[tauri::command]
async fn set_autostart(app: AppHandle, on: bool) -> Result<bool, String> {
    blocking(move || {
        let m = app.autolaunch();
        if on {
            m.enable().map_err(|e| e.to_string())?;
        } else {
            m.disable().map_err(|e| e.to_string())?;
        }
        Ok(m.is_enabled().unwrap_or(on))
    })
    .await
}

#[tauri::command]
fn get_silent_start(state: State<AppState>) -> bool {
    *state.silent_start.lock().unwrap()
}

#[tauri::command]
async fn set_silent_start(app: AppHandle, on: bool) -> Result<bool, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let _update = state.update_lock.lock().unwrap();
        let mut preference = config::load_preferences(&app);
        preference.silent_start = on;
        config::save_preferences(&app, &preference)?;
        *state.silent_start.lock().unwrap() = on;
        Ok(on)
    })
    .await
}

#[tauri::command]
async fn pick_program(window: tauri::WebviewWindow) -> Result<Option<String>, String> {
    let selected = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Choose a program")
        // The first filter is the default; the second allows any file type.
        .add_filter("Executables (*.exe)", &["exe"])
        .add_filter("All files (*.*)", &["*"])
        .pick_file()
        .await;
    selected_program_name(selected.as_ref().map(|file| file.path()))
}

fn validate_settings_drag(label: &str) -> Result<(), String> {
    if label == "settings" {
        Ok(())
    } else {
        Err("Only the settings window can start a settings drag".to_owned())
    }
}

#[tauri::command]
fn start_settings_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    // Tauri injects the invoking window; the frontend cannot supply another HWND
    // or window label. Never let the tray popup move the settings window.
    validate_settings_drag(window.label())?;
    window.start_dragging().map_err(|error| error.to_string())
}

#[tauri::command]
fn minimize_settings(app: AppHandle) -> Result<(), String> {
    app.get_webview_window("settings")
        .ok_or_else(|| "Settings window is not open".to_owned())?
        .minimize()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn close_settings(app: AppHandle) -> Result<(), String> {
    // Closing this WebView does not set exit_requested or stop the engine.
    app.get_webview_window("settings")
        .ok_or_else(|| "Settings window is not open".to_owned())?
        .close()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn open_settings_window(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("paxile-settings-window".into())
        .spawn(move || open_settings(&app));
}

#[tauri::command]
async fn quit_app(app: AppHandle) -> Result<(), String> {
    app.state::<AppState>()
        .exit_requested
        .store(true, Ordering::SeqCst);
    blocking(move || {
        runtime::stop(&app);
        app.exit(0);
        Ok(())
    })
    .await
}

fn settings_window_config() -> tauri::utils::config::WindowConfig {
    tauri::utils::config::WindowConfig {
        label: "settings".into(),
        title: "Paxile".into(),
        url: WebviewUrl::App("index.html".into()),
        width: 600.0,
        height: 800.0,
        min_width: Some(420.0),
        min_height: Some(500.0),
        center: true,
        resizable: true,
        decorations: false,
        // Tao enables the native DWM shadow for this undecorated window.
        // Windows 11's otherwise visible 1px border is suppressed below.
        shadow: true,
        visible: false,
        ..Default::default()
    }
}

/// Settings window is created on demand and destroyed on close (default close
/// behavior), so idle Paxile is just the tray — no WebView2 processes.
fn open_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    match WebviewWindowBuilder::from_config(app, &settings_window_config())
        .and_then(|builder| builder.build())
    {
        Ok(window) => {
            // Configure DWM before showing to avoid flashing the default border.
            #[cfg(windows)]
            if let Err(error) = window_frame::apply(&window) {
                runtime::log(app, "warn", &error);
            }
            let _ = window.show();
            let _ = window.set_focus();
        }
        Err(error) => runtime::log(app, "error", &format!("Open settings: {error}")),
    }
}

fn hide_tray_popup(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("tray-menu") {
        let _ = window.hide();
    }
}

fn open_tray_popup(app: &AppHandle) {
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("paxile-tray-popup".into())
        .spawn(move || show_tray_popup(&app));
}

fn show_tray_popup(app: &AppHandle) {
    let (x, y) = app
        .cursor_position()
        .ok()
        .and_then(|point| {
            app.monitor_from_point(point.x, point.y)
                .ok()
                .flatten()
                .map(|monitor| {
                    let area = monitor.work_area();
                    let scale = monitor.scale_factor();
                    let left = area.position.x as f64;
                    let top = area.position.y as f64;
                    let right = left + area.size.width as f64;
                    let bottom = top + area.size.height as f64;
                    let width = 128.0 * scale;
                    let height = 144.0 * scale;
                    let px = point.x.clamp(left, (right - width).max(left));
                    let py = point.y.clamp(top, (bottom - height).max(top));
                    (px, py)
                })
        })
        .unwrap_or((800.0, 600.0));
    if let Some(window) = app.get_webview_window("tray-menu") {
        let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let app_handle = app.clone();
    let _ = WebviewWindowBuilder::new(app, "tray-menu", WebviewUrl::App("tray.html".into()))
        .title("Paxile")
        .inner_size(128.0, 144.0)
        .min_inner_size(128.0, 144.0)
        .max_inner_size(128.0, 144.0)
        .position(x, y)
        .decorations(false)
        .shadow(true)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false)
        .build()
        .map(|window| {
            let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
            #[cfg(windows)]
            if let Err(error) = window_frame::apply(&window) {
                eprintln!("{error}");
            }
            let _ = window.show();
            let _ = window.set_focus();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Focused(false) = event {
                    if let Some(window) = app_handle.get_webview_window("tray-menu") {
                        let _ = window.hide();
                    }
                }
            });
        });
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            open_settings(app);
        }))
        .plugin(AutostartBuilder::new().arg("--paxile-autostart").build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_status,
            set_enabled,
            get_available_presets,
            get_logs,
            get_core_status,
            get_autostart,
            set_autostart,
            get_silent_start,
            set_silent_start,
            open_settings_window,
            pick_program,
            start_settings_drag,
            minimize_settings,
            close_settings,
            quit_app,
            check_update,
            install_update
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let updater_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                match check_update(updater_handle.clone()).await {
                    Ok(info) => runtime::log(
                        &updater_handle,
                        "info",
                        &format!(
                            "Update check: available={}, version={:?}",
                            info.available, info.version
                        ),
                    ),
                    Err(error) => {
                        runtime::log(&updater_handle, "warn", &format!("Update check: {error}"))
                    }
                }
            });
            let _tray = TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Paxile")
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => hide_tray_popup(tray.app_handle()),
                    TrayIconEvent::Click {
                        button: MouseButton::Right,
                        button_state: MouseButtonState::Up,
                        ..
                    } => open_tray_popup(tray.app_handle()),
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => open_settings(tray.app_handle()),
                    _ => {}
                })
                .build(app)?;

            std::thread::Builder::new()
                .name("paxile-core-monitor".into())
                .spawn(move || {
                    let state = handle.state::<AppState>();
                    let silent;
                    {
                        // Serialize initialization with edits and shutdown. Never publish a child after exit.
                        let _update = state.update_lock.lock().unwrap();
                        if state.exit_requested.load(Ordering::SeqCst) {
                            return;
                        }
                        let loaded = config::load(&handle);
                        let preferences = config::load_preferences(&handle);
                        *state.config.lock().unwrap() = loaded.clone();
                        *state.silent_start.lock().unwrap() = preferences.silent_start;
                        silent = should_start_silently(
                            std::env::args().any(|arg| arg == "--paxile-autostart"),
                            preferences.silent_start,
                        );
                        let result = config::save(&handle, &loaded).and_then(|()| {
                            if state.exit_requested.load(Ordering::SeqCst) {
                                return Ok(());
                            }
                            let child = runtime::start(&handle)?;
                            *state.backend.lock().unwrap() = Some(child);
                            Ok(())
                        });
                        if let Err(error) = result {
                            runtime::log(&handle, "error", &error);
                        }
                    }
                    if state.exit_requested.load(Ordering::SeqCst) {
                        return;
                    }
                    if !silent || state.core_error.lock().unwrap().is_some() {
                        open_settings(&handle);
                    }
                    let mut previous = None;
                    loop {
                        if state.exit_requested.load(Ordering::SeqCst) {
                            break;
                        }
                        let status = runtime::refresh_status(&handle);
                        if status.error != previous {
                            if let Some(ref error) = status.error {
                                runtime::log(&handle, "error", error);
                            }
                            previous = status.error;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                })?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Paxile")
        .run(|app, event| {
            // Closing the settings window must not quit the app: the tray stays.
            if let RunEvent::ExitRequested { api, .. } = event {
                if app
                    .state::<AppState>()
                    .exit_requested
                    .load(Ordering::SeqCst)
                {
                    runtime::stop(app);
                } else {
                    api.prevent_exit();
                }
            }
        });
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn update_metadata_schema_preserves_notes_and_confirmation_id() {
        let info = UpdateInfo {
            available: true,
            version: Some("0.2.0".into()),
            body: Some("Release notes\n- Fixed bug".into()),
            current_version: "0.1.0".into(),
            update_id: Some("42".into()),
        };
        let value = serde_json::to_value(info).unwrap();
        assert_eq!(value["body"], "Release notes\n- Fixed bug");
        assert_eq!(value["current_version"], "0.1.0");
        assert_eq!(value["update_id"], "42");
        assert_eq!(value["available"], true);
    }

    #[test]
    fn drag_request_is_limited_to_the_settings_webview() {
        assert!(validate_settings_drag("settings").is_ok());
        assert!(validate_settings_drag("tray-menu").is_err());
        assert!(validate_settings_drag("other").is_err());
    }

    #[test]
    fn picker_cancel_is_null() {
        assert_eq!(selected_program_name(None), Ok(None));
    }

    #[test]
    fn picker_accepts_all_files_without_changing_the_extension() {
        assert_eq!(
            selected_program_name(Some(Path::new(r"D:\Tools\helper.cmd"))),
            Ok(Some("helper.cmd".into()))
        );
    }

    #[test]
    fn picker_rejects_paths_without_a_filename() {
        assert!(selected_program_name(Some(Path::new(r"C:\"))).is_err());
    }

    #[test]
    fn settings_window_is_portrait_and_borderless() {
        let config = settings_window_config();
        assert_eq!((config.width, config.height), (600.0, 800.0));
        assert_eq!(
            (config.min_width, config.min_height),
            (Some(420.0), Some(500.0))
        );
        assert!(!config.decorations);
        assert!(config.shadow);
        assert!(config.resizable);
        assert_eq!(config.label, "settings");
    }

    #[test]
    fn picker_returns_only_the_selected_filename() {
        assert_eq!(
            selected_program_name(Some(Path::new(r"C:\Program Files\绘画\Paint.EXE"))),
            Ok(Some("Paint.EXE".into()))
        );
    }
}

#[cfg(test)]
mod startup_tests {
    use super::should_start_silently;

    #[test]
    fn silent_start_applies_to_manual_and_autostart() {
        assert!(!should_start_silently(false, false));
        assert!(should_start_silently(false, true));
        assert!(!should_start_silently(true, false));
        assert!(should_start_silently(true, true));
    }
}
