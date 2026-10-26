//! Window / process inspection helpers. Everything here is read-only and
//! avoids sending messages to the target (no WM_GETTEXT), so a hung target
//! can't stall us.

use std::collections::HashMap;

use serde_json::{json, Value};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetAncestor, GetClassNameW, GetDesktopWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, InternalGetWindowText, IsWindow,
    IsWindowVisible, GA_PARENT, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GW_OWNER,
};

/// CSP's window classes are all "<app GUID>[-<kind GUID>]"; keep only the
/// distinguishing part for human-readable output (JSON keeps the full name).
const CSP_CLASS: &str = "742DEA58-ED6B-4402-BC11-20DFC6D08040";

pub fn short_class(c: &str) -> String {
    match c.strip_prefix(CSP_CLASS) {
        Some("") => "CSP:main".into(),
        Some(rest) => format!(
            "CSP:{}",
            rest.trim_start_matches('-').get(..8).unwrap_or(rest)
        ),
        None => c.into(),
    }
}

/// Threads that own at least one top-level window of the given processes.
pub fn gui_tids(pids: &[u32]) -> Vec<u32> {
    let mut v: Vec<u32> = top_levels()
        .into_iter()
        .map(thread_pid)
        .filter(|(_, p)| pids.contains(p))
        .map(|(t, _)| t)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

pub fn hx(h: usize) -> String {
    format!("{:#x}", h)
}

pub fn raw(h: HWND) -> usize {
    h.0 as usize
}

fn wstr(buf: &[u16], n: i32) -> String {
    String::from_utf16_lossy(&buf[..n.clamp(0, buf.len() as i32) as usize])
}

pub fn thread_pid(h: HWND) -> (u32, u32) {
    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (tid, pid)
}

pub fn is_window(h: HWND) -> bool {
    !h.0.is_null() && unsafe { IsWindow(h) }.as_bool()
}

const STYLE_FLAGS: &[(u32, &str)] = &[
    (0x8000_0000, "POPUP"),
    (0x4000_0000, "CHILD"),
    (0x0800_0000, "DISABLED"),
    (0x00C0_0000, "CAPTION"),
    (0x0004_0000, "THICKFRAME"),
];

const EXSTYLE_FLAGS: &[(u32, &str)] = &[
    (0x0000_0008, "TOPMOST"),
    (0x0000_0080, "TOOLWINDOW"),
    (0x0004_0000, "APPWINDOW"),
    (0x0008_0000, "LAYERED"),
    (0x0000_0020, "TRANSPARENT"),
    (0x0800_0000, "NOACTIVATE"),
    (0x0020_0000, "NOREDIRBMP"),
    (0x0001_0000, "CONTROLPARENT"),
];

#[derive(Clone, Debug)]
pub struct WinDesc {
    pub hwnd: usize,
    pub class: String,
    pub title: String,
    pub pid: u32,
    pub tid: u32,
    pub visible: bool,
    pub style: u32,
    pub exstyle: u32,
    /// 0 for top-level windows (parent is the desktop).
    pub parent: usize,
    pub owner: usize,
    pub root: usize,
    pub rect: [i32; 4],
    pub alive: bool,
}

impl WinDesc {
    pub fn flags(&self) -> Vec<&'static str> {
        let mut v: Vec<&str> = STYLE_FLAGS
            .iter()
            .filter(|(b, _)| self.style & b == *b)
            .map(|(_, n)| *n)
            .collect();
        v.extend(
            EXSTYLE_FLAGS
                .iter()
                .filter(|(b, _)| self.exstyle & b == *b)
                .map(|(_, n)| *n),
        );
        v
    }

    pub fn short(&self) -> String {
        let mut s = format!("{} {:?}", hx(self.hwnd), short_class(&self.class));
        if !self.title.is_empty() {
            s += &format!(" {:?}", self.title);
        }
        if !self.alive {
            s += " (dead)";
        }
        s
    }

    pub fn line(&self) -> String {
        let [l, t, r, b] = self.rect;
        let mut s = format!(
            "{} {} {}x{}@({},{})",
            if self.visible { "VIS" } else { "---" },
            self.short(),
            r - l,
            b - t,
            l,
            t
        );
        let f = self.flags();
        if !f.is_empty() {
            s += " ";
            s += &f.join("|");
        }
        if self.owner != 0 {
            s += &format!(" owner={}", hx(self.owner));
        }
        s
    }

    pub fn to_json(&self) -> Value {
        json!({
            "hwnd": hx(self.hwnd),
            "class": self.class,
            "title": self.title,
            "pid": self.pid,
            "tid": self.tid,
            "visible": self.visible,
            "style": format!("{:#010x}", self.style),
            "exstyle": format!("{:#010x}", self.exstyle),
            "flags": self.flags(),
            "parent": hx(self.parent),
            "owner": hx(self.owner),
            "root": hx(self.root),
            "rect": self.rect,
            "alive": self.alive,
        })
    }
}

pub fn describe(h: HWND) -> WinDesc {
    unsafe {
        let mut cb = [0u16; 256];
        let n = GetClassNameW(h, &mut cb);
        let mut tb = [0u16; 512];
        let tn = InternalGetWindowText(h, &mut tb);
        let (tid, pid) = thread_pid(h);
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        let parent = GetAncestor(h, GA_PARENT);
        let parent = if parent == GetDesktopWindow() {
            0
        } else {
            raw(parent)
        };
        WinDesc {
            hwnd: raw(h),
            class: wstr(&cb, n),
            title: wstr(&tb, tn),
            pid,
            tid,
            visible: IsWindowVisible(h).as_bool(),
            style: GetWindowLongPtrW(h, GWL_STYLE) as u32,
            exstyle: GetWindowLongPtrW(h, GWL_EXSTYLE) as u32,
            parent,
            owner: GetWindow(h, GW_OWNER).map(raw).unwrap_or(0),
            root: raw(GetAncestor(h, GA_ROOT)),
            rect: [r.left, r.top, r.right, r.bottom],
            alive: true,
        }
    }
}

/// Describes `h`, falling back to the cached description if it is already gone.
pub fn describe_cached(cache: &mut HashMap<usize, WinDesc>, h: HWND) -> Option<WinDesc> {
    if h.0.is_null() {
        return None;
    }
    if is_window(h) {
        let d = describe(h);
        cache.insert(d.hwnd, d.clone());
        Some(d)
    } else {
        cache.get(&raw(h)).map(|d| WinDesc {
            alive: false,
            ..d.clone()
        })
    }
}

unsafe extern "system" fn collect_cb(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<HWND>);
    v.push(h);
    TRUE
}

pub fn top_levels() -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

pub fn descendants(h: HWND) -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(h, Some(collect_cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

pub fn processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let n = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                out.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

pub fn pids_by_exe(exe: &str) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, n)| n.eq_ignore_ascii_case(exe))
        .map(|(p, _)| p)
        .collect()
}
