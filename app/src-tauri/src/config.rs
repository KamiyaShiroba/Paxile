//! Persistence for the shared engine config (paxile_policy::Config).
//! The file lives in the OS app-config dir so the engine can read the same one.

use paxile_policy::Config;
use std::path::PathBuf;
use tauri::Manager;

pub fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("config.json"))
}

/// Loads the config, falling back to defaults on any missing/invalid file so
/// the UI always opens.
pub fn load(app: &tauri::AppHandle) -> Config {
    config_path(app)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Config>(&b).ok())
        .filter(|c| c.validate().is_ok())
        .unwrap_or_default()
}

pub fn save(app: &tauri::AppHandle, config: &Config) -> Result<(), String> {
    let path = config_path(app)?;
    ensure_saved_at(&path, config)
}

pub fn ensure_saved_at(path: &std::path::Path, config: &Config) -> Result<(), String> {
    config.validate()?;
    if let Ok(bytes) = std::fs::read(path) {
        if serde_json::from_slice::<Config>(&bytes)
            .ok()
            .filter(|current| {
                serde_json::to_value(current).ok() == serde_json::to_value(config).ok()
            })
            .is_some()
        {
            return Ok(());
        }
    }
    with_staged_config(path, config, |staged| {
        std::fs::rename(staged, path).map_err(|e| e.to_string())
    })
}

/// Stage beside the destination: failed writes/renames leave the committed file intact.
pub fn with_staged_config<T>(
    path: &std::path::Path,
    config: &Config,
    operation: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    use std::io::Write;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    config.validate()?;
    let staged = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        operation(&staged)
    })();
    let _ = std::fs::remove_file(staged);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_preserves_valid_config_bytes() {
        let directory =
            std::env::temp_dir().join(format!("paxile-startup-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.json");
        let config = Config::default();
        let bytes = serde_json::to_vec(&config).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        ensure_saved_at(&path, &config).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, b"invalid").unwrap();
        ensure_saved_at(&path, &config).unwrap();
        assert!(serde_json::from_slice::<Config>(&std::fs::read(&path).unwrap()).is_ok());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn silent_start_defaults_on_and_preserves_explicit_off() {
        assert!(AppPreferences::default().silent_start);
        assert!(
            serde_json::from_str::<AppPreferences>("{}")
                .unwrap()
                .silent_start
        );
        assert!(
            !serde_json::from_str::<AppPreferences>(r#"{"silent_start":false}"#)
                .unwrap()
                .silent_start
        );
    }
    #[test]
    fn staging_failure_preserves_existing_file_and_success_replaces_it() {
        let directory =
            std::env::temp_dir().join(format!("paxile-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.json");
        std::fs::write(&path, b"previous").unwrap();
        let config = Config::default();
        let failed: Result<(), String> = with_staged_config(&path, &config, |staged| {
            assert!(staged.exists());
            Err("commit denied".into())
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        with_staged_config(&path, &config, |staged| {
            std::fs::rename(staged, &path).map_err(|e| e.to_string())
        })
        .unwrap();
        let saved: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!saved.enabled);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    pub silent_start: bool,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self { silent_start: true }
    }
}

fn preferences_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("preferences.json"))
}

pub fn load_preferences(app: &tauri::AppHandle) -> AppPreferences {
    preferences_path(app)
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_preferences(
    app: &tauri::AppHandle,
    preferences: &AppPreferences,
) -> Result<(), String> {
    let path = preferences_path(app)?;
    let bytes = serde_json::to_vec_pretty(preferences).map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}
