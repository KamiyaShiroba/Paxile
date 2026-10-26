//! Live observer. Three read-only sources, all delivered on this thread's
//! message loop:
//!  - out-of-context WinEvent hooks on the target pid(s) (window lifecycle,
//!    focus, menus, caret, IME events) plus a global hook for IME UI that
//!    lives in other processes;
//!  - a timer that polls GetGUIThreadInfo + IME open/conversion state of the
//!    foreground thread and logs only what changed;
//!  - a low-level keyboard hook, always passing keys on untouched: ScrollLock
//!    / Pause drop a MARK, and with --keys every key is logged while the
//!    target is in the foreground;
//!  - with --spy, the paxile-spy DLL's in-process message records.
//!
//! Lines typed into this console are also logged as MARKs.

use std::cell::RefCell;
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
use crate::win::{
    describe_cached, gui_tids, hx, pids_by_exe, processes, raw, short_class, thread_pid, WinDesc,
};

const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const WM_IME_CONTROL: u32 = 0x0283;
const IMC_GETCONVERSIONMODE: usize = 0x0001;
const IMC_GETOPENSTATUS: usize = 0x0005;

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
}

/// Runs `f` against the context unless it's already borrowed (re-entrant
/// delivery); in that case the event is dropped rather than panicking.
fn with_ctx(f: impl FnOnce(&mut Ctx)) {
    CTX.with(|c| {
        if let Ok(mut g) = c.try_borrow_mut() {
            if let Some(ctx) = g.as_mut() {
                f(ctx)
            }
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
    control: crate::control::Shared,
    managed: bool,
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
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if r.get("backend").is_some() {
            self.log.emit("backend", &format!("IME BACKEND {line}"), r);
            return;
        }
        if r.get("hello").is_some() {
            self.log.emit(
                "info",
                &format!("spy connected: pid {} tid {}", r["pid"], r["tid"]),
                r,
            );
            return;
        }
        if r.get("disconnected").is_some() || r.get("pipe_error").is_some() {
            self.log.emit("info", &format!("spy: {line}"), r);
            return;
        }
        let q = r["qpc"].as_i64().unwrap_or_else(crate::log::qpc);
        let name = r["msg"].as_str().unwrap_or("?");
        let mut human = format!(
            "SPY {} {:<23} {} {:?}",
            r["src"].as_str().unwrap_or(""),
            name,
            r["hwnd"].as_str().unwrap_or(""),
            short_class(r["class"].as_str().unwrap_or(""))
        );
        if let Some(s) = r["sub"].as_str() {
            human += &format!(" {s}");
        }
        if let Some(vk) = r["vk"].as_u64() {
            human += &format!(" {}", vk_name(vk as u32));
            if let Some(o) = r["vk_orig"].as_u64() {
                human += &format!(" (was {})", vk_name(o as u32));
            }
        }
        if let Some(c) = r["ch"].as_str() {
            human += &format!(" ch={c:?}");
        }
        if let Some(c) = r["comp"].as_str() {
            human += &format!(" comp={c:?}");
        }
        if let Some(c) = r["result"].as_str() {
            human += &format!(" result={c:?}");
        }
        if name == "WM_IME_SETCONTEXT" {
            human += &format!(" active={}", r["wp"]);
        }
        match r["himc"].as_bool() {
            Some(true) => {
                human += &format!(
                    "  [imc open={} conv={}]",
                    r["open"],
                    conv_str(r["conv"].as_u64().map(|c| c as usize))
                )
            }
            Some(false) => human += "  [no imc]",
            None => {}
        }
        self.log.emit_at(q, "spy", &human, r);
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
        // Every ~2s: flush, hook new UI threads, notice if the target went away.
        if c.managed && c.ticks % 20 == 0 {
            let policy = c.control.lock().unwrap().policy();
            c.pids = processes()
                .into_iter()
                .filter(|(_, exe)| {
                    policy.presets.iter().any(|p| {
                        (policy.profile == "auto" || policy.profile == p.name)
                            && p.executables.iter().any(|e| e.eq_ignore_ascii_case(exe))
                    })
                })
                .map(|(pid, _)| pid)
                .collect();
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

    let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
    unsafe {
        for &pid in &pids {
            for (lo, hi) in events::target_ranges(opts.all) {
                hooks.push(SetWinEventHook(
                    lo,
                    hi,
                    HMODULE::default(),
                    Some(on_target_event),
                    pid,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                ));
            }
        }
        hooks.push(SetWinEventHook(
            events::EVENT_OBJECT_IME_SHOW,
            events::EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED,
            HMODULE::default(),
            Some(on_global_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        ));
    }
    let failed = hooks.iter().filter(|h| h.0.is_null()).count();

    // Installed even without --keys: it's how ScrollLock/Pause marks arrive.
    let kb_hook = unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), HINSTANCE(hmod.0), 0).ok()
    };

    let mut log = log;
    log.emit(
        "info",
        &format!(
            "watching {} pid {:?}: {} winevent hooks ({} failed), keys={}, spy={}, poll={}ms, all={}",
            opts.exe, pids, hooks.len(), failed, opts.keys, opts.spy, opts.poll_ms, opts.all
        ),
        json!({ "exe": opts.exe, "pids": pids, "hooks": hooks.len(), "failed": failed,
                "keys": opts.keys, "spy": opts.spy, "poll_ms": opts.poll_ms, "all": opts.all }),
    );
    if let Some(p) = &log.path {
        log.note(&format!("jsonl -> {}", p.display()));
    }
    log.note("MARK: press ScrollLock / Pause, or type a note here + Enter");

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
            control,
            managed: opts.managed,
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
        for h in hooks {
            let _ = UnhookWinEvent(h);
        }
        if let Some(h) = kb_hook {
            let _ = UnhookWindowsHookEx(h);
        }
    }

    if let Some(mut c) = CTX.with(|c| c.borrow_mut().take()) {
        if let Some(s) = c.spy.as_mut() {
            s.unhook_all();
        }
        c.log.note("== event counts:");
        for (k, v) in &c.counts {
            c.log.note(&format!("   {v:>6} {k}"));
        }
        c.log.note("== window classes seen in events:");
        for (k, v) in &c.classes {
            c.log.note(&format!("   {v:>6} {k:?}"));
        }
        let rec = json!({ "counts": c.counts, "classes": c.classes });
        c.log.emit("summary", "done", rec);
        c.log.flush();
    }
}
