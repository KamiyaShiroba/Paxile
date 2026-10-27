//! IMM mutations are confined to hook/timer callbacks on the window owner thread.
#[cfg(test)]
mod tests;
use paxile_policy::{CandidateEvidence, Config, Decision, Observation};
use std::cell::RefCell;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::Ime::{
    ImmAssociateContext, ImmGetContext, ImmNotifyIME, ImmReleaseContext, CPS_CANCEL, HIMC,
    NI_COMPOSITIONSTR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow, KillTimer, SetTimer,
    GUITHREADINFO,
};

pub static CONFIG: Mutex<Option<Config>> = Mutex::new(None);
pub static LEASE: AtomicU64 = AtomicU64::new(0);
// revision << 1 | requested_enabled; distinct from the connection lease.
pub static CONTROL: AtomicU64 = AtomicU64::new(0);
// Incremented for every transport session; prevents stale connection state
// from being mistaken for a live lease by owner-thread callbacks.
pub static SESSION: AtomicU64 = AtomicU64::new(0);
thread_local! { static LOCAL: RefCell<Local> = RefCell::new(Local::default()); }
#[derive(Default)]
struct Local {
    session: u64,
    control: u64,
    evidence: CandidateEvidence,
    detached: Option<(usize, usize)>,
    timer: usize,
}
fn live() -> bool {
    let lease = LEASE.load(Ordering::Acquire);
    CONTROL.load(Ordering::Acquire) & 1 != 0
        && lease != 0
        && unsafe { GetTickCount64() }.saturating_sub(lease) < 750
}
fn sync_control() {
    let session = SESSION.load(Ordering::Acquire);
    let control = CONTROL.load(Ordering::Acquire);
    let changed = LOCAL.with(|s| {
        let mut s = s.borrow_mut();
        if s.session == session && s.control == control {
            return false;
        }
        s.session = session;
        s.control = control;
        s.evidence.clear();
        true
    });
    if changed {
        restore();
    }
}
fn take_detached() -> Option<(usize, usize)> {
    LOCAL.with(|s| s.borrow_mut().detached.take())
}
fn restore() {
    if let Some((h, c)) = take_detached() {
        unsafe {
            let hwnd = HWND(h as *mut _);
            if IsWindow(hwnd).as_bool()
                && GetWindowThreadProcessId(hwnd, None) == GetCurrentThreadId()
            {
                ImmAssociateContext(hwnd, HIMC(c as *mut _));
                super::push(serde_json::json!({"backend":"restored","tid":GetCurrentThreadId(),"hwnd":format!("{h:#x}")}).to_string());
            }
        }
    }
}
unsafe extern "system" fn watchdog(_: HWND, _: u32, _: usize, _: u32) {
    let _ = std::panic::catch_unwind(|| {
        sync_control();
        let should_restore = !live()
            || unsafe {
                let mut pid = 0;
                GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
                let mut gui = GUITHREADINFO {
                    cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                    ..Default::default()
                };
                let known = GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_ok();
                let detached = LOCAL.with(|s| s.borrow().detached.map(|(h, _)| h));
                pid != GetCurrentProcessId()
                    || !known
                    || !gui.hwndCaret.0.is_null()
                    || detached.is_some_and(|h| h != gui.hwndFocus.0 as usize)
            };
        if should_restore {
            restore();
        }
        if !live() {
            let timer = LOCAL.with(|s| {
                let mut s = s.borrow_mut();
                s.evidence.clear();
                std::mem::take(&mut s.timer)
            });
            if timer != 0 {
                let _ = KillTimer(HWND::default(), timer);
            }
        }
    });
}

/// Called after the rest of WH_GETMESSAGE, and at WH_CALLWNDPROC for sent
/// candidate/focus notifications. No message is swallowed or rewritten.
pub fn observe(hwnd: HWND, msg: u32, wp: WPARAM, _lp: LPARAM, posted: bool) {
    sync_control();
    if !live() {
        LOCAL.with(|s| s.borrow_mut().evidence.clear());
        restore();
        return;
    }
    unsafe {
        if hwnd.0.is_null() || GetWindowThreadProcessId(hwnd, None) != GetCurrentThreadId() {
            return;
        }
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_err() {
            restore();
            return;
        }
        // Restore before the application can enter a different text context.
        if matches!(
            msg,
            0x0007 | 0x0008 | 0x0201 | 0x0204 | 0x0246 | 0x0082 | 0x0051
        ) || (msg == 0x001c && wp.0 == 0)
            || (msg == 0x0006 && wp.0 & 0xffff == 0)
            || (msg == 0x0281 && wp.0 == 0)
        {
            restore();
            LOCAL.with(|s| s.borrow_mut().evidence.clear());
            return;
        }
        let now = GetTickCount64();
        if msg == 0x0282 && wp.0 == 9 && !gui.hwndFocus.0.is_null() {
            LOCAL.with(|s| {
                s.borrow_mut()
                    .evidence
                    .record(gui.hwndFocus.0 as usize, now)
            });
            // A delayed text signal overrides a prior non-text hypothesis.
            restore();
            return;
        }
        // CSP evidence currently covers queued starts only. Sent starts fail open.
        if msg != 0x010d || !posted || gui.hwndFocus != hwnd {
            return;
        }
        let mut foreground_pid = 0;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut foreground_pid));
        if foreground_pid != GetCurrentProcessId() {
            restore();
            return;
        }
        let candidate = LOCAL.with(|s| s.borrow().evidence.recent(hwnd.0 as usize, now));
        let o = Observation {
            class: super::class_of(hwnd),
            caret: !gui.hwndCaret.0.is_null(),
            candidate,
        };
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let block = CONFIG
            .lock()
            .ok()
            .and_then(|c| {
                c.as_ref().map(|c| {
                    // Serialize with worker publication: restore the old context
                    // before evaluating a policy received during this observation.
                    sync_control();
                    let current = Observation {
                        class: o.class.clone(),
                        caret: o.caret,
                        candidate: LOCAL.with(|s| {
                            s.borrow()
                                .evidence
                                .recent(hwnd.0 as usize, GetTickCount64())
                        }),
                    };
                    live() && c.decide(&exe, &current) == Decision::Block
                })
            })
            .unwrap_or(false);
        super::push(serde_json::json!({"backend":"decision","block":block,"candidate":candidate,"caret":o.caret,"class":o.class,"tid":GetCurrentThreadId(),"hwnd":format!("{:#x}",hwnd.0 as usize)}).to_string());
        if !block {
            restore();
            return;
        }
        if LOCAL.with(|s| s.borrow().detached.is_some()) {
            return;
        }
        // Pin the module for the process lifetime before registering a callback.
        // A hung UI thread can retain its timer beyond probe shutdown/unhook.
        let timer = LOCAL.with(|s| s.borrow().timer);
        if timer == 0 {
            let mut module = windows::Win32::Foundation::HMODULE::default();
            if windows::Win32::System::LibraryLoader::GetModuleHandleExW(
                0x5,
                windows::core::PCWSTR(watchdog as *const u16),
                &mut module,
            )
            .is_err()
            {
                return;
            }
            let timer = SetTimer(HWND::default(), 0, 100, Some(watchdog));
            if timer == 0 {
                return;
            }
            LOCAL.with(|s| s.borrow_mut().timer = timer);
        }
        if !live() {
            return;
        }
        let context = ImmGetContext(hwnd);
        if context.is_invalid() {
            return;
        }
        // Cancellation may synchronously re-enter hooks; no LOCAL borrow is held.
        let cancelled = ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0).as_bool();
        let _ = ImmReleaseContext(hwnd, context);
        if !cancelled || !live() {
            return;
        }
        // Re-check evidence after cancellation callbacks before changing context.
        if LOCAL.with(|s| {
            s.borrow()
                .evidence
                .recent(hwnd.0 as usize, GetTickCount64())
        }) {
            return;
        }
        detach(hwnd);
    }
}

fn detach(hwnd: HWND) -> bool {
    sync_control();
    unsafe {
        if !live()
            || GetWindowThreadProcessId(hwnd, None) != GetCurrentThreadId()
            || LOCAL.with(|s| s.borrow().detached.is_some())
        {
            return false;
        }
        let previous = ImmAssociateContext(hwnd, HIMC::default());
        if previous.is_invalid() {
            return false;
        }
        LOCAL.with(|s| s.borrow_mut().detached = Some((hwnd.0 as usize, previous.0 as usize)));
        super::push(serde_json::json!({"backend":"detached","tid":GetCurrentThreadId(),"hwnd":format!("{:#x}",hwnd.0 as usize),"replay":false}).to_string());
        true
    }
}
