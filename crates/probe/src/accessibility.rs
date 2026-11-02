//! Explicitly enabled, out-of-process structural UIA diagnostics.

use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc, Mutex,
};
const CAPACITY: usize = 32;

#[derive(Clone)]
struct Output {
    tx: SyncSender<Value>,
    drops: Arc<AtomicU64>,
}
impl Output {
    fn push(&self, record: Value) {
        if self.tx.try_send(record).is_err() {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(super) struct Probe {
    output: Output,
    rx: Receiver<Value>,
    targets: Arc<Mutex<Vec<u32>>>,
    stop: Arc<AtomicBool>,
}
impl Probe {
    pub(super) fn start(pids: Vec<u32>) -> std::io::Result<Option<Self>> {
        Self::start_with(
            std::env::var_os("PAXILE_ACCESSIBILITY_DIAGNOSTICS").as_deref(),
            pids,
            worker,
        )
    }
    fn start_with(
        value: Option<&std::ffi::OsStr>,
        pids: Vec<u32>,
        run: impl FnOnce(Output, Arc<Mutex<Vec<u32>>>, Arc<AtomicBool>) + Send + 'static,
    ) -> std::io::Result<Option<Self>> {
        if !enabled(value) {
            return Ok(None);
        }
        let probe = Self::channel(pids);
        let (output, targets, stop) = (
            probe.output.clone(),
            probe.targets.clone(),
            probe.stop.clone(),
        );
        // Deliberately detach: a provider can hang indefinitely in any COM call,
        // including Release/CoUninitialize. Never join from the message pump.
        // The stop flag is cooperative, NOT a hard provider deadline. A stuck
        // worker is retained until this controller process exits.
        std::thread::Builder::new()
            .name("paxile-uia-diagnostics".into())
            .spawn(move || run(output, targets, stop))?;
        Ok(Some(probe))
    }
    fn channel(pids: Vec<u32>) -> Self {
        let (tx, rx) = sync_channel(CAPACITY);
        Self {
            output: Output {
                tx,
                drops: Arc::new(AtomicU64::new(0)),
            },
            rx,
            targets: Arc::new(Mutex::new(pids)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(super) fn set_targets(&mut self, pids: &[u32]) {
        *self.targets.lock().unwrap() = pids.to_vec();
    }
    pub(super) fn drain(&self) -> Vec<Value> {
        self.rx.try_iter().take(CAPACITY).collect()
    }
    pub(super) fn dropped(&self) -> u64 {
        self.output.drops.load(Ordering::Relaxed)
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

fn for_target<T>(pid: u32, pids: &[u32], query: impl FnOnce() -> T) -> Option<T> {
    (pid != 0 && pids.contains(&pid)).then(query)
}

const MAX_DEPTH: usize = 3;
const MAX_CHILDREN: usize = 8;
const MAX_NODES: usize = 16;
#[derive(Default)]
struct Budget {
    nodes: usize,
}
impl Budget {
    fn take(&mut self, depth: usize, child: usize) -> bool {
        if depth > MAX_DEPTH || child >= MAX_CHILDREN || self.nodes >= MAX_NODES {
            return false;
        }
        self.nodes += 1;
        true
    }
}

#[derive(Default)]
struct Dedup {
    previous: Option<Value>,
    emitted: Option<std::time::Instant>,
}
impl Dedup {
    // Input is the structural payload only; query timing is deliberately excluded.
    fn classify(&mut self, structure: &Value, now: std::time::Instant) -> Option<bool> {
        let same = self.previous.as_ref() == Some(structure);
        if same
            && self
                .emitted
                .is_some_and(|last| now.duration_since(last).as_secs() < 30)
        {
            return None;
        }
        self.previous = Some(structure.clone());
        self.emitted = Some(now);
        Some(same)
    }
}

// APIs verified against the installed windows 0.58 registry bindings.
use windows::Win32::{
    System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    },
    UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationLegacyIAccessiblePattern,
        IUIAutomationValuePattern, UIA_IsTextPatternAvailablePropertyId,
        UIA_IsValuePatternAvailablePropertyId, UIA_LegacyIAccessiblePatternId, UIA_ValuePatternId,
        UIA_PROPERTY_ID,
    },
};

struct ComApartment;
impl ComApartment {
    fn initialize() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn numeric<T: Into<Value>>(result: windows::core::Result<T>) -> Value {
    match result {
        Ok(value) => serde_json::json!({"valid":true,"value":value.into()}),
        Err(error) => serde_json::json!({"valid":false,"hresult":error.code().0}),
    }
}
fn available(element: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Value {
    numeric(unsafe { element.GetCurrentPropertyValue(id) }.and_then(|v| bool::try_from(&v)))
}

fn metadata(element: &IUIAutomationElement, pid: u32) -> Value {
    let text_pattern = available(element, UIA_IsTextPatternAvailablePropertyId);
    let value_pattern = available(element, UIA_IsValuePatternAvailablePropertyId);
    let read_only = if value_pattern["value"] == true {
        numeric(unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .and_then(|pattern| pattern.CurrentIsReadOnly())
                .map(|b| b.as_bool())
        })
    } else {
        serde_json::json!({"valid":false,"queried":false})
    };
    // Legacy bridge: role/state/child-id only. Never acquire its underlying
    // IAccessible, Name/Value/Description/default action or any text/ranges.
    let legacy = match unsafe {
        element.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
            UIA_LegacyIAccessiblePatternId,
        )
    } {
        Ok(pattern) => serde_json::json!({"available":true,
            "role":numeric(unsafe { pattern.CurrentRole() }),
            "state":numeric(unsafe { pattern.CurrentState() }),
            "child_id":numeric(unsafe { pattern.CurrentChildId() })}),
        Err(error) => serde_json::json!({"available":false,"hresult":error.code().0}),
    };
    serde_json::json!({"pid":pid,
        "control_type":numeric(unsafe {element.CurrentControlType()}.map(|v| v.0)),
        "is_password":numeric(unsafe {element.CurrentIsPassword()}.map(|v|v.as_bool())),
        "text_pattern":text_pattern,"value_pattern":value_pattern,
        "value_read_only":read_only,"legacy_msaa":legacy})
}

fn relation_snapshot(
    automation: &IUIAutomation,
    focus: &IUIAutomationElement,
    pids: &[u32],
    stop: &AtomicBool,
) -> Value {
    let mut nodes = Vec::new();
    let mut timings = Vec::new();
    let mut budget = Budget::default();
    let mut truncated = false;
    let mut errors = Vec::new();
    let mut pid_errors = Vec::new();
    // Every element gets its own PID gate before structural queries, then a
    // second PID check afterwards. Foreign branches are not traversed.
    let mut add =
        |element: &IUIAutomationElement, relation: &str, depth: usize, child: usize| -> bool {
            if stop.load(Ordering::Relaxed) || !budget.take(depth, child) {
                return false;
            }
            let started = crate::log::qpc();
            let pid = match unsafe { element.CurrentProcessId() } {
                Ok(pid) if for_target(pid as u32, pids, || ()).is_some() => pid as u32,
                Ok(_) => return false,
                Err(error) => {
                    pid_errors.push(error.code().0);
                    return false;
                }
            };
            let mut node = metadata(element, pid);
            if unsafe { element.CurrentProcessId() }.ok() != Some(pid as i32) {
                return false;
            }
            node["relation"] = relation.into();
            node["depth"] = depth.into();
            node["index"] = child.into();
            nodes.push(node);
            timings.push(serde_json::json!({"qpc":started,"completed_qpc":crate::log::qpc()}));
            true
        };
    if !add(focus, "focus", 0, 0) {
        return serde_json::json!({"structure":{"valid":false,"nodes":[],"errors":pid_errors},"timings":timings});
    }
    match unsafe { automation.RawViewWalker() } {
        Ok(walker) => {
            let mut cursor = focus.clone();
            for depth in 1..=MAX_DEPTH {
                if stop.load(Ordering::Relaxed) {
                    truncated = true;
                    break;
                }
                match unsafe { walker.GetParentElement(&cursor) } {
                    Ok(parent) => {
                        if !add(&parent, "parent", depth, 0) {
                            truncated = true;
                            break;
                        }
                        cursor = parent;
                        if depth == MAX_DEPTH {
                            truncated = true;
                        }
                    }
                    Err(error) => {
                        errors.push(error.code().0);
                        break;
                    }
                }
            }
            // Only focus's immediate children, never recursive descendants.
            let mut next = unsafe { walker.GetFirstChildElement(focus) };
            for child in 0..MAX_CHILDREN {
                if stop.load(Ordering::Relaxed) {
                    truncated = true;
                    break;
                }
                match next {
                    Ok(element) => {
                        if !add(&element, "child", 1, child) {
                            truncated = true;
                            break;
                        }
                        if child + 1 == MAX_CHILDREN {
                            truncated = true;
                            break;
                        }
                        next = unsafe { walker.GetNextSiblingElement(&element) };
                    }
                    Err(error) => {
                        errors.push(error.code().0);
                        break;
                    }
                }
            }
        }
        Err(error) => errors.push(error.code().0),
    }
    errors.extend(pid_errors);
    truncated |= !errors.is_empty();
    serde_json::json!({"structure":{"valid":true,"nodes":nodes,"errors":errors,
        "truncated":truncated,"bounds":{"parent_depth":MAX_DEPTH,"immediate_children":MAX_CHILDREN,"total_nodes":MAX_NODES},
        "truncation_is_conservative":true},"timings":timings})
}

fn worker(output: Output, targets: Arc<Mutex<Vec<u32>>>, stop: Arc<AtomicBool>) {
    let result = (|| -> windows::core::Result<()> {
        let _apartment = ComApartment::initialize()?;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        let mut dedup = Dedup::default();
        while !stop.load(Ordering::Relaxed) {
            let pids = targets.lock().unwrap().clone();
            if !pids.is_empty() {
                let started = crate::log::qpc();
                // Only global focus lookup + PID are acquired before the gate.
                if let Ok(element) = unsafe { automation.GetFocusedElement() } {
                    if let Ok(pid) = unsafe { element.CurrentProcessId() } {
                        if let Some(record) = for_target(pid as u32, &pids, || {
                            relation_snapshot(&automation, &element, &pids, &stop)
                        }) {
                            // Recheck managed membership after potentially slow provider calls.
                            let current = targets.lock().unwrap().clone();
                            if !stop.load(Ordering::Relaxed)
                                && current.contains(&(pid as u32))
                                && record["structure"]["nodes"]
                                    .as_array()
                                    .is_some_and(|nodes| {
                                        nodes.iter().all(|node| {
                                            node["pid"]
                                                .as_u64()
                                                .is_some_and(|id| current.contains(&(id as u32)))
                                        })
                                    })
                            {
                                if let Some(heartbeat) =
                                    dedup.classify(&record["structure"], std::time::Instant::now())
                                {
                                    output.push(serde_json::json!({"uia":"structural_metadata",
                                        "qpc":started,"completed_qpc":crate::log::qpc(),
                                        "heartbeat":heartbeat,"snapshot":record}));
                                }
                            }
                        }
                    }
                }
            }
            // At most one query in flight; 200ms minimum spacing, not a call deadline.
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Ok(())
    })();
    if let Err(error) = result {
        // HRESULT only: provider error descriptions may contain sensitive strings.
        output.push(serde_json::json!({"uia":"unavailable", "hresult":error.code().0, "qpc":crate::log::qpc()}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_query_shape_never_serializes_provider_error_text() {
        let result = numeric::<u32>(Err(windows::core::Error::from_hresult(
            windows::core::HRESULT(0x80004005u32 as i32),
        )));
        assert_eq!(
            result,
            serde_json::json!({"valid":false,"hresult":0x80004005u32 as i32})
        );
        assert_eq!(
            numeric(Ok(42u32)),
            serde_json::json!({"valid":true,"value":42})
        );
    }
    #[test]
    fn identical_structure_is_suppressed_until_heartbeat() {
        let mut gate = Dedup::default();
        let start = std::time::Instant::now();
        let a = serde_json::json!({"nodes":[{"pid":11,"role":42}]});
        assert_eq!(gate.classify(&a, start), Some(false));
        assert_eq!(
            gate.classify(&a, start + std::time::Duration::from_secs(1)),
            None
        );
        assert_eq!(
            gate.classify(&a, start + std::time::Duration::from_secs(30)),
            Some(true)
        );
        assert_eq!(
            gate.classify(
                &serde_json::json!({"nodes":[]}),
                start + std::time::Duration::from_secs(31)
            ),
            Some(false)
        );
    }
    #[test]
    fn structural_bounds_are_enforced() {
        let mut budget = Budget::default();
        assert!(!budget.take(4, 0));
        assert!(!budget.take(1, 8));
        for _ in 0..16 {
            assert!(budget.take(0, 0));
        }
        assert!(!budget.take(0, 0));
    }
    #[test]
    fn disabled_start_never_spawns_worker() {
        assert!(
            Probe::start_with(None, vec![], |_, _, _| panic!("must not spawn"))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn queue_is_bounded_counts_loss_and_does_not_wait_for_worker() {
        let mut probe = Probe::channel(vec![11]);
        for n in 0..CAPACITY + 3 {
            probe.output.push(serde_json::json!({"qpc":n}));
        }
        assert_eq!(probe.drain().len(), CAPACITY);
        assert_eq!(probe.dropped(), 3);
        probe.set_targets(&[22]);
        assert_eq!(*probe.targets.lock().unwrap(), vec![22]);
        let stop = probe.stop.clone();
        drop(probe);
        assert!(stop.load(Ordering::Relaxed));
    }
    #[test]
    fn foreign_pid_never_queries_metadata() {
        assert_eq!(
            for_target(22, &[11], || panic!("foreign metadata")),
            None::<u8>
        );
        assert_eq!(for_target(11, &[11], || 7), Some(7));
        assert_eq!(for_target(0, &[0], || panic!("invalid pid")), None::<u8>);
    }
    #[test]
    fn opt_in_requires_exact_one() {
        assert!(enabled(Some(std::ffi::OsStr::new("1"))));
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some(" 1"),
            Some("1 "),
        ] {
            assert!(!enabled(value.map(std::ffi::OsStr::new)));
        }
    }
}
