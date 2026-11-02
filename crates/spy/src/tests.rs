use super::*;

#[test]
fn tsf_result_distinguishes_absence_failure_and_presence_without_text() {
    let absent = tsf_result("thread_manager", 0, false);
    assert_eq!(absent["state"], "absent");
    assert_eq!(absent["hresult"], 0);
    let failure = tsf_result("thread_manager", -2147467259, false);
    assert_eq!(failure["state"], "error");
    let present = tsf_result("thread_manager", 0, true);
    assert_eq!(present["state"], "present");
    assert_eq!(present.as_object().unwrap().len(), 3);
    assert!(present.get("text").is_none());
}

#[test]
fn tsf_is_build_opt_in_selected_and_passive() {
    let source = include_str!("lib.rs");
    assert!(source.contains("record[\"tsf\"]"));
    let source = include_str!("tsf.rs");
    for forbidden in [
        "CoInitialize",
        "TF_CreateThreadMgr",
        ".Activate(",
        ".SetFocus(",
        "RequestEditSession",
        "GetText",
        "GetRange",
    ] {
        assert!(!source.contains(forbidden), "forbidden {forbidden}");
    }
    assert!(source.contains("[None; 1]"));
    assert!(source.contains("TF_GetThreadMgr"));
}

#[test]
fn return_observation_is_selected_and_never_reads_request_payload() {
    let record =
        sent_return_record(HWND::default(), 0x0288, WPARAM(6), LPARAM(1), LRESULT(42)).unwrap();
    assert_eq!(record["lresult"], 42);
    assert_eq!(record["request"], 6);
    assert!(record.get("lp").is_none());
    assert!(
        sent_return_record(HWND::default(), 0x000f, WPARAM(0), LPARAM(1), LRESULT(0)).is_none()
    );
}

#[test]
fn return_observation_includes_natural_dlgcode_and_owner_metadata() {
    let record = sent_return_record(
        HWND::default(),
        0x0087,
        WPARAM(0xe5),
        LPARAM(0x1234),
        LRESULT(0x0004),
    )
    .unwrap();
    assert_eq!(record["msg"], "WM_GETDLGCODE");
    assert_eq!(record["original_key"], 0xe5);
    assert_eq!(record["lresult"], 0x0004);
    assert!(record["owner_snapshot"].is_object());
    assert!(record["natural"].as_bool().unwrap());
}

#[test]
fn dlgcode_is_selected_but_arbitrary_request_payload_is_not_read() {
    let record = sent_return_record(
        HWND::default(),
        0x0087,
        WPARAM(0x41),
        LPARAM(usize::MAX as isize),
        LRESULT(0),
    )
    .unwrap();
    assert!(record.get("lp").is_none());
    assert!(
        sent_return_record(HWND::default(), 0x000f, WPARAM(0), LPARAM(1), LRESULT(0)).is_none()
    );
}

#[test]
fn dlgcode_v4_identity_and_capability_are_explicit() {
    let source = include_str!("lib.rs");
    assert!(source.contains("/preview-passive-v4"));
    assert!(source.contains("\"diagnostic_schema\":4"));
    assert_eq!(source.matches("\"dlgcode_return\"").count(), 2);
    assert!(!source.contains("SendMessage"));
}

#[test]
fn natural_dlgcode_and_key_candidate_click_share_producer_timeline() {
    let _guard = TEST_LOCK.lock().unwrap();
    STATE.store(ST_RUNNING, Ordering::Release);
    BUF.lock().unwrap().clear();
    for (msg, key) in [(0x0201, 0), (0x0087, 0xe5), (0x0100, 0xe5), (0x0282, 9)] {
        if msg == 0x0087 {
            push_value(
                sent_return_record(HWND::default(), msg, WPARAM(key), LPARAM(1), LRESULT(4))
                    .unwrap(),
            );
        } else {
            on_msg(
                "correlation_test",
                HWND::default(),
                msg,
                WPARAM(key),
                LPARAM(0),
                None,
            );
        }
    }
    let records: Vec<Value> = BUF
        .lock()
        .unwrap()
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(records.len(), 4);
    assert_eq!(records[1]["msg"], "WM_GETDLGCODE");
    for pair in records.windows(2) {
        assert!(pair[0]["seq"].as_u64().unwrap() < pair[1]["seq"].as_u64().unwrap());
        assert!(pair[0]["qpc"].as_i64().unwrap() <= pair[1]["qpc"].as_i64().unwrap());
        assert_eq!(pair[0]["tid"], pair[1]["tid"]);
        assert!(pair[1]["dropped"].is_u64());
    }
}

#[test]
fn snapshots_select_pre_key_focus_click_not_high_rate_noise() {
    for msg in [0x0100, 0x0104, 0x0007, 0x0008, 0x0201, 0x0204, 0x0246] {
        assert!(snapshot_selected(msg));
    }
    for msg in [0x0200, 0x000f, 0x0102] {
        assert!(!snapshot_selected(msg));
    }
}

#[test]
fn operation_pairs_preserve_result_and_nested_depth() {
    let _guard = TEST_LOCK.lock().unwrap();
    STATE.store(ST_RUNNING, Ordering::Release);
    BUF.lock().unwrap().clear();
    let returned = blocker::imm_operation("test_outer", HWND::default(), || {
        assert_eq!(blocker::reentry_depth(), 1);
        blocker::imm_operation("test_inner", HWND::default(), || {
            assert_eq!(blocker::reentry_depth(), 2);
            17
        })
    });
    assert_eq!(returned, 17);
    assert_eq!(blocker::reentry_depth(), 0);
    let records: Vec<Value> = BUF
        .lock()
        .unwrap()
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(records.len(), 4);
    assert_eq!(records[0]["phase"], "begin");
    assert_eq!(records[1]["depth"], 2);
    assert_eq!(records[2]["result"], 17);
    assert_eq!(records[0]["operation_id"], records[3]["operation_id"]);
    assert!(records[3]["qpc"].as_i64().unwrap() >= records[0]["qpc"].as_i64().unwrap());
}

#[test]
fn every_notify_origin_survives_inactive_lease_and_helper_overwrite() {
    let _guard = TEST_LOCK.lock().unwrap();
    STATE.store(ST_RUNNING, Ordering::Release);
    BUF.lock().unwrap().clear();
    for source in [1usize, 2] {
        blocker::observe(HWND(source as *mut _), 0x0282, WPARAM(9), LPARAM(1), false);
    }
    let records: Vec<Value> = BUF
        .lock()
        .unwrap()
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .filter(|v| v["backend"] == "candidate_notify_origin")
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["source_hwnd"], "0x1");
    assert_eq!(records[1]["source_hwnd"], "0x2");
}

pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn character_payload_is_redacted() {
    let _guard = TEST_LOCK.lock().unwrap();
    STATE.store(ST_RUNNING, Ordering::Release);
    for msg in [0x0102, 0x0106, 0x0286, 0x010f] {
        on_msg(
            "privacy_test",
            HWND::default(),
            msg,
            WPARAM(0x79d8),
            LPARAM(0),
            None,
        );
    }
    let b = BUF.lock().unwrap();
    let records: Vec<Value> = b
        .iter()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v["src"] == "privacy_test")
        .collect();
    assert_eq!(records.len(), 4);
    for r in records {
        assert!(r.get("ch").is_none());
        assert!(r.get("comp").is_none());
        assert!(r.get("result").is_none());
        assert!(r.get("wp").is_none());
        assert!(r.get("lp").is_none());
    }
}

#[test]
fn diagnostic_contract_has_origin_phase_restore_and_timing() {
    let source = include_str!("blocker.rs");
    for field in [
        "candidate_source",
        "source_hwnd",
        "source_class",
        "focus_hwnd",
        "phase",
        "reason",
    ] {
        assert!(source.contains(field), "missing {field}");
    }
    let source = include_str!("lib.rs");
    for field in [
        "hook_entry_qpc",
        "downstream_return_qpc",
        "observe_end_qpc",
        "diagnostic_schema",
        "capabilities",
        "comp_len",
        "result_len",
    ] {
        assert!(source.contains(field), "missing {field}");
    }
}

#[test]
fn overflow_is_counted_and_sequence_has_a_gap() {
    let _guard = TEST_LOCK.lock().unwrap();
    STATE.store(ST_RUNNING, Ordering::Release);
    let before = DROPPED.load(Ordering::Relaxed);
    {
        let mut b = BUF.lock().unwrap();
        b.resize(MAX_BUFFERED, String::new());
    }
    push(json!({"backend":"overflow_test"}).to_string());
    let discarded_seq = SEQ.load(Ordering::Relaxed);
    assert!(DROPPED.load(Ordering::Relaxed) > before);
    BUF.lock().unwrap().clear();
    push(json!({"backend":"after_overflow"}).to_string());
    let b = BUF.lock().unwrap();
    let r: Value = serde_json::from_str(b.last().unwrap()).unwrap();
    assert!(r["seq"].as_u64().unwrap() > discarded_seq);
    assert!(r["dropped"].as_u64().unwrap() > before);
}

#[test]
fn pushed_records_have_producer_identity_and_order() {
    let _guard = TEST_LOCK.lock().unwrap();
    STATE.store(ST_RUNNING, Ordering::Release);
    push(json!({"backend":"diagnostic_test"}).to_string());
    push(json!({"backend":"diagnostic_test"}).to_string());
    let b = BUF.lock().unwrap();
    let records: Vec<Value> = b
        .iter()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v["backend"] == "diagnostic_test")
        .collect();
    assert_eq!(records.len(), 2);
    for r in &records {
        assert!(r["qpc"].as_i64().is_some_and(|q| q > 0));
        assert_eq!(r["pid"], unsafe { GetCurrentProcessId() });
        assert_eq!(r["tid"], unsafe { GetCurrentThreadId() });
        assert!(r["dropped"].is_u64());
    }
    assert!(records[0]["seq"].as_u64().unwrap() < records[1]["seq"].as_u64().unwrap());
}
