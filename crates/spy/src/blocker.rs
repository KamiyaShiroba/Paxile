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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, VIRTUAL_KEY,
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
    /// Key the IME took over (`VK_PROCESSKEY`) and may have swallowed.
    key: Option<PendingKey>,
    /// Window that window belonged to.
    key_hwnd: usize,
    /// One-shot timer that settles a claimed key no composition answered.
    flush: usize,
}

/// The key that opened an IME composition. Only the scan code survives: the
/// virtual key of such a `WM_KEYDOWN` is `VK_PROCESSKEY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingKey {
    scan: u16,
    extended: bool,
    at: u64,
}

const KEY_REPLAY_WINDOW_MS: u64 = 400;
/// A text field emits `IMN_SETCANDIDATEPOS` within a millisecond of the key it
/// claims; waiting this long tells text apart from a canvas composition without
/// a visible delay.
const CLAIM_GRACE_MS: u64 = 20;
const REPLAY_MARKER: usize = 0x5041_5849; // "PAXI"

/// Replays the key the IME claimed for the composition we just cancelled.
/// Text intent (candidate evidence) never qualifies, and the key must belong to
/// this composition rather than an older one.
fn should_replay(key: Option<PendingKey>, now: u64, candidate: bool) -> Option<PendingKey> {
    if candidate {
        return None;
    }
    let key = key?;
    (now.saturating_sub(key.at) <= KEY_REPLAY_WINDOW_MS).then_some(key)
}

/// Re-sends the swallowed key with its original scan code. The IME must already
/// be detached, otherwise the replay would be eaten again.
fn replay(key: PendingKey) -> bool {
    let flags = |up: bool| {
        let mut value = KEYEVENTF_SCANCODE.0;
        if key.extended {
            value |= KEYEVENTF_EXTENDEDKEY.0;
        }
        if up {
            value |= KEYEVENTF_KEYUP.0;
        }
        KEYBD_EVENT_FLAGS(value)
    };
    let event = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: key.scan,
                dwFlags: flags(up),
                time: 0,
                dwExtraInfo: REPLAY_MARKER,
            },
        },
    };
    let input = [event(false), event(true)];
    unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) == input.len() as u32 }
}

fn clear_replay() {
    LOCAL.with(|s| {
        let mut s = s.borrow_mut();
        s.key = None;
        s.key_hwnd = 0;
    });
}

fn claim_expired(key: PendingKey, now: u64) -> bool {
    now.saturating_sub(key.at) >= CLAIM_GRACE_MS
}

fn stop_flush() {
    let timer = LOCAL.with(|s| std::mem::take(&mut s.borrow_mut().flush));
    if timer != 0 {
        unsafe {
            let _ = KillTimer(HWND::default(), timer);
        }
    }
}

/// Pins this module and arms the one-shot timer that settles a claimed key when
/// no composition message follows it.
fn ensure_flush() {
    if LOCAL.with(|s| s.borrow().flush) != 0 {
        return;
    }
    unsafe {
        let mut module = windows::Win32::Foundation::HMODULE::default();
        if windows::Win32::System::LibraryLoader::GetModuleHandleExW(
            0x5,
            windows::core::PCWSTR(flush_claimed as *const u16),
            &mut module,
        )
        .is_err()
        {
            return;
        }
        let timer = SetTimer(
            HWND::default(),
            0,
            CLAIM_GRACE_MS as u32,
            Some(flush_claimed),
        );
        if timer == 0 {
            // The extra module reference is intentionally kept: the timer may
            // still be pending, and a leaked pin only costs one refcount.
            return;
        }
        LOCAL.with(|s| s.borrow_mut().flush = timer);
    }
}

unsafe extern "system" fn flush_claimed(_: HWND, _: u32, _: usize, _: u32) {
    let _ = std::panic::catch_unwind(|| {
        sync_control();
        let now = GetTickCount64();
        let (key, hwnd) = LOCAL.with(|s| {
            let s = s.borrow();
            (s.key, s.key_hwnd)
        });
        let Some(key) = key else {
            stop_flush();
            return;
        };
        if !claim_expired(key, now) {
            return;
        }
        stop_flush();
        if !live() || hwnd == 0 {
            clear_replay();
            return;
        }
        // A key that vanished into an already-open composition never reports
        // WM_IME_STARTCOMPOSITION, so the claim itself has to settle it.
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_err()
            || gui.hwndFocus.0 as usize != hwnd
            || LOCAL.with(|s| s.borrow().evidence.recent(hwnd, now))
        {
            clear_replay();
            return;
        }
        decide_and_block(HWND(hwnd as *mut _), now, "claimed_key");
    });
}

/// Applies the policy to `hwnd`, logs the verdict, and cancels/detaches the IME
/// when it says the focus is not a text context.
unsafe fn decide_and_block(hwnd: HWND, now: u64, source: &str) -> bool {
    let mut foreground_pid = 0;
    GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut foreground_pid));
    if foreground_pid != GetCurrentProcessId() {
        restore();
        clear_replay();
        stop_flush();
        return false;
    }
    let mut gui = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_err() {
        restore();
        clear_replay();
        stop_flush();
        return false;
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
    super::push(serde_json::json!({"backend":"decision","source":source,"block":block,"candidate":candidate,"caret":o.caret,"class":o.class,"tid":GetCurrentThreadId(),"hwnd":format!("{:#x}",hwnd.0 as usize)}).to_string());
    if !block {
        restore();
        clear_replay();
        stop_flush();
        return false;
    }
    if LOCAL.with(|s| s.borrow().detached.is_some()) {
        return false;
    }
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
            return false;
        }
        let timer = SetTimer(HWND::default(), 0, 100, Some(watchdog));
        if timer == 0 {
            return false;
        }
        LOCAL.with(|s| s.borrow_mut().timer = timer);
    }
    if !live() {
        return false;
    }
    let context = ImmGetContext(hwnd);
    if context.is_invalid() {
        return false;
    }
    // Cancellation may synchronously re-enter hooks; no LOCAL borrow is held.
    let cancelled = ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0).as_bool();
    let _ = ImmReleaseContext(hwnd, context);
    if !cancelled || !live() {
        return false;
    }
    // Re-check evidence after cancellation callbacks before changing context.
    if LOCAL.with(|s| {
        s.borrow()
            .evidence
            .recent(hwnd.0 as usize, GetTickCount64())
    }) {
        return false;
    }
    let replay_key = LOCAL.with(|s| should_replay(s.borrow().key, now, false));
    if detach(hwnd) {
        // Detach happens first so the replay reaches the application instead of
        // being claimed by the same IME again.
        if let Some(key) = replay_key {
            if replay(key) {
                super::push(serde_json::json!({"backend":"replayed","scan":key.scan,"extended":key.extended,"tid":GetCurrentThreadId(),"hwnd":format!("{:#x}",hwnd.0 as usize)}).to_string());
            }
        }
        clear_replay();
        stop_flush();
        return true;
    }
    false
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
pub fn observe(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, posted: bool) {
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
        let now = GetTickCount64();
        // The IME replaces the virtual key with VK_PROCESSKEY when it claims a
        // key; the scan code still identifies what the user pressed.
        if msg == 0x0100 {
            let scan = ((lp.0 >> 16) & 0xff) as u16;
            let extended = (lp.0 >> 24) & 1 != 0;
            let claimed = wp.0 == 0xe5 && scan != 0;
            LOCAL.with(|s| {
                let mut s = s.borrow_mut();
                s.key = claimed.then_some(PendingKey {
                    scan,
                    extended,
                    at: now,
                });
                s.key_hwnd = if claimed { hwnd.0 as usize } else { 0 };
            });
            if claimed {
                ensure_flush();
            } else {
                clear_replay();
                stop_flush();
            }
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
        if msg == 0x0282 && wp.0 == 9 && !gui.hwndFocus.0.is_null() {
            LOCAL.with(|s| {
                s.borrow_mut()
                    .evidence
                    .record(gui.hwndFocus.0 as usize, now)
            });
            // A delayed text signal overrides a prior non-text hypothesis.
            clear_replay();
            stop_flush();
            restore();
            return;
        }
        // Composition start is the second chance: the claim timer may already
        // have settled this key, and text fields arrive here with evidence.
        if msg == 0x010d && posted && gui.hwndFocus == hwnd {
            decide_and_block(hwnd, now, "composition_start");
        }
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
