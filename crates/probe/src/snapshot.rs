//! One-shot dump of every window the target owns, arranged as an owner forest
//! (floating palettes / popups hang off the window that owns them), optionally
//! with each window's child-HWND tree.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::json;
use windows::Win32::Foundation::HWND;

use crate::log::Logger;
use crate::win::{descendants, describe, hx, top_levels, WinDesc};

pub fn run(pids: &[u32], children: bool, log: &mut Logger) {
    let tops: Vec<WinDesc> = top_levels()
        .into_iter()
        .map(describe)
        .filter(|d| pids.contains(&d.pid))
        .collect();
    let ids: HashSet<usize> = tops.iter().map(|d| d.hwnd).collect();
    let mut owned: HashMap<usize, Vec<&WinDesc>> = HashMap::new();
    let mut roots = Vec::new();
    for d in &tops {
        if d.owner != 0 && ids.contains(&d.owner) {
            owned.entry(d.owner).or_default().push(d);
        } else {
            roots.push(d);
        }
    }

    let mut classes: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for d in &tops {
        let e = classes.entry(d.class.clone()).or_default();
        e.0 += 1;
        e.1 += d.visible as u32;
    }

    let visible = tops.iter().filter(|d| d.visible).count();
    log.note(&format!(
        "== {} top-level windows ({} visible) for pid {:?}; indentation = owner chain",
        tops.len(),
        visible,
        pids
    ));
    for r in &roots {
        print_owned(r, &owned, 0, children, log);
    }
    log.note("== top-level classes (total / visible):");
    for (c, (n, v)) in &classes {
        log.note(&format!("   {n:>4} / {v:<4} {c:?}"));
    }

    log.emit(
        "snapshot",
        &format!("snapshot: {} top-level windows", tops.len()),
        json!({
            "pids": pids,
            "windows": tops.iter().map(|d| {
                let mut j = d.to_json();
                if children {
                    j["children"] = json!(descendants(HWND(d.hwnd as *mut _))
                        .into_iter().map(|h| describe(h).to_json()).collect::<Vec<_>>());
                }
                j
            }).collect::<Vec<_>>(),
        }),
    );
}

fn print_owned(
    d: &WinDesc,
    owned: &HashMap<usize, Vec<&WinDesc>>,
    depth: usize,
    children: bool,
    log: &Logger,
) {
    log.note(&format!("{}{}", "  ".repeat(depth), d.line()));
    if children {
        print_children(d, depth + 1, log);
    }
    for o in owned.get(&d.hwnd).into_iter().flatten() {
        print_owned(o, owned, depth + 1, children, log);
    }
}

fn print_children(top: &WinDesc, depth: usize, log: &Logger) {
    let kids: Vec<WinDesc> = descendants(HWND(top.hwnd as *mut _))
        .into_iter()
        .map(describe)
        .collect();
    if kids.is_empty() {
        return;
    }
    let mut by_parent: HashMap<usize, Vec<&WinDesc>> = HashMap::new();
    for k in &kids {
        by_parent.entry(k.parent).or_default().push(k);
    }
    fn rec(p: usize, by: &HashMap<usize, Vec<&WinDesc>>, depth: usize, log: &Logger) {
        for k in by.get(&p).into_iter().flatten() {
            log.note(&format!("{}· {}", "  ".repeat(depth), k.line()));
            rec(k.hwnd, by, depth + 1, log);
        }
    }
    log.note(&format!(
        "{}[{} child hwnds of {}]",
        "  ".repeat(depth),
        kids.len(),
        hx(top.hwnd)
    ));
    rec(top.hwnd, &by_parent, depth, log);
}
