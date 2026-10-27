use crate::AppState;
use serde::Serialize;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub msg: String,
}
#[derive(Default)]
pub struct LogBuffer {
    pub entries: VecDeque<LogEntry>,
}
impl LogBuffer {
    pub fn push(&mut self, level: &str, msg: &str) -> LogEntry {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        let entry = LogEntry {
            time,
            level: level.into(),
            msg: msg.chars().take(4096).collect(),
        };
        if self.entries.len() == 500 {
            self.entries.pop_front();
        }
        self.entries.push_back(entry.clone());
        entry
    }
}
#[derive(Clone, Default, Serialize)]
pub struct CoreStatus {
    pub running: bool,
    pub requested_enabled: Option<bool>,
    pub applied_enabled: Option<bool>,
    pub error: Option<String>,
}
pub fn log(app: &AppHandle, level: &str, msg: &str) {
    if level == "error"
        || msg.contains("spy disabled:")
        || msg.contains("control unavailable:")
        || msg.contains("runtime_unconfirmed")
    {
        *app.state::<AppState>().core_error.lock().unwrap() =
            Some(msg.chars().take(4096).collect());
    }
    let entry = app
        .state::<AppState>()
        .logs
        .lock()
        .unwrap()
        .push(level, msg);
    let _ = app.emit("paxile-log", entry);
}
fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}
pub fn start(app: &AppHandle) -> Result<Child, String> {
    let probe = crate::probe_path(app).ok_or("找不到匹配的引擎 EXE / DLL")?;
    if !probe.with_file_name("paxile_spy.dll").is_file() {
        return Err("缺少 paxile_spy.dll".into());
    }
    let mut command = Command::new(probe);
    command
        .args([
            "watch",
            "--managed",
            "--spy",
            "--block",
            "--no-file",
            "--config",
        ])
        .arg(crate::config::config_path(app)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hidden(&mut command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    for (reader, level) in [
        (Box::new(stdout) as Box<dyn std::io::Read + Send>, "info"),
        (Box::new(stderr) as Box<dyn std::io::Read + Send>, "error"),
    ] {
        let app = app.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut line = Vec::new();
            loop {
                match reader.fill_buf() {
                    Ok([]) | Err(_) => break,
                    Ok(bytes) => {
                        let n = bytes
                            .iter()
                            .position(|b| *b == b'\n')
                            .map(|i| i + 1)
                            .unwrap_or(bytes.len());
                        let complete = bytes[n - 1] == b'\n';
                        let remaining = 4096usize.saturating_sub(line.len());
                        line.extend_from_slice(&bytes[..n.min(remaining)]);
                        reader.consume(n);
                        if complete {
                            log(&app, level, String::from_utf8_lossy(&line).trim());
                            line.clear();
                        }
                    }
                }
            }
        });
    }
    log(app, "info", "引擎进程已启动；等待控制通道就绪。");
    Ok(child)
}
pub fn control(app: &AppHandle, pid: u32, action: &str) -> Result<serde_json::Value, String> {
    control_with_config(app, pid, action, None)
}
fn control_with_config(
    app: &AppHandle,
    pid: u32,
    action: &str,
    config: Option<&std::path::Path>,
) -> Result<serde_json::Value, String> {
    let mut command = Command::new(crate::probe_path(app).ok_or("找不到引擎")?);
    command.args(["control", action, "--session", &pid.to_string()]);
    if action == "reload" {
        command
            .arg("--config")
            .arg(config.ok_or("reload requires staged configuration")?);
    }
    hidden(&mut command);
    let output = command.output().map_err(|e| e.to_string())?;
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| String::from_utf8_lossy(&output.stderr).trim().to_string())?;
    if !output.status.success() || response["ok"] != true {
        return Err(response["error"].as_str().unwrap_or("引擎请求失败").into());
    }
    Ok(response)
}
pub fn update(app: &AppHandle, reload: bool, config: &paxile_policy::Config) -> Result<(), String> {
    let enabled = config.enabled;
    let state = app.state::<AppState>();
    let mut backend = state.backend.lock().unwrap();
    let result = (|| {
        let child = backend.as_mut().ok_or("引擎未运行，请重新启动 Paxile")?;
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("引擎已退出：{status}"));
        }
        if reload {
            let before = control(app, child.id(), "status")?;
            let path = crate::config::config_path(app)?;
            crate::config::with_staged_config(&path, config, |staged| {
                let response = control_with_config(app, child.id(), "reload", Some(staged))?;
                validate_reload(&before, &response)
            })?;
        }
        let response = control(app, child.id(), if enabled { "on" } else { "off" })?;
        if response["status"]["requested_enabled"] != enabled {
            return Err("引擎未接受开关状态".into());
        }
        Ok(())
    })();
    *state.core_error.lock().unwrap() = result.as_ref().err().cloned();
    match &result {
        Ok(()) => log(
            app,
            "info",
            "引擎已接收请求；DLL 接收与 UI 线程应用见后续日志，不代表所有窗口已应用。",
        ),
        Err(e) => log(app, "error", e),
    }
    result
}
pub fn cached_status(state: &AppState) -> CoreStatus {
    state.core_status.lock().unwrap().clone()
}

fn snapshot_from_status(
    running: bool,
    response: Result<serde_json::Value, String>,
    prior_error: Option<String>,
) -> CoreStatus {
    let (requested_enabled, error) = match response {
        Ok(value) => match value["status"]["requested_enabled"].as_bool() {
            Some(enabled) => (Some(enabled), prior_error),
            None => (None, Some("invalid engine status".into())),
        },
        Err(error) => (None, Some(error)),
    };
    CoreStatus {
        running,
        requested_enabled,
        applied_enabled: None,
        error,
    }
}

pub fn refresh_status(app: &AppHandle) -> CoreStatus {
    let state = app.state::<AppState>();
    // A poll must not interleave with reload/on/off/persist/compensation.
    let _update = state.update_lock.lock().unwrap();
    let mut guard = state.backend.lock().unwrap();
    let running = match guard.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(None) => true,
            Ok(Some(exit)) => {
                *state.core_error.lock().unwrap() = Some(format!("引擎已退出：{exit}"));
                false
            }
            Err(e) => {
                *state.core_error.lock().unwrap() = Some(e.to_string());
                false
            }
        },
        None => false,
    };
    let pid = guard.as_ref().map(Child::id);
    drop(guard);
    let error = state.core_error.lock().unwrap().clone();
    let snapshot = if running {
        snapshot_from_status(true, control(app, pid.unwrap(), "status"), error)
    } else {
        CoreStatus {
            error,
            ..CoreStatus::default()
        }
    };
    *state.core_status.lock().unwrap() = snapshot.clone();
    snapshot
}
fn validate_reload(before: &serde_json::Value, after: &serde_json::Value) -> Result<(), String> {
    let old = before["status"]["revision"]
        .as_u64()
        .ok_or("missing engine revision")?;
    let new = after["status"]["revision"]
        .as_u64()
        .ok_or("reload unsupported: missing revision")?;
    if before["status"]["authorized"].as_bool().is_none()
        || before["status"]["requested_enabled"].as_bool().is_none()
        || new <= old
        || after["status"]["authorized"].as_bool() != before["status"]["authorized"].as_bool()
        || after["status"]["requested_enabled"].as_bool()
            != before["status"]["requested_enabled"].as_bool()
    {
        return Err("engine did not confirm policy reload without changing startup authorization/control state".into());
    }
    Ok(())
}

pub fn stop(app: &AppHandle) {
    let state = app.state::<AppState>();
    let _update = state.update_lock.lock().unwrap();
    let child = state.backend.lock().unwrap().take();
    if let Some(mut child) = child {
        let _ = control(app, child.id(), "off");
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_status_keeps_running_but_never_claims_disabled_or_applied() {
        let snapshot = snapshot_from_status(true, Err("control timed out".into()), None);
        assert!(snapshot.running);
        assert_eq!(snapshot.requested_enabled, None);
        assert_eq!(snapshot.applied_enabled, None);
        assert_eq!(snapshot.error.as_deref(), Some("control timed out"));
        let malformed = snapshot_from_status(true, Ok(serde_json::json!({"status": {}})), None);
        assert_eq!(malformed.requested_enabled, None);
        assert!(malformed.error.is_some());
        let off = snapshot_from_status(
            true,
            Ok(serde_json::json!({"status":{"requested_enabled":false}})),
            None,
        );
        assert_eq!(off.requested_enabled, Some(false));
        assert_eq!(off.applied_enabled, None);
    }
    #[test]
    fn cached_reads_do_not_wait_for_a_delayed_control_transaction() {
        let state = std::sync::Arc::new(AppState::default());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            let _backend = worker_state.backend.lock().unwrap();
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.recv().unwrap();
        let reader_state = state.clone();
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for _ in 0..100 {
                let snapshot = cached_status(&reader_state);
                assert!(!snapshot.running);
                assert_eq!(snapshot.requested_enabled, None);
                assert_eq!(snapshot.applied_enabled, None);
            }
            read_tx.send(()).unwrap();
        });
        let completed = read_rx.recv_timeout(std::time::Duration::from_millis(250));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        reader.join().unwrap();
        assert!(
            completed.is_ok(),
            "cached reads waited for the control/backend lock"
        );
    }
    #[test]
    fn reload_requires_revision_progress_and_preserves_authorization() {
        let before = serde_json::json!({"status":{"revision":4,"authorized":false,"requested_enabled":false}});
        let mut after = before.clone();
        assert!(validate_reload(&before, &after).is_err());
        after["status"]["revision"] = 5.into();
        assert!(validate_reload(&before, &after).is_ok());
        after["status"]["authorized"] = true.into();
        assert!(validate_reload(&before, &after).is_err());
        assert!(validate_reload(&before, &serde_json::json!({"ok":true})).is_err());
    }
    #[test]
    fn log_history_is_bounded_and_entries_are_real() {
        let mut logs = LogBuffer::default();
        for n in 0..600 {
            logs.push("info", &format!("line {n}"));
        }
        assert_eq!(logs.entries.len(), 500);
        assert_eq!(logs.entries.front().unwrap().msg, "line 100");
        assert_eq!(logs.entries.back().unwrap().msg, "line 599");
        assert!(!logs.entries[0].time.is_empty());
    }
}
