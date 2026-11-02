//! Live observer. Three read-only sources, all delivered on this thread's
//! message loop:
//!  - out-of-context WinEvent hooks on the target pid(s) (window lifecycle,
//!    focus, menus, caret, IME events) plus a global hook for IME UI that
//!    lives in other processes;
//!  - a timer that polls GetGUIThreadInfo + IME open/conversion state of the
//!    foreground thread and logs only what changed;
//!  - with --keys, a low-level keyboard hook passing keys on untouched: ScrollLock
//!    / Pause drop a MARK, and with --keys every key is logged while the
//!    target is in the foreground;
//!  - with --spy, the paxile-spy DLL's in-process message records.
//!
//! Lines typed into this console are also logged as MARKs.

#[path = "accessibility.rs"]
mod accessibility;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};

use serde_json::{json, Value};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetGUIThreadInfo, GetMessageW,
    KillTimer, PostThreadMessageW, SendMessageTimeoutW, SetTimer, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, GUITHREADINFO, HHOOK, KBDLLHOOKSTRUCT, MSG,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, WH_KEYBOARD_LL, WM_QUIT, WM_TIMER,
};

use crate::events::{self, event_name, objid_name, EVENT_OBJECT_DESTROY, OBJID_WINDOW};
use crate::log::Logger;
use crate::spy::Spy;
use crate::win::{describe_cached, gui_tids, hx, pids_by_exe, processes, raw, thread_pid, WinDesc};

const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const WM_IME_CONTROL: u32 = 0x0283;
const IMC_GETCONVERSIONMODE: usize = 0x0001;
const IMC_GETOPENSTATUS: usize = 0x0005;

#[derive(Default)]
struct TargetHooks<H> {
    entries: BTreeMap<(u32, usize), H>,
}

impl<H> TargetHooks<H> {
    fn sync<R: Copy>(
        &mut self,
        pids: &[u32],
        ranges: &[R],
        mut install: impl FnMut(u32, R) -> Option<H>,
        mut remove: impl FnMut(H),
    ) {
        let stale: Vec<_> = self
            .entries
            .keys()
            .copied()
            .filter(|(pid, _)| !pids.contains(pid))
            .collect();
        for key in stale {
            if let Some(hook) = self.entries.remove(&key) {
                remove(hook);
            }
        }
        for &pid in pids {
            for (index, &range) in ranges.iter().enumerate() {
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    self.entries.entry((pid, index))
                {
                    if let Some(hook) = install(pid, range) {
                        entry.insert(hook);
                    }
                }
            }
        }
    }
}

fn sync_target_hooks(hooks: &mut TargetHooks<HWINEVENTHOOK>, pids: &[u32], all: bool) {
    hooks.sync(
        pids,
        &events::target_ranges(all),
        |pid, (lo, hi)| {
            let hook = unsafe {
                SetWinEventHook(
                    lo,
                    hi,
                    HMODULE::default(),
                    Some(on_target_event),
                    pid,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if hook.0.is_null() {
                eprintln!("paxile-probe: WinEvent hook failed pid={pid} range={lo:#x}..{hi:#x}");
                None
            } else {
                Some(hook)
            }
        },
        |hook| {
            if !unsafe { UnhookWinEvent(hook) }.as_bool() {
                eprintln!("paxile-probe: UnhookWinEvent failed");
            }
        },
    );
}

/// Sanitize before constructing either sink, including records from old DLLs.
fn redact_input(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let char_message = map.get("msg").and_then(Value::as_str).is_some_and(|s| {
                matches!(
                    s,
                    "WM_CHAR"
                        | "WM_SYSCHAR"
                        | "WM_DEADCHAR"
                        | "WM_SYSDEADCHAR"
                        | "WM_UNICHAR"
                        | "WM_IME_CHAR"
                        | "WM_IME_COMPOSITION"
                )
            });
            map.retain(|key, _| {
                !matches!(
                    key.to_ascii_lowercase().as_str(),
                    "ch" | "char"
                        | "text"
                        | "comp"
                        | "result"
                        | "title"
                        | "composition"
                        | "window_title"
                ) && !(char_message
                    && matches!(
                        key.as_str(),
                        "wp" | "wparam" | "lp" | "lparam" | "vk" | "vk_orig"
                    ))
            });
            for v in map.values_mut() {
                redact_input(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                redact_input(v);
            }
        }
        _ => {}
    }
}

fn log_spy_line(log: &mut Logger, line: &str) -> bool {
    let Ok(mut record) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if !record.is_object() {
        return false;
    }
    redact_input(&mut record);
    let received = crate::log::qpc();
    let producer = record.get("qpc").and_then(Value::as_i64).filter(|q| *q > 0);
    if producer.is_none() {
        record.as_object_mut().unwrap().remove("qpc");
    }
    record["received_qpc"] = json!(received);
    record["time_basis"] = json!(if producer.is_some() {
        "producer_qpc"
    } else {
        "received_qpc"
    });
    let kind = if record.get("backend").is_some() {
        "backend"
    } else if record.get("hello").is_some()
        || record.get("disconnected").is_some()
        || record.get("pipe_error").is_some()
    {
        "info"
    } else {
        "spy"
    };
    let human = format!("SPY {record}");
    log.emit_at(producer.unwrap_or(received), kind, &human, record);
    true
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn diagnostic_stdout_is_redacted() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "watch::diagnostic_tests::backend_and_hello_use_production_time_and_redact_legacy_payloads", "--nocapture"])
            .output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("PRIVATE"));
        assert!(stdout.contains("producer_qpc"));
        assert!(stdout.contains("received_qpc"));
    }

    #[test]
    fn reentrant_delivery_is_counted_without_panicking() {
        REENTRANT_DROPS.with(|n| n.set(0));
        CTX.with(|c| {
            let _guard = c.borrow_mut();
            with_ctx(|_| panic!("reentrant callback must be dropped"));
        });
        assert_eq!(REENTRANT_DROPS.with(Cell::get), 1);
    }

    #[test]
    fn managed_hooks_add_retry_remove_and_release() {
        let mut hooks = TargetHooks::<u32>::default();
        let mut removed = Vec::new();
        hooks.sync(
            &[10],
            &[1, 2],
            |pid, range| (range == 1).then_some(pid + range),
            |h| removed.push(h),
        );
        assert_eq!(hooks.entries.len(), 1);
        hooks.sync(
            &[10, 20],
            &[1, 2],
            |pid, range| Some(pid + range),
            |h| removed.push(h),
        );
        assert_eq!(hooks.entries.len(), 4);
        hooks.sync(
            &[20],
            &[1, 2],
            |_, _| panic!("duplicate install"),
            |h| removed.push(h),
        );
        assert_eq!(hooks.entries.len(), 2);
        removed.sort();
        assert_eq!(removed, vec![11, 12]);
        hooks.sync(
            &[],
            &[1, 2],
            |_, _| panic!("shutdown install"),
            |h| removed.push(h),
        );
        assert!(hooks.entries.is_empty());
        removed.sort();
        assert_eq!(removed, vec![11, 12, 21, 22]);
    }

    #[test]
    fn backend_and_hello_use_production_time_and_redact_legacy_payloads() {
        let mut log = Logger::new("probe-timeline-test", true).unwrap();
        for discriminator in ["backend", "hello"] {
            let line = json!({discriminator:true,"qpc":123,"ch":"PRIVATE",
                "comp":"PRIVATE","result":"PRIVATE","title":"PRIVATE",
                "msg":"WM_CHAR","wp":31169,"lp":99,"nested":{"title":"PRIVATE"}})
            .to_string();
            assert!(log_spy_line(&mut log, &line));
        }
        assert!(log_spy_line(&mut log, r#"{"backend":"legacy"}"#));
        assert!(!log_spy_line(&mut log, "not json PRIVATE"));
        let path = log.path.clone().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("PRIVATE"));
        let records: Vec<Value> = text
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        for r in &records[..2] {
            assert_eq!(r["qpc"], 123);
            assert_eq!(r["time_basis"], "producer_qpc");
            assert!(r["received_qpc"].as_i64().unwrap() > 123);
            assert!(r.get("wp").is_none());
            assert!(r.get("lp").is_none());
        }
        assert_eq!(records[2]["time_basis"], "received_qpc");
        assert!(records[2].get("qpc").is_none());
        drop(log);
        std::fs::remove_file(path).unwrap();
    }
}

pub struct Opts {
    pub exe: String,
    pub managed: bool,
    pub authorized: bool,
    pub wait: bool,
    pub all: bool,
    pub keys: bool,
    pub spy: bool,
    pub poll_ms: u32,
    pub policy: paxile_policy::Config,
}

const VK_PAUSE: u32 = 0x13;
const VK_SCROLL: u32 = 0x91;

static MAIN_TID: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
    static REENTRANT_DROPS: Cell<u64> = const { Cell::new(0) };
}

/// Runs `f` against the context unless it's already borrowed (re-entrant
/// delivery); in that case the event is dropped rather than panicking.
fn with_ctx(f: impl FnOnce(&mut Ctx)) {
    CTX.with(|c| {
        if let Ok(mut g) = c.try_borrow_mut() {
            if let Some(ctx) = g.as_mut() {
                f(ctx)
            }
        } else {
            REENTRANT_DROPS.with(|n| n.set(n.get().saturating_add(1)));
        }
    })
}

#[derive(Clone, PartialEq, Default)]
struct GuiState {
    target: bool,
    fg: usize,
    fg_pid: u32,
    active: usize,
    focus: usize,
    capture: usize,
    menu_owner: usize,
    move_size: usize,
    caret: usize,
    caret_rect: [i32; 4],
    flags: u32,
    ime_wnd: usize,
    ime_open: Option<usize>,
    conv: Option<usize>,
    hkl: usize,
}

const GUI_FLAGS: &[(u32, &str)] = &[
    (0x01, "CARETBLINKING"),
    (0x02, "INMOVESIZE"),
    (0x04, "INMENUMODE"),
    (0x08, "SYSTEMMENUMODE"),
    (0x10, "POPUPMENUMODE"),
];

fn gui_flags(f: u32) -> String {
    let v: Vec<&str> = GUI_FLAGS
        .iter()
        .filter(|(b, _)| f & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if v.is_empty() {
        "0".into()
    } else {
        v.join("|")
    }
}

fn conv_str(c: Option<usize>) -> String {
    match c {
        None => "n/a".into(),
        Some(c) => format!(
            "{:#x}{}",
            c,
            if c & 1 != 0 { "(NATIVE)" } else { "(ALPHA)" }
        ),
    }
}

struct Ctx {
    log: Logger,
    cache: HashMap<usize, WinDesc>,
    pids: Vec<u32>,
    exe_names: HashMap<u32, String>,
    all: bool,
    last: GuiState,
    ticks: u64,
    counts: BTreeMap<&'static str, u64>,
    classes: BTreeMap<String, u64>,
    keys: bool,
    marks: u32,
    mark_rx: Receiver<String>,
    spy: Option<Spy>,
    accessibility: Option<accessibility::Probe>,
    control: crate::control::Shared,
    managed: bool,
    target_hooks: TargetHooks<HWINEVENTHOOK>,
}

impl Ctx {
    fn desc(&mut self, h: usize) -> Option<WinDesc> {
        describe_cached(&mut self.cache, HWND(h as *mut _))
    }

    fn desc_json(&mut self, h: usize) -> Value {
        self.desc(h).map(|d| d.to_json()).unwrap_or(Value::Null)
    }

    fn short(&mut self, h: usize) -> String {
        if h == 0 {
            return "0".into();
        }
        self.desc(h).map(|d| d.short()).unwrap_or_else(|| hx(h))
    }

    fn exe_of(&mut self, pid: u32) -> String {
        if !self.exe_names.contains_key(&pid) {
            self.exe_names.extend(processes());
        }
        self.exe_names
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| "?".into())
    }

    fn on_event(&mut self, ev: u32, hwnd: HWND, idobj: i32, idchild: i32, tid: u32, global: bool) {
        if !events::wanted(ev, idobj, idchild, self.all) {
            return;
        }
        let d = self.desc(raw(hwnd));
        let pid = d.as_ref().map(|d| d.pid).unwrap_or(0);
        if global && (self.pids.contains(&pid) || !events::is_ime_event(ev)) {
            return; // already seen by the per-pid hook
        }
        let name = event_name(ev);
        *self.counts.entry(name).or_default() += 1;
        if let Some(d) = &d {
            *self.classes.entry(d.class.clone()).or_default() += 1;
        }

        let mut human = format!("{name:<16}");
        if idobj != OBJID_WINDOW {
            human += &format!(" [{}]", objid_name(idobj));
        }
        if idchild != 0 {
            human += &format!(" #{idchild}");
        }
        match &d {
            Some(d) if ev == events::EVENT_OBJECT_CREATE || ev == events::EVENT_OBJECT_SHOW => {
                human += &format!(" {}", d.line())
            }
            Some(d) => human += &format!(" {}", d.short()),
            None => human += &format!(" {}", hx(raw(hwnd))),
        }
        if global {
            human += &format!("  <pid {pid} {}>", self.exe_of(pid));
        }
        self.log.emit(
            "event",
            &human,
            json!({
                "event": name,
                "code": format!("{ev:#06x}"),
                "obj": objid_name(idobj),
                "child": idchild,
                "tid": tid,
                "global": global,
                "win": d.as_ref().map(|d| d.to_json()),
            }),
        );
        if ev == EVENT_OBJECT_DESTROY && idobj == OBJID_WINDOW {
            self.cache.remove(&raw(hwnd));
        }
    }

    fn on_state(&mut self, mut s: GuiState) {
        // A timed-out IME query (target busy) means "unknown", not "changed".
        if s.target && s.ime_wnd == self.last.ime_wnd {
            s.ime_open = s.ime_open.or(self.last.ime_open);
            s.conv = s.conv.or(self.last.conv);
        }
        if s == self.last {
            return;
        }
        let old = std::mem::replace(&mut self.last, s.clone());
        let mut parts = Vec::new();
        if s.target != old.target || s.fg != old.fg {
            parts.push(if s.target {
                format!("fg -> {}", self.short(s.fg))
            } else {
                format!(
                    "fg -> other app (pid {} {})",
                    s.fg_pid,
                    self.exe_of(s.fg_pid)
                )
            });
        }
        if s.target {
            for (label, a, b) in [
                ("active", old.active, s.active),
                ("focus", old.focus, s.focus),
                ("capture", old.capture, s.capture),
                ("menu", old.menu_owner, s.menu_owner),
                ("movesize", old.move_size, s.move_size),
                ("caret", old.caret, s.caret),
                ("imewnd", old.ime_wnd, s.ime_wnd),
            ] {
                if a != b {
                    parts.push(format!("{label}: {} -> {}", self.short(a), self.short(b)));
                }
            }
            if s.caret_rect != old.caret_rect && s.caret != 0 {
                parts.push(format!("caret_rect {:?}", s.caret_rect));
            }
            if s.flags != old.flags {
                parts.push(format!(
                    "flags {} -> {}",
                    gui_flags(old.flags),
                    gui_flags(s.flags)
                ));
            }
            if s.ime_open != old.ime_open {
                parts.push(format!("ime_open {:?} -> {:?}", old.ime_open, s.ime_open));
            }
            if s.conv != old.conv {
                parts.push(format!(
                    "conv {} -> {}",
                    conv_str(old.conv),
                    conv_str(s.conv)
                ));
            }
            if s.hkl != old.hkl {
                parts.push(format!("hkl {:#x} -> {:#x}", old.hkl, s.hkl));
            }
        }
        if parts.is_empty() {
            return;
        }
        let rec = json!({
            "target": s.target,
            "fg_pid": s.fg_pid,
            "fg": self.desc_json(s.fg),
            "active": self.desc_json(s.active),
            "focus": self.desc_json(s.focus),
            "capture": self.desc_json(s.capture),
            "menu_owner": self.desc_json(s.menu_owner),
            "move_size": self.desc_json(s.move_size),
            "caret": self.desc_json(s.caret),
            "caret_rect": s.caret_rect,
            "flags": gui_flags(s.flags),
            "ime_wnd": hx(s.ime_wnd),
            "ime_open": s.ime_open,
            "conv": s.conv,
            "hkl": hx(s.hkl),
        });
        self.log
            .emit("state", &format!("STATE  {}", parts.join(", ")), rec);
    }

    fn hook_new_threads(&mut self) {
        let Some(spy) = self.spy.as_mut() else { return };
        let res = spy.hook_threads(&gui_tids(&self.pids));
        for (tid, err) in res {
            match err {
                None => self.log.emit(
                    "info",
                    &format!("spy hooked tid {tid}"),
                    json!({ "spy_tid": tid }),
                ),
                Some(e) => self.log.emit(
                    "info",
                    &format!("spy hook FAILED tid {tid}: {e}"),
                    json!({ "spy_tid": tid, "error": e }),
                ),
            }
        }
    }

    fn mark(&mut self, text: &str) {
        self.marks += 1;
        let n = self.marks;
        self.log.emit(
            "mark",
            &format!("==================== MARK {n} {text}"),
            json!({ "n": n, "text": text }),
        );
    }

    fn on_spy_line(&mut self, line: &str) {
        if !log_spy_line(&mut self.log, line) {
            *self.counts.entry("spy_parse_failed").or_default() += 1;
        }
    }

    fn on_key(&mut self, msg: u32, k: &KBDLLHOOKSTRUCT) {
        let up = k.flags.0 & 0x80 != 0;
        if !up && (k.vkCode == VK_SCROLL || k.vkCode == VK_PAUSE) {
            self.mark(if k.vkCode == VK_SCROLL {
                "(ScrollLock)"
            } else {
                "(Pause)"
            });
            return;
        }
        if !self.keys || !self.last.target {
            return;
        }
        let injected = k.flags.0 & 0x10 != 0;
        let sys = msg == 0x0104 || msg == 0x0105;
        let focus = self.last.focus;
        let fdesc = self.short(focus);
        let mut human = format!(
            "KEY {:<4} {} sc={:#04x}",
            if up { "up" } else { "down" },
            vk_name(k.vkCode),
            k.scanCode
        );
        if sys {
            human += " [sys]";
        }
        if injected {
            human += " [injected]";
        }
        human += &format!("  ime={} focus={}", conv_str(self.last.conv), fdesc);
        self.log.emit(
            "key",
            &human,
            json!({
                "up": up,
                "vk": k.vkCode,
                "vk_name": vk_name(k.vkCode),
                "scan": k.scanCode,
                "flags": k.flags.0,
                "injected": injected,
                "focus": hx(focus),
                "ime_open": self.last.ime_open,
                "conv": self.last.conv,
            }),
        );
    }
}

fn vk_name(vk: u32) -> String {
    let n = match vk {
        0x08 => "BACK",
        0x09 => "TAB",
        0x0D => "RETURN",
        0x10 => "SHIFT",
        0x11 => "CONTROL",
        0x12 => "MENU",
        0x14 => "CAPITAL",
        0x15 => "KANA/HANGUL",
        0x19 => "KANJI",
        0x1B => "ESCAPE",
        0x1C => "CONVERT",
        0x1D => "NONCONVERT",
        0x20 => "SPACE",
        0x25 => "LEFT",
        0x26 => "UP",
        0x27 => "RIGHT",
        0x28 => "DOWN",
        0x2E => "DELETE",
        0x5B => "LWIN",
        0x5C => "RWIN",
        0xA0 => "LSHIFT",
        0xA1 => "RSHIFT",
        0xA2 => "LCONTROL",
        0xA3 => "RCONTROL",
        0xA4 => "LMENU",
        0xA5 => "RMENU",
        0xE5 => "PROCESSKEY",
        0x30..=0x39 | 0x41..=0x5A => return char::from_u32(vk).unwrap().to_string(),
        0x70..=0x87 => return format!("F{}", vk - 0x6F),
        _ => return format!("{vk:#04x}"),
    };
    n.into()
}

unsafe extern "system" fn on_target_event(
    _h: HWINEVENTHOOK,
    ev: u32,
    hwnd: HWND,
    idobj: i32,
    idchild: i32,
    tid: u32,
    _time: u32,
) {
    with_ctx(|c| c.on_event(ev, hwnd, idobj, idchild, tid, false));
}

unsafe extern "system" fn on_global_event(
    _h: HWINEVENTHOOK,
    ev: u32,
    hwnd: HWND,
    idobj: i32,
    idchild: i32,
    tid: u32,
    _time: u32,
) {
    with_ctx(|c| c.on_event(ev, hwnd, idobj, idchild, tid, true));
}

unsafe extern "system" fn on_key(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        with_ctx(|c| c.on_key(wp.0 as u32, k));
    }
    CallNextHookEx(HHOOK::default(), code, wp, lp)
}

unsafe extern "system" fn on_ctrl(_t: u32) -> BOOL {
    let _ = PostThreadMessageW(
        MAIN_TID.load(Ordering::SeqCst),
        WM_QUIT,
        WPARAM(0),
        LPARAM(0),
    );
    TRUE
}

/// IME query via the target's default IME window. SMTO_BLOCK keeps us from
/// pumping (and re-entering our own callbacks) while we wait.
fn ime_query(ime_wnd: HWND, cmd: usize) -> Option<usize> {
    if ime_wnd.0.is_null() {
        return None;
    }
    let mut res = 0usize;
    let ok = unsafe {
        SendMessageTimeoutW(
            ime_wnd,
            WM_IME_CONTROL,
            WPARAM(cmd),
            LPARAM(0),
            SMTO_BLOCK | SMTO_ABORTIFHUNG,
            30,
            Some(&mut res),
        )
    };
    (ok.0 != 0).then_some(res)
}

/// Gathers the foreground GUI state. Runs with no borrow of CTX held.
fn sample(pids: &[u32]) -> GuiState {
    let fg = unsafe { GetForegroundWindow() };
    let (tid, pid) = thread_pid(fg);
    let mut s = GuiState {
        fg: raw(fg),
        fg_pid: pid,
        ..Default::default()
    };
    if !pids.contains(&pid) {
        return s;
    }
    s.target = true;
    let mut g = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(tid, &mut g) }.is_ok() {
        s.active = raw(g.hwndActive);
        s.focus = raw(g.hwndFocus);
        s.capture = raw(g.hwndCapture);
        s.menu_owner = raw(g.hwndMenuOwner);
        s.move_size = raw(g.hwndMoveSize);
        s.caret = raw(g.hwndCaret);
        let r = g.rcCaret;
        s.caret_rect = [r.left, r.top, r.right, r.bottom];
        s.flags = g.flags.0;
    }
    let anchor = [s.focus, s.active, s.fg]
        .into_iter()
        .find(|&h| h != 0)
        .unwrap_or(0);
    let ime = unsafe { ImmGetDefaultIMEWnd(HWND(anchor as *mut _)) };
    s.ime_wnd = raw(ime);
    s.ime_open = ime_query(ime, IMC_GETOPENSTATUS);
    s.conv = ime_query(ime, IMC_GETCONVERSIONMODE);
    s.hkl = unsafe { GetKeyboardLayout(tid) }.0 as usize;
    s
}

fn tick() {
    let mut pids = Vec::new();
    with_ctx(|c| {
        c.ticks += 1;
        pids = c.pids.clone();
    });

    let s = sample(&pids);
    let mut quit = false;
    with_ctx(|c| {
        c.on_state(s);
        let marks: Vec<String> = c.mark_rx.try_iter().collect();
        for m in marks {
            if m == "/block" || m.starts_with("/block ") {
                let response =
                    crate::control::execute(&c.control, m.strip_prefix("/block ").unwrap_or(""));
                c.log
                    .emit("backend", &format!("CONTROL {response}"), response);
            } else {
                c.mark(&m);
            }
        }
        let lines = c.spy.as_ref().map(|s| s.drain()).unwrap_or_default();
        for l in lines {
            c.on_spy_line(&l);
        }
        if let Some(probe) = &c.accessibility {
            for mut record in probe.drain() {
                let qpc = record["qpc"].as_i64().unwrap_or_else(crate::log::qpc);
                record["received_qpc"] = json!(crate::log::qpc());
                record["time_basis"] = json!("producer_qpc");
                record["producer_drops"] = json!(probe.dropped());
                c.log
                    .emit_at(qpc, "accessibility", &format!("UIA {record}"), record);
            }
        }
        // Every ~2s: flush, hook new UI threads, notice if the target went away.
        if c.managed && c.ticks % 20 == 0 {
            let policy = c.control.lock().unwrap().policy();
            c.pids = processes()
                .into_iter()
                .filter(|(_, exe)| {
                    policy.presets.iter().any(|p| {
                        (policy.profile == "auto" || policy.profile == p.name) && p.matches_exe(exe)
                    })
                })
                .map(|(pid, _)| pid)
                .collect();
            sync_target_hooks(&mut c.target_hooks, &c.pids, c.all);
            if let Some(probe) = &mut c.accessibility {
                probe.set_targets(&c.pids);
            }
            c.hook_new_threads();
        }
        if c.ticks % 40 == 0 {
            c.log.flush();
            c.hook_new_threads();
            let alive: Vec<u32> = processes().into_iter().map(|(p, _)| p).collect();
            if !c.managed && !c.pids.iter().any(|p| alive.contains(p)) {
                c.log.emit("info", "target process exited", json!({}));
                quit = true;
            }
        }
    });
    if quit {
        let _ = unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

pub fn run(opts: Opts, pids: Vec<u32>, log: Logger) {
    unsafe {
        MAIN_TID.store(GetCurrentThreadId(), Ordering::SeqCst);
        let _ = SetConsoleCtrlHandler(Some(on_ctrl), true);
    }

    let pids = if pids.is_empty() && opts.wait && !opts.managed {
        log.note(&format!("waiting for {} ... (Ctrl+C to stop)", opts.exe));
        loop {
            let p = pids_by_exe(&opts.exe);
            if !p.is_empty() {
                break p;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    } else {
        pids
    };

    let mut hooks = TargetHooks::default();
    sync_target_hooks(&mut hooks, &pids, opts.all);
    let global_hook = unsafe {
        SetWinEventHook(
            events::EVENT_OBJECT_IME_SHOW,
            events::EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED,
            HMODULE::default(),
            Some(on_global_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    let failed = events::target_ranges(opts.all).len() * pids.len() - hooks.entries.len()
        + usize::from(global_hook.0.is_null());
    // Low-level key sampling is explicit opt-in only.
    let kb_hook = if opts.keys {
        unsafe {
            let hmod = GetModuleHandleW(None).unwrap_or_default();
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), HINSTANCE(hmod.0), 0).ok()
        }
    } else {
        None
    };

    let mut log = log;
    log.emit(
        "info",
        &format!(
            "watching {} pid {:?}: {} winevent hooks ({} failed), keys={}, spy={}, poll={}ms, all={}",
            opts.exe, pids, hooks.entries.len() + 1, failed, opts.keys, opts.spy, opts.poll_ms, opts.all
        ),
        json!({ "exe": opts.exe, "pids": pids, "hooks": hooks.entries.len() + 1, "failed": failed,
                "keys": opts.keys, "spy": opts.spy, "poll_ms": opts.poll_ms, "all": opts.all }),
    );
    if let Some(p) = &log.path {
        log.note(&format!("jsonl -> {}", p.display()));
    }
    log.note("MARK: type a note + Enter; --keys also enables ScrollLock/Pause marks and records virtual keys/scan codes");

    let control = std::sync::Arc::new(std::sync::Mutex::new(paxile_policy::BackendControl::new(
        opts.authorized,
    )));
    {
        let mut guard = control.lock().unwrap();
        guard.initialize(opts.policy.clone());
        if !opts.policy.enabled {
            let _ = guard.execute(paxile_policy::ControlCommand::Off);
        }
    }
    let _control_server = match crate::control::Server::start(control.clone()) {
        Ok(server) => Some(server),
        Err(e) => {
            log.note(&format!("control unavailable: {e}"));
            None
        }
    };
    log.note(&format!(
        "runtime control: /block on|off|toggle|status; external --session {}",
        std::process::id()
    ));
    let spy = if opts.spy || opts.policy.enabled {
        log.note(&format!(
            "IME backend: enabled={} profile={} (experimental; first-key replay disabled)",
            opts.policy.enabled, opts.policy.profile
        ));
        match Spy::start(pids.clone(), opts.policy.clone(), control.clone()) {
            Ok(s) => {
                log.emit(
                    "info",
                    &format!("spy dll {}", s.dll),
                    json!({ "spy_dll": s.dll }),
                );
                Some(s)
            }
            Err(e) => {
                log.emit(
                    "info",
                    &format!("spy disabled: {e}"),
                    json!({ "spy_error": e }),
                );
                None
            }
        }
    } else {
        None
    };

    let accessibility = match accessibility::Probe::start(pids.clone()) {
        Ok(probe) => probe,
        Err(_) => {
            log.emit(
                "info",
                "UIA diagnostic worker unavailable",
                json!({"uia":"worker_start_failed"}),
            );
            None
        }
    };
    if accessibility.is_some() {
        log.emit(
            "info",
            "UIA structural diagnostics explicitly enabled (provider calls have no hard deadline)",
            json!({"uia":"enabled", "poll_ms":200}),
        );
    }
    let (mark_tx, mark_rx) = channel();
    let managed = opts.managed;
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if mark_tx.send(line.trim().to_string()).is_err() {
                break;
            }
        }
        if managed {
            let _ = unsafe {
                PostThreadMessageW(
                    MAIN_TID.load(Ordering::SeqCst),
                    WM_QUIT,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    });

    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            log,
            cache: HashMap::new(),
            pids,
            exe_names: HashMap::new(),
            all: opts.all,
            last: GuiState::default(),
            ticks: 0,
            counts: BTreeMap::new(),
            classes: BTreeMap::new(),
            keys: opts.keys,
            marks: 0,
            mark_rx,
            spy,
            accessibility,
            control,
            managed: opts.managed,
            target_hooks: hooks,
        })
    });
    with_ctx(|c| c.hook_new_threads());

    unsafe {
        let timer = SetTimer(HWND::default(), 1, opts.poll_ms, None);
        tick();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if msg.message == WM_TIMER && msg.hwnd.0.is_null() {
                tick();
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = KillTimer(HWND::default(), timer);
        let _ = UnhookWinEvent(global_hook);
        if let Some(h) = kb_hook {
            let _ = UnhookWindowsHookEx(h);
        }
    }

    if let Some(mut c) = CTX.with(|c| c.borrow_mut().take()) {
        sync_target_hooks(&mut c.target_hooks, &[], c.all);
        if let Some(s) = c.spy.as_mut() {
            s.unhook_all();
        }
        let uia_drops = c.accessibility.as_ref().map(|p| p.dropped()).unwrap_or(0);
        drop(c.accessibility.take());
        c.log.emit(
            "info",
            "UIA diagnostic producer loss",
            json!({"uia":"summary", "producer_drops":uia_drops}),
        );
        c.log.note("== event counts:");
        for (k, v) in &c.counts {
            c.log.note(&format!("   {v:>6} {k}"));
        }
        c.log.note("== window classes seen in events:");
        for (k, v) in &c.classes {
            c.log.note(&format!("   {v:>6} {k:?}"));
        }
        let rec = json!({ "counts": c.counts, "classes": c.classes, "reentrant_drops": REENTRANT_DROPS.with(Cell::get) });
        c.log.emit("summary", "done", rec);
        c.log.flush();
    }
}
