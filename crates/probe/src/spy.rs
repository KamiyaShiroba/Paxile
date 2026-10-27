//! Probe side of paxile-spy: a named-pipe server that receives the DLL's
//! JSON lines, and per-thread installation of its two message hooks.
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE, HMODULE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_NOWAIT,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, HOOKPROC, WH_CALLWNDPROC, WH_GETMESSAGE,
};

pub struct Spy {
    pub dll: String,
    hmod: HMODULE,
    getmsg: HOOKPROC,
    callwnd: HOOKPROC,
    hooks: HashMap<u32, Vec<HHOOK>>,
    rx: Receiver<String>,
    stop: Arc<AtomicBool>,
}

impl Spy {
    pub fn start(
        pids: Vec<u32>,
        policy: paxile_policy::Config,
        control: crate::control::Shared,
    ) -> Result<Spy, String> {
        let dll = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name("paxile_spy.dll");
        if !dll.exists() {
            return Err(format!(
                "{} not found (cargo build the workspace)",
                dll.display()
            ));
        }
        let wide: Vec<u16> = dll.as_os_str().encode_wide_nul();
        unsafe {
            let hmod =
                LoadLibraryW(PCWSTR(wide.as_ptr())).map_err(|e| format!("LoadLibrary: {e}"))?;
            let getmsg = GetProcAddress(hmod, s!("paxile_getmsg_proc"))
                .ok_or("missing paxile_getmsg_proc")?;
            let callwnd = GetProcAddress(hmod, s!("paxile_callwnd_proc"))
                .ok_or("missing paxile_callwnd_proc")?;
            let (tx, rx) = channel();
            let stop = Arc::new(AtomicBool::new(false));
            let server_stop = stop.clone();
            std::thread::spawn(move || serve(tx, pids, policy, server_stop, control));
            Ok(Spy {
                dll: dll.display().to_string(),
                hmod,
                getmsg: std::mem::transmute::<unsafe extern "system" fn() -> isize, HOOKPROC>(
                    getmsg,
                ),
                callwnd: std::mem::transmute::<unsafe extern "system" fn() -> isize, HOOKPROC>(
                    callwnd,
                ),
                hooks: HashMap::new(),
                rx,
                stop,
            })
        }
    }

    /// Hooks any thread in `tids` not hooked yet. Returns (tid, error) for failures.
    pub fn hook_threads(&mut self, tids: &[u32]) -> Vec<(u32, Option<String>)> {
        let mut out = Vec::new();
        for &tid in tids {
            if self.hooks.contains_key(&tid) {
                continue;
            }
            let inst = HINSTANCE(self.hmod.0);
            let mut hs = Vec::new();
            let mut err = None;
            for (id, proc_) in [(WH_GETMESSAGE, self.getmsg), (WH_CALLWNDPROC, self.callwnd)] {
                match unsafe { SetWindowsHookExW(id, proc_, inst, tid) } {
                    Ok(h) => hs.push(h),
                    Err(e) => err = Some(e.to_string()),
                }
            }
            self.hooks.insert(tid, hs);
            out.push((tid, err));
        }
        out
    }

    pub fn drain(&self) -> Vec<String> {
        self.rx.try_iter().collect()
    }

    pub fn unhook_all(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The DLL owner-thread timer restores contexts when the lease stops.
        // The worker pins the DLL until every such timer has retired.
        std::thread::sleep(std::time::Duration::from_millis(1200));
        for (_, hs) in self.hooks.drain() {
            for h in hs {
                let _ = unsafe { UnhookWindowsHookEx(h) };
            }
        }
    }
}

trait EncodeWideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl EncodeWideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

/// Accept loop: one reader thread per connected DLL instance.
fn serve(
    tx: Sender<String>,
    pids: Vec<u32>,
    policy: paxile_policy::Config,
    stop: Arc<AtomicBool>,
    control: crate::control::Shared,
) {
    // Watch and transport tests initialize synchronously before exposing control.
    let _ = policy;
    while !stop.load(Ordering::Acquire) {
        let h = unsafe {
            CreateNamedPipeW(
                w!(r"\\.\pipe\paxile-spy"),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                1 << 20,
                0,
                None,
            )
        };
        if h.is_invalid() {
            let _ = tx.send(r#"{"pipe_error":"CreateNamedPipe failed"}"#.into());
            return;
        }
        // ERROR_PIPE_CONNECTED (client won the race) also means connected.
        loop {
            if stop.load(Ordering::Acquire) {
                let _ = unsafe { CloseHandle(h) };
                return;
            }
            match unsafe { ConnectNamedPipe(h, None) } {
                Ok(()) => break,
                Err(e) if e.code() == windows::core::HRESULT::from_win32(535) => break,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        // Use nonblocking mode only for the cancellable accept. Connected
        // sessions retain byte-pipe write semantics (no short NOWAIT frames).
        if unsafe {
            windows::Win32::System::Pipes::SetNamedPipeHandleState(
                h,
                Some(&PIPE_READMODE_BYTE),
                None,
                None,
            )
        }
        .is_err()
        {
            let _ = unsafe { CloseHandle(h) };
            continue;
        }
        let mut pid = 0;
        let current_policy = control.lock().unwrap().policy();
        if unsafe { GetNamedPipeClientProcessId(h, &mut pid) }.is_err() {
            let _ = unsafe { CloseHandle(h) };
            continue;
        }
        let allowed = pids.contains(&pid)
            || crate::win::processes().iter().any(|(id, exe)| {
                *id == pid
                    && current_policy.presets.iter().any(|p| {
                        (current_policy.profile == "auto" || current_policy.profile == p.name)
                            && p.executables.iter().any(|e| e.eq_ignore_ascii_case(exe))
                    })
            });
        if !allowed {
            let _ = unsafe { CloseHandle(h) };
            continue;
        }
        let payload = serde_json::to_vec(&current_policy).expect("config serialization");
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        if unsafe { WriteFile(h, Some(&frame), None, None) }.is_err() {
            let _ = unsafe { CloseHandle(h) };
            continue;
        }
        let raw = h.0 as usize;
        let tx2 = tx.clone();
        let connection_stop = stop.clone();
        let connection_control = control.clone();
        std::thread::spawn(move || {
            read_conn(
                HANDLE(raw as *mut _),
                tx2,
                connection_stop,
                connection_control,
                payload,
            )
        });
    }
}

fn read_conn(
    h: HANDLE,
    tx: Sender<String>,
    stop: Arc<AtomicBool>,
    control: crate::control::Shared,
    mut sent_policy: Vec<u8>,
) {
    let mut heartbeat_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut buf = vec![0u8; 64 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    let mut awaiting_config: Option<(u64, std::time::Instant)> = None;
    loop {
        let stopped = stop.load(Ordering::Acquire);
        if awaiting_config
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed().as_secs() >= 2)
        {
            let _ = tx.send(r#"{"runtime_unconfirmed":"DLL did not acknowledge config revision; unsupported older pinned DLL or unresponsive transport; no application is confirmed"}"#.into());
            awaiting_config = None;
        }
        if stopped || heartbeat_at.elapsed().as_millis() >= 100 {
            let guard = control.lock().expect("control mutex");
            let status = guard.status();
            let policy = serde_json::to_vec(&guard.policy()).unwrap();
            drop(guard);
            if !stopped && policy != sent_policy {
                let mut update = vec![3];
                update.extend_from_slice(&status.revision.to_le_bytes());
                update.extend_from_slice(&(policy.len() as u32).to_le_bytes());
                update.extend_from_slice(&policy);
                awaiting_config = Some((status.revision, std::time::Instant::now()));
                if unsafe { WriteFile(h, Some(&update), None, None) }.is_err() {
                    break;
                }
                sent_policy = policy;
            }
            let mut frame = [0u8; 9];
            frame[0] = if stopped {
                0
            } else if status.requested_enabled {
                1
            } else {
                2
            };
            frame[1..].copy_from_slice(&status.revision.to_le_bytes());
            if unsafe { WriteFile(h, Some(&frame), None, None) }.is_err() || stopped {
                break;
            }
            heartbeat_at = std::time::Instant::now();
        }
        let mut available = 0;
        if unsafe {
            windows::Win32::System::Pipes::PeekNamedPipe(
                h,
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        }
        .is_err()
        {
            break;
        }
        if available == 0 {
            if stop.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        let mut n = 0u32;
        if unsafe { ReadFile(h, Some(&mut buf), Some(&mut n), None) }.is_err() || n == 0 {
            break;
        }
        pending.extend_from_slice(&buf[..n as usize]);
        if pending.len() > 1024 * 1024 {
            break;
        }
        while let Some(i) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=i).collect();
            let s = String::from_utf8_lossy(&line[..line.len() - 1])
                .trim()
                .to_string();
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&s) {
                if event["backend"] == "config_received"
                    && awaiting_config
                        .as_ref()
                        .is_some_and(|(revision, _)| event["revision"].as_u64() == Some(*revision))
                {
                    awaiting_config = None;
                }
            }
            if !s.is_empty() && tx.send(s).is_err() {
                break;
            }
        }
    }
    if awaiting_config.is_some() {
        let _ = tx.send(r#"{"runtime_unconfirmed":"DLL disconnected before config revision receipt; older pinned DLL may not support live reload"}"#.into());
    }
    let _ = tx.send(r#"{"disconnected":true}"#.into());

    let _ = unsafe { CloseHandle(h) };
}
