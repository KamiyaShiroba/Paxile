#[cfg(test)]
mod tests;

use paxile_policy::{BackendControl, ControlCommand};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PeekNamedPipe, PIPE_NOWAIT,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};

pub type Shared = Arc<Mutex<BackendControl>>;
fn name(pid: u32) -> String {
    format!(r"\\.\pipe\paxile-control-{pid}")
}
pub fn execute(state: &Shared, command: &str) -> Value {
    if let Some(payload) = command.strip_prefix("reload ") {
        let result = serde_json::from_str::<paxile_policy::Config>(payload)
            .map_err(|e| e.to_string())
            .and_then(|policy| {
                state
                    .lock()
                    .map_err(|_| "control unavailable".to_string())?
                    .reload(policy)
            });
        return match result {
            Ok(()) => json!({"ok":true,"status":state.lock().unwrap().status()}),
            Err(error) => json!({"ok":false,"error":error}),
        };
    }
    match command.parse::<ControlCommand>().and_then(|c| {
        state
            .lock()
            .map_err(|_| "control unavailable".to_string())?
            .execute(c)
    }) {
        Ok(status) => json!({"ok":true,"status":status}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}
pub struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub fn start(state: Shared) -> Result<Self, String> {
        let wide: Vec<u16> = name(std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let h = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                None,
            )
        };
        if h.is_invalid() {
            return Err("cannot create local control pipe".into());
        }
        let raw = h.0 as usize;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || unsafe {
            let h = HANDLE(raw as *mut _);
            while !stopped.load(Ordering::Acquire) {
                match ConnectNamedPipe(h, None) {
                    Ok(()) => {}
                    Err(e) if e.code() == windows::core::HRESULT::from_win32(535) => {}
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                }
                let deadline = Instant::now() + Duration::from_millis(500);
                let mut input = Vec::new();
                loop {
                    if stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
                        break;
                    }
                    let mut available = 0;
                    if PeekNamedPipe(h, None, 0, None, Some(&mut available), None).is_err() {
                        break;
                    }
                    if available > 0 {
                        let mut bytes = [0; 128];
                        let mut n = 0;
                        if ReadFile(h, Some(&mut bytes), Some(&mut n), None).is_err() {
                            break;
                        }
                        input.extend_from_slice(&bytes[..n as usize]);
                        if input.len() > 65536 {
                            break;
                        }
                        if input.last() == Some(&b'\n') {
                            let response = match std::str::from_utf8(&input[..input.len() - 1]) {
                                Ok(command) => execute(&state, command),
                                Err(_) => json!({"ok":false,"error":"invalid UTF-8"}),
                            };
                            let line = format!("{response}\n");
                            let _ = WriteFile(h, Some(line.as_bytes()), None, None);
                            // Keep connection until the client consumes the bounded response.
                            while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
                                if PeekNamedPipe(h, None, 0, None, None, None).is_err() {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = DisconnectNamedPipe(h);
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = CloseHandle(h);
        });
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub fn request(pid: u32, command: &str) -> Result<Value, String> {
    use std::io::{Read, Write};
    use std::os::windows::io::AsRawHandle;
    if command.len() > 65535 {
        return Err("control request too long".into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut file = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name(pid))
        {
            Ok(file) => break file,
            Err(e) if Instant::now() >= deadline => {
                return Err(format!("control session {pid} unavailable: {e}"))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    file.write_all(format!("{command}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    while Instant::now() < deadline {
        let mut available = 0;
        unsafe {
            PeekNamedPipe(
                HANDLE(file.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        if available > 0 {
            let mut bytes = [0; 1024];
            let n = file.read(&mut bytes).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("control disconnected".into());
            }
            response.extend_from_slice(&bytes[..n]);
            if response.len() > 4096 {
                return Err("control response too large".into());
            }
            if response.last() == Some(&b'\n') {
                return serde_json::from_slice(&response).map_err(|e| e.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("control request timed out".into())
}
pub fn cli(args: &[String]) -> std::process::ExitCode {
    if args == ["--help"] || args == ["-h"] {
        println!("usage: paxile-probe control on|off|toggle|status --session PROBE_PID");
        return std::process::ExitCode::SUCCESS;
    }
    if args.len() == 5 && args[0] == "reload" && args[1] == "--session" && args[3] == "--config" {
        let result = args[2]
            .parse::<u32>()
            .map_err(|e| e.to_string())
            .and_then(|pid| {
                let text = std::fs::read_to_string(&args[4]).map_err(|e| e.to_string())?;
                let config: paxile_policy::Config =
                    serde_json::from_str(&text).map_err(|e| e.to_string())?;
                config.validate()?;
                request(
                    pid,
                    &format!("reload {}", serde_json::to_string(&config).unwrap()),
                )
            });
        return match result {
            Ok(r) => {
                println!("{r}");
                if r["ok"] == true {
                    std::process::ExitCode::SUCCESS
                } else {
                    std::process::ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let parsed = if args.len() == 3 && args[1] == "--session" {
        args[0].parse::<ControlCommand>().and_then(|_| {
            args[2]
                .parse::<u32>()
                .map_err(|_| "invalid session PID".into())
        })
    } else {
        Err("usage: paxile-probe control on|off|toggle|status --session PROBE_PID".into())
    };
    let result = parsed.and_then(|pid| {
        if pid == 0 {
            Err("invalid session PID".into())
        } else {
            request(pid, &args[0])
        }
    });
    match result {
        Ok(response) => {
            println!("{response}");
            if response["ok"] == true {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::from(2)
        }
    }
}
