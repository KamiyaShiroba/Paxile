//! WinEvent constants, names, and the filter that decides what is worth logging.

pub const OBJID_WINDOW: i32 = 0;
pub const OBJID_CARET: i32 = -8;

pub const EVENT_OBJECT_CREATE: u32 = 0x8000;
pub const EVENT_OBJECT_DESTROY: u32 = 0x8001;
pub const EVENT_OBJECT_SHOW: u32 = 0x8002;
pub const EVENT_OBJECT_HIDE: u32 = 0x8003;
pub const EVENT_OBJECT_REORDER: u32 = 0x8004;
pub const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
pub const EVENT_OBJECT_IME_SHOW: u32 = 0x8027;
pub const EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED: u32 = 0x8030;

pub fn event_name(ev: u32) -> &'static str {
    match ev {
        0x0001 => "SOUND",
        0x0002 => "ALERT",
        0x0003 => "FOREGROUND",
        0x0004 => "MENUSTART",
        0x0005 => "MENUEND",
        0x0006 => "MENUPOPUPSTART",
        0x0007 => "MENUPOPUPEND",
        0x0008 => "CAPTURESTART",
        0x0009 => "CAPTUREEND",
        0x000A => "MOVESIZESTART",
        0x000B => "MOVESIZEEND",
        0x000C => "CONTEXTHELPSTART",
        0x000D => "CONTEXTHELPEND",
        0x000E => "DRAGDROPSTART",
        0x000F => "DRAGDROPEND",
        0x0010 => "DIALOGSTART",
        0x0011 => "DIALOGEND",
        0x0012 => "SCROLLINGSTART",
        0x0013 => "SCROLLINGEND",
        0x0014 => "SWITCHSTART",
        0x0015 => "SWITCHEND",
        0x0016 => "MINIMIZESTART",
        0x0017 => "MINIMIZEEND",
        0x8000 => "CREATE",
        0x8001 => "DESTROY",
        0x8002 => "SHOW",
        0x8003 => "HIDE",
        0x8004 => "REORDER",
        0x8005 => "FOCUS",
        0x8006 => "SELECTION",
        0x8007 => "SELECTIONADD",
        0x8008 => "SELECTIONREMOVE",
        0x8009 => "SELECTIONWITHIN",
        0x800A => "STATECHANGE",
        0x800B => "LOCATIONCHANGE",
        0x800C => "NAMECHANGE",
        0x800D => "DESCRIPTIONCHANGE",
        0x800E => "VALUECHANGE",
        0x800F => "PARENTCHANGE",
        0x8014 => "TEXTSELCHANGED",
        0x8017 => "CLOAKED",
        0x8018 => "UNCLOAKED",
        0x8027 => "IME_SHOW",
        0x8028 => "IME_HIDE",
        0x8029 => "IME_CHANGE",
        0x8030 => "CONVTARGETCHANGED",
        _ => "?",
    }
}

pub fn objid_name(id: i32) -> String {
    match id {
        0 => "WINDOW".into(),
        -1 => "SYSMENU".into(),
        -2 => "TITLEBAR".into(),
        -3 => "MENU".into(),
        -4 => "CLIENT".into(),
        -5 => "VSCROLL".into(),
        -6 => "HSCROLL".into(),
        -7 => "SIZEGRIP".into(),
        -8 => "CARET".into(),
        -9 => "CURSOR".into(),
        -10 => "ALERT".into(),
        -11 => "SOUND".into(),
        -16 => "NATIVEOM".into(),
        n => n.to_string(),
    }
}

/// Event ranges to hook for the target process. Kept narrow so the
/// out-of-context hook doesn't marshal cursor-move noise across processes.
pub fn target_ranges(all: bool) -> Vec<(u32, u32)> {
    let mut v = vec![
        (0x0003, 0x0011), // FOREGROUND .. DIALOGEND
        (0x0016, 0x0017), // MINIMIZE*
        (0x8000, 0x8005), // CREATE .. FOCUS
        (0x800B, 0x800B), // LOCATIONCHANGE (caret only, filtered below)
        (0x800F, 0x800F), // PARENTCHANGE
        (0x8014, 0x8014), // TEXTSELCHANGED
        (0x8017, 0x8018), // CLOAKED / UNCLOAKED
        (0x8027, 0x8030), // IME_* .. CONVTARGETCHANGED
    ];
    if all {
        v.push((0x8006, 0x800E)); // selection / state / name / value
    }
    v
}

pub fn wanted(ev: u32, idobj: i32, idchild: i32, all: bool) -> bool {
    match ev {
        EVENT_OBJECT_CREATE | EVENT_OBJECT_DESTROY | EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE => {
            idobj == OBJID_CARET || all || (idobj == OBJID_WINDOW && idchild == 0)
        }
        EVENT_OBJECT_LOCATIONCHANGE => idobj == OBJID_CARET,
        EVENT_OBJECT_REORDER => all,
        _ => true,
    }
}

pub fn is_ime_event(ev: u32) -> bool {
    (EVENT_OBJECT_IME_SHOW..=EVENT_OBJECT_TEXTEDIT_CONVERSIONTARGETCHANGED).contains(&ev)
        && ev != 0x802A
}
