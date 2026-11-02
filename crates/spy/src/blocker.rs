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
    // Diagnostics only: never used to authorize a policy decision.
    candidate_source: Option<serde_json::Value>,
    detached: Option<(usize, usize)>,
    timer: usize,
}
thread_local! { static CONTEXT: RefCell<Option<(usize, u32, bool)>> = const { RefCell::new(None) }; }
fn diagnostic_context() -> serde_json::Value {
    let source = CONTEXT.with(|s| *s.borrow());
    let mut gui = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let known = unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_ok() };
    serde_json::json!({
        "source_hwnd":source.map(|(h,_,_)| format!("{h:#x}")),
        "source_msg":source.map(|(_,m,_)| m),
        "source_class":source.map(|(h,_,_)| super::class_of(HWND(h as *mut _))),
        "focus_hwnd":if known {Some(format!("{:#x}",gui.hwndFocus.0 as usize))} else {None},
        "phase":source.map(|(_,_,posted)| if posted {"getmessage_after_downstream"} else {"callwnd_before_downstream"}).unwrap_or("owner_thread_control_or_watchdog")
    })
}
fn candidate_origin(
    source: usize,
    focus: usize,
    class: &str,
    posted: bool,
    now: u64,
) -> serde_json::Value {
    serde_json::json!({"source_hwnd":format!("{source:#x}"),
        "focus_hwnd":format!("{focus:#x}"), "source_class":class,
        "source_msg":0x0282, "notify":9, "source_matches_focus":source == focus,
        "recorded_tick_ms":now, "recorded_qpc":super::qpc_now(),
        "phase":if posted {"getmessage_after_downstream"} else {"callwnd_before_downstream"}})
}
thread_local! { static IMM_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
static OPERATION_ID: AtomicU64 = AtomicU64::new(0);
pub(super) fn reentry_depth() -> u32 {
    IMM_DEPTH.with(|d| d.get())
}

pub(super) fn imm_operation<T: Copy + Into<serde_json::Value>>(
    name: &str,
    hwnd: HWND,
    operation: impl FnOnce() -> T,
) -> T {
    let id = OPERATION_ID.fetch_add(1, Ordering::Relaxed) + 1;
    let depth = IMM_DEPTH.with(|d| {
        let next = d.get() + 1;
        d.set(next);
        next
    });
    struct DepthReset;
    impl Drop for DepthReset {
        fn drop(&mut self) {
            IMM_DEPTH.with(|d| d.set(d.get() - 1));
        }
    }
    let _reset = DepthReset;
    let begin = super::qpc_now();
    super::push_value(
        serde_json::json!({"backend":"imm_operation", "operation":name, "operation_id":id,
        "phase":"begin", "begin_qpc":begin, "depth":depth, "hwnd":format!("{:#x}",hwnd.0 as usize)}),
    );
    let result = operation();
    let end = super::qpc_now();
    super::push_value(
        serde_json::json!({"backend":"imm_operation", "operation":name, "operation_id":id,
        "phase":"end", "begin_qpc":begin, "end_qpc":end, "depth":depth, "result":result.into(),
        "hwnd":format!("{:#x}",hwnd.0 as usize)}),
    );
    result
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
        s.candidate_source = None;
        true
    });
    if changed {
        restore("control_or_session_changed");
    }
}
fn take_detached() -> Option<(usize, usize)> {
    LOCAL.with(|s| s.borrow_mut().detached.take())
}
fn restore(reason: &str) {
    if let Some((h, c)) = take_detached() {
        unsafe {
            let hwnd = HWND(h as *mut _);
            if IsWindow(hwnd).as_bool()
                && GetWindowThreadProcessId(hwnd, None) == GetCurrentThreadId()
            {
                imm_operation("ImmAssociateContext_restore", hwnd, || {
                    ImmAssociateContext(hwnd, HIMC(c as *mut _)).0 as usize
                });
                let mut record = diagnostic_context();
                record["backend"] = serde_json::json!("restored");
                record["reason"] = serde_json::json!(reason);
                record["hwnd"] = serde_json::json!(format!("{h:#x}"));
                super::push(record.to_string());
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
            restore("watchdog_context_or_lease");
        }
        if !live() {
            let timer = LOCAL.with(|s| {
                let mut s = s.borrow_mut();
                s.evidence.clear();
                s.candidate_source = None;
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
    if msg == 0x0282 {
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let valid = unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut gui).is_ok() };
        let mut origin = candidate_origin(
            hwnd.0 as usize,
            gui.hwndFocus.0 as usize,
            &super::class_of(hwnd),
            posted,
            unsafe { GetTickCount64() },
        );
        origin["backend"] = serde_json::json!("candidate_notify_origin");
        origin["notify"] = serde_json::json!(wp.0);
        origin["gui_valid"] = serde_json::json!(valid);
        origin["reentry_depth"] = serde_json::json!(reentry_depth());
        if !valid {
            origin["focus_hwnd"] = serde_json::Value::Null;
            origin["source_matches_focus"] = serde_json::Value::Null;
        }
        super::push_value(origin);
    }
    let previous = CONTEXT.with(|s| s.replace(Some((hwnd.0 as usize, msg, posted))));
    struct Reset(Option<(usize, u32, bool)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CONTEXT.with(|s| {
                s.replace(self.0);
            });
        }
    }
    let _reset = Reset(previous);
    observe_inner(hwnd, msg, wp, lp, posted);
}

fn observe_inner(hwnd: HWND, msg: u32, wp: WPARAM, _lp: LPARAM, posted: bool) {
    sync_control();
    if !live() {
        LOCAL.with(|s| {
            let mut s = s.borrow_mut();
            s.evidence.clear();
            s.candidate_source = None;
        });
        restore("lease_inactive");
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
            restore("gui_query_failed");
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
            restore(match msg {
                0x0007 => "set_focus",
                0x0008 => "kill_focus",
                0x0201 => "left_click",
                0x0204 => "right_click",
                0x0246 => "pointer_down",
                0x0082 => "window_destroyed",
                0x0051 => "input_language_changed",
                0x001c => "application_deactivated",
                0x0006 => "window_deactivated",
                _ => "ime_context_deactivated",
            });
            LOCAL.with(|s| {
                let mut s = s.borrow_mut();
                s.evidence.clear();
                s.candidate_source = None;
            });
            return;
        }
        let now = GetTickCount64();
        if msg == 0x0282 && wp.0 == 9 && !gui.hwndFocus.0.is_null() {
            LOCAL.with(|s| {
                s.borrow_mut()
                    .evidence
                    .record(gui.hwndFocus.0 as usize, now);
                s.borrow_mut().candidate_source = Some(candidate_origin(
                    hwnd.0 as usize,
                    gui.hwndFocus.0 as usize,
                    &super::class_of(hwnd),
                    posted,
                    now,
                ));
            });
            // A delayed text signal overrides a prior non-text hypothesis.
            restore("candidate_position");
            return;
        }
        // CSP evidence currently covers queued starts only. Sent starts fail open.
        if msg != 0x010d || !posted || gui.hwndFocus != hwnd {
            return;
        }
        let mut foreground_pid = 0;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut foreground_pid));
        if foreground_pid != GetCurrentProcessId() {
            restore("foreground_process_changed");
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
        let mut record = diagnostic_context();
        record["backend"] = serde_json::json!("decision");
        record["decision_source"] = serde_json::json!("composition_start");
        record["block"] = serde_json::json!(block);
        record["candidate"] = serde_json::json!(candidate);
        record["candidate_source"] = LOCAL
            .with(|s| s.borrow().candidate_source.clone())
            .unwrap_or(serde_json::Value::Null);
        record["candidate_source_is_recent"] = serde_json::json!(candidate);
        record["caret"] = serde_json::json!(o.caret);
        record["class"] = serde_json::json!(o.class);
        record["hwnd"] = serde_json::json!(format!("{:#x}", hwnd.0 as usize));
        super::push(record.to_string());
        if !block {
            restore("decision_allowed");
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
        let context =
            HIMC(imm_operation("ImmGetContext", hwnd, || ImmGetContext(hwnd).0 as usize) as *mut _);
        if context.is_invalid() {
            return;
        }
        // Cancellation may synchronously re-enter hooks; no LOCAL borrow is held.
        let cancelled = imm_operation("ImmNotifyIME_cancel", hwnd, || {
            ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0).as_bool()
        });
        let _ = imm_operation("ImmReleaseContext", hwnd, || {
            ImmReleaseContext(hwnd, context).as_bool()
        });
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
        let previous = HIMC(imm_operation("ImmAssociateContext_detach", hwnd, || {
            ImmAssociateContext(hwnd, HIMC::default()).0 as usize
        }) as *mut _);
        if previous.is_invalid() {
            return false;
        }
        LOCAL.with(|s| s.borrow_mut().detached = Some((hwnd.0 as usize, previous.0 as usize)));
        super::push(serde_json::json!({"backend":"detached","tid":GetCurrentThreadId(),"hwnd":format!("{:#x}",hwnd.0 as usize),"replay":false}).to_string());
        true
    }
}
