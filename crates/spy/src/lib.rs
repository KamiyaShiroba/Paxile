//! paxile-spy: loaded into the target's UI thread(s) by paxile-probe via
//! SetWindowsHookEx(WH_GETMESSAGE / WH_CALLWNDPROC). Read-only unless a
//! validated probe config and live heartbeat explicitly enable blocking.
//! Messages are never altered or swallowed. See blocker.rs for IMM mutations.
//!
//! Hook procs only format a line and push it into a buffer; a worker thread
//! ships the buffer to the probe over a named pipe. The worker retains one
//! module reference for the target process lifetime and reconnects after the
//! controller goes away; owner-thread timers independently fail open.

mod blocker;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use serde_json::{json, Map, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, FreeLibrary, GENERIC_READ, GENERIC_WRITE, HMODULE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::LibraryLoader::{FreeLibraryAndExitThread, GetModuleHandleExW};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::Threading::{
    CreateThread, GetCurrentProcessId, GetCurrentThreadId, Sleep, THREAD_CREATION_FLAGS,
};
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmGetConversionStatus, ImmGetOpenStatus,
    ImmReleaseContext, GCS_COMPSTR, GCS_RESULTSTR, HIMC, IME_COMPOSITION_STRING,
    IME_CONVERSION_MODE, IME_SENTENCE_MODE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetClassNameW, CWPSTRUCT, HHOOK, MSG,
};

const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
const PM_REMOVE: usize = 0x1;
const MAX_BUFFERED: usize = 20_000;

const ST_IDLE: u8 = 0;
const ST_RUNNING: u8 = 1;
const ST_DEAD: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(ST_IDLE);
static BUF: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ---------------------------------------------------------------- hook procs

/// # Safety
/// Called by Windows with a valid hook payload for `code == HC_ACTION`.
#[no_mangle]
pub unsafe extern "system" fn paxile_getmsg_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Let the rest of the chain (e.g. TSF's own message hook) run first, so we
    // see the message as the app will: an IME-eaten key shows as VK_PROCESSKEY.
    let before = (code == 0 && wp.0 & PM_REMOVE != 0).then(|| *(lp.0 as *const MSG));
    let r = CallNextHookEx(HHOOK::default(), code, wp, lp);
    if let Some(b) = before {
        let after = *(lp.0 as *const MSG);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            blocker::observe(after.hwnd, after.message, after.wParam, after.lParam, true)
        }));
        let _ = catch_unwind(AssertUnwindSafe(|| {
            on_msg(
                "post",
                after.hwnd,
                after.message,
                after.wParam,
                after.lParam,
                Some(&b),
            )
        }));
    }
    r
}

/// # Safety
/// Called by Windows with a valid CWPSTRUCT for `code == HC_ACTION`.
#[no_mangle]
pub unsafe extern "system" fn paxile_callwnd_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == 0 {
        let c = *(lp.0 as *const CWPSTRUCT);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            blocker::observe(c.hwnd, c.message, c.wParam, c.lParam, false)
        }));
        let _ = catch_unwind(AssertUnwindSafe(|| {
            on_msg("send", c.hwnd, c.message, c.wParam, c.lParam, None)
        }));
    }
    CallNextHookEx(HHOOK::default(), code, wp, lp)
}

// ---------------------------------------------------------------- formatting

fn msg_name(m: u32) -> Option<&'static str> {
    Some(match m {
        0x0006 => "WM_ACTIVATE",
        0x0007 => "WM_SETFOCUS",
        0x0008 => "WM_KILLFOCUS",
        0x0050 => "WM_INPUTLANGCHANGEREQUEST",
        0x0051 => "WM_INPUTLANGCHANGE",
        0x0100 => "WM_KEYDOWN",
        0x0101 => "WM_KEYUP",
        0x0102 => "WM_CHAR",
        0x0104 => "WM_SYSKEYDOWN",
        0x0105 => "WM_SYSKEYUP",
        0x0106 => "WM_SYSCHAR",
        0x010D => "WM_IME_STARTCOMPOSITION",
        0x010E => "WM_IME_ENDCOMPOSITION",
        0x010F => "WM_IME_COMPOSITION",
        0x0201 => "WM_LBUTTONDOWN",
        0x0204 => "WM_RBUTTONDOWN",
        0x0246 => "WM_POINTERDOWN",
        0x0281 => "WM_IME_SETCONTEXT",
        0x0282 => "WM_IME_NOTIFY",
        // 0x0283 WM_IME_CONTROL is skipped: the probe's own state polling sends it.
        0x0284 => "WM_IME_COMPOSITIONFULL",
        0x0285 => "WM_IME_SELECT",
        0x0286 => "WM_IME_CHAR",
        0x0288 => "WM_IME_REQUEST",
        0x0290 => "WM_IME_KEYDOWN",
        0x0291 => "WM_IME_KEYUP",
        _ => return None,
    })
}

fn imn_name(n: usize) -> &'static str {
    match n {
        0x1 => "CLOSESTATUSWINDOW",
        0x2 => "OPENSTATUSWINDOW",
        0x3 => "CHANGECANDIDATE",
        0x4 => "CLOSECANDIDATE",
        0x5 => "OPENCANDIDATE",
        0x6 => "SETCONVERSIONMODE",
        0x7 => "SETSENTENCEMODE",
        0x8 => "SETOPENSTATUS",
        0x9 => "SETCANDIDATEPOS",
        0xA => "SETCOMPOSITIONFONT",
        0xB => "SETCOMPOSITIONWINDOW",
        0xC => "SETSTATUSWINDOWPOS",
        0xD => "GUIDELINE",
        0xE => "PRIVATE",
        _ => "?",
    }
}

fn class_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut b) };
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

fn comp_string(himc: HIMC, which: IME_COMPOSITION_STRING) -> Option<String> {
    unsafe {
        let bytes = ImmGetCompositionStringW(himc, which, None, 0);
        if bytes <= 0 {
            return None;
        }
        let mut buf = vec![0u16; bytes as usize / 2];
        let got = ImmGetCompositionStringW(
            himc,
            which,
            Some(buf.as_mut_ptr() as *mut c_void),
            bytes as u32,
        );
        (got > 0).then(|| String::from_utf16_lossy(&buf[..got as usize / 2]))
    }
}

/// Getter-only snapshot of the window's input context.
fn ime_state(h: HWND, msg: u32, lp: LPARAM, out: &mut Map<String, Value>) {
    unsafe {
        let himc = ImmGetContext(h);
        if himc.is_invalid() {
            out.insert("himc".into(), json!(false));
            return;
        }
        let mut conv = IME_CONVERSION_MODE(0);
        let mut sent = IME_SENTENCE_MODE(0);
        let _ = ImmGetConversionStatus(himc, Some(&mut conv), Some(&mut sent));
        out.insert("himc".into(), json!(true));
        out.insert("open".into(), json!(ImmGetOpenStatus(himc).as_bool()));
        out.insert("conv".into(), json!(conv.0));
        if msg == 0x010F {
            if lp.0 as u32 & GCS_COMPSTR.0 != 0 {
                if let Some(s) = comp_string(himc, GCS_COMPSTR) {
                    out.insert("comp".into(), json!(s));
                }
            }
            if lp.0 as u32 & GCS_RESULTSTR.0 != 0 {
                if let Some(s) = comp_string(himc, GCS_RESULTSTR) {
                    out.insert("result".into(), json!(s));
                }
            }
        }
        let _ = ImmReleaseContext(h, himc);
    }
}

fn on_msg(src: &str, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, before: Option<&MSG>) {
    if STATE.load(Ordering::Relaxed) == ST_DEAD {
        return;
    }
    let Some(name) = msg_name(msg) else { return };
    let mut qpc = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut qpc) };
    let class = class_of(hwnd);

    let mut r = Map::new();
    r.insert("qpc".into(), json!(qpc));
    r.insert("tid".into(), json!(unsafe { GetCurrentThreadId() }));
    r.insert("src".into(), json!(src));
    r.insert("msg".into(), json!(name));
    r.insert("hwnd".into(), json!(format!("{:#x}", hwnd.0 as usize)));
    r.insert("class".into(), json!(class));
    r.insert("wp".into(), json!(wp.0));
    r.insert("lp".into(), json!(lp.0));
    match msg {
        0x0100 | 0x0101 | 0x0104 | 0x0105 => {
            r.insert("vk".into(), json!(wp.0));
            if let Some(b) = before {
                if b.message != msg || b.wParam != wp {
                    // Something further down the hook chain rewrote the key.
                    r.insert("vk_orig".into(), json!(b.wParam.0));
                    r.insert("msg_orig".into(), json!(b.message));
                }
            }
        }
        0x0102 | 0x0106 | 0x0286 => {
            if let Some(c) = char::from_u32(wp.0 as u32) {
                r.insert("ch".into(), json!(c.to_string()));
            }
        }
        0x0282 => {
            r.insert("sub".into(), json!(imn_name(wp.0)));
        }
        _ => {}
    }
    if class != "IME" && class != "MSCTFIME UI" {
        ime_state(hwnd, msg, lp, &mut r);
    }
    push(Value::Object(r).to_string());
}

// ---------------------------------------------------------------- transport

fn push(line: String) {
    match STATE.load(Ordering::Acquire) {
        ST_DEAD => return,
        ST_IDLE => start_worker(),
        _ => {}
    }
    if let Ok(mut b) = BUF.lock() {
        if b.len() < MAX_BUFFERED {
            b.push(line);
        }
    }
}

fn start_worker() {
    if STATE
        .compare_exchange(ST_IDLE, ST_RUNNING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    unsafe {
        // Extra reference on ourselves, released by FreeLibraryAndExitThread.
        let mut hm = HMODULE::default();
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(start_worker as *const u16),
            &mut hm,
        )
        .is_err()
        {
            STATE.store(ST_DEAD, Ordering::Release);
            return;
        }
        match CreateThread(
            None,
            0,
            Some(worker),
            Some(hm.0 as *const c_void),
            THREAD_CREATION_FLAGS(0),
            None,
        ) {
            Ok(h) => {
                let _ = CloseHandle(h);
            }
            Err(_) => {
                STATE.store(ST_DEAD, Ordering::Release);
                let _ = FreeLibrary(hm);
            }
        }
    }
}

unsafe extern "system" fn worker(p: *mut c_void) -> u32 {
    // Keep the pinned DLL and worker alive across controller disconnects.
    let _ = catch_unwind(run_worker);
    STATE.store(ST_DEAD, Ordering::Release);
    if let Ok(mut b) = BUF.lock() {
        *b = Vec::new();
    }
    FreeLibraryAndExitThread(HMODULE(p), 0)
}

fn run_worker() {
    loop {
        let _ = catch_unwind(run_session);
        // Invalidate even after a malformed handshake or unexpected panic.
        blocker::LEASE.store(0, Ordering::Release);
        if let Ok(mut config) = blocker::CONFIG.lock() {
            *config = None;
            blocker::CONTROL.store(0, Ordering::Release);
            blocker::SESSION.fetch_add(1, Ordering::AcqRel);
        }
        if let Ok(mut buffer) = BUF.lock() {
            buffer.clear();
        }
        unsafe { Sleep(100) };
    }
}

fn run_session() {
    unsafe {
        let h = loop {
            if let Ok(h) = CreateFileW(
                w!(r"\\.\pipe\paxile-spy"),
                GENERIC_WRITE.0 | GENERIC_READ.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            ) {
                break h;
            }
            Sleep(500);
        };

        let mut length = [0u8; 4];
        if !read_exact(h, &mut length) {
            let _ = CloseHandle(h);
            return;
        }
        let length = u32::from_le_bytes(length) as usize;
        if length > 65536 {
            let _ = CloseHandle(h);
            return;
        }
        let mut payload = vec![0; length];
        if !read_exact(h, &mut payload) {
            let _ = CloseHandle(h);
            return;
        }
        let config = match serde_json::from_slice::<paxile_policy::Config>(&payload) {
            Ok(c) if c.validate().is_ok() => c,
            _ => {
                let _ = CloseHandle(h);
                return;
            }
        };
        let authorized = config.enabled;
        if let Ok(mut c) = blocker::CONFIG.lock() {
            *c = Some(config);
        }

        let hello =
            json!({ "hello": true, "pid": GetCurrentProcessId(), "tid": GetCurrentThreadId() });
        let mut out = format!("{hello}\n");
        let mut idle_ms = 0u32;
        let mut commands = Vec::new();
        loop {
            let mut available = 0;
            if windows::Win32::System::Pipes::PeekNamedPipe(
                h,
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
            .is_err()
            {
                break;
            }
            if available > 0 {
                let mut bytes = vec![0u8; available.min(1024) as usize];
                let mut n = 0;
                if ReadFile(h, Some(&mut bytes), Some(&mut n), None).is_err() || n == 0 {
                    break;
                }
                commands.extend_from_slice(&bytes[..n as usize]);
                let mut invalid = false;
                while commands.len() >= 9 {
                    if commands[0] == 3 {
                        if commands.len() < 13 {
                            break;
                        }
                        let length =
                            u32::from_le_bytes(commands[9..13].try_into().unwrap()) as usize;
                        if length > 65536 {
                            invalid = true;
                            break;
                        }
                        if commands.len() < 13 + length {
                            break;
                        }
                        let revision = u64::from_le_bytes(commands[1..9].try_into().unwrap());
                        let policy = serde_json::from_slice::<paxile_policy::Config>(
                            &commands[13..13 + length],
                        );
                        match policy {
                            Ok(mut policy)
                                if policy.validate().is_ok() && revision <= u64::MAX >> 1 =>
                            {
                                policy.enabled = authorized;
                                // Readers take CONFIG before evaluating policy. Publish the
                                // revision under that lock so owner threads restore first.
                                if let Ok(mut config) = blocker::CONFIG.lock() {
                                    *config = Some(policy);
                                    let enabled = blocker::CONTROL.load(Ordering::Acquire) & 1;
                                    blocker::CONTROL
                                        .store((revision << 1) | enabled, Ordering::Release);
                                }
                                push(
                                    json!({"backend":"config_received","revision":revision})
                                        .to_string(),
                                );
                            }
                            _ => {
                                invalid = true;
                                break;
                            }
                        }
                        commands.drain(..13 + length);
                        continue;
                    }
                    let frame: Vec<u8> = commands.drain(..9).collect();
                    if !matches!(frame[0], 1 | 2) {
                        invalid = true;
                        break;
                    }
                    let revision = u64::from_le_bytes(frame[1..].try_into().unwrap());
                    if revision > u64::MAX >> 1 {
                        invalid = true;
                        break;
                    }
                    let control = (revision << 1) | u64::from(frame[0] == 1 && authorized);
                    let previous = blocker::CONTROL.swap(control, Ordering::AcqRel);
                    if previous != control {
                        push(json!({"backend":"control_received","revision":revision,"enabled":frame[0] == 1 && authorized}).to_string());
                    }
                    blocker::LEASE.store(
                        windows::Win32::System::SystemInformation::GetTickCount64(),
                        Ordering::Release,
                    );
                }
                if invalid {
                    break;
                }
            }
            if !out.is_empty() {
                if WriteFile(h, Some(out.as_bytes()), None, None).is_err() {
                    break;
                }
                out.clear();
                idle_ms = 0;
            }
            Sleep(20);
            idle_ms += 20;
            if let Ok(mut b) = BUF.lock() {
                for l in b.drain(..) {
                    out.push_str(&l);
                    out.push('\n');
                }
            }
            // Heartbeat: an empty line, so a vanished probe is noticed quickly.
            if out.is_empty() && idle_ms >= 500 {
                out.push('\n');
            }
        }
        blocker::LEASE.store(0, Ordering::Release);
        let _ = CloseHandle(h);
    }
}

fn read_exact(h: windows::Win32::Foundation::HANDLE, mut bytes: &mut [u8]) -> bool {
    // An accepted but silent/partial peer must not strand the only worker.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !bytes.is_empty() {
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
            || std::time::Instant::now() >= deadline
        {
            return false;
        }
        if available == 0 {
            unsafe { Sleep(10) };
            continue;
        }
        let count = bytes.len().min(available as usize);
        let mut n = 0;
        if unsafe { ReadFile(h, Some(&mut bytes[..count]), Some(&mut n), None) }.is_err() || n == 0
        {
            return false;
        }
        bytes = &mut bytes[n as usize..];
    }
    true
}
