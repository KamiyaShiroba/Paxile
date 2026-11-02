use super::*;
use windows::core::w;
use windows::Win32::UI::Input::Ime::{ImmCreateContext, ImmDestroyContext};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
};
#[test]
fn candidate_origin_records_helper_contamination_without_changing_evidence() {
    let _guard = crate::tests::TEST_LOCK.lock().unwrap();
    crate::STATE.store(crate::ST_RUNNING, Ordering::Release);
    let source = candidate_origin(0x10, 0x20, "IME", false, 123);
    assert_eq!(source["source_hwnd"], "0x10");
    assert_eq!(source["focus_hwnd"], "0x20");
    assert_eq!(source["source_class"], "IME");
    assert_eq!(source["source_matches_focus"], false);
    assert_eq!(source["phase"], "callwnd_before_downstream");
    let mut evidence = CandidateEvidence::default();
    evidence.record(0x20, 123);
    assert!(evidence.recent(0x20, 123));
    assert!(!evidence.recent(0x10, 123));
}

#[test]
fn hidden_window_context_restored_on_expired_lease() {
    let _guard = crate::tests::TEST_LOCK.lock().unwrap();
    crate::STATE.store(crate::ST_RUNNING, Ordering::Release);
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Paxile IMM test"),
            WINDOW_STYLE(0),
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let context = ImmCreateContext();
        assert!(!context.is_invalid());
        let original = ImmAssociateContext(hwnd, context);
        LEASE.store(GetTickCount64(), Ordering::Release);
        CONTROL.store(1, Ordering::Release);
        assert!(detach(hwnd));
        assert!(ImmGetContext(hwnd).is_invalid());
        LOCAL.with(|s| {
            s.borrow_mut()
                .evidence
                .record(hwnd.0 as usize, GetTickCount64())
        });
        CONTROL.store(2, Ordering::Release); // revision 1, disabled; lease stays live
        watchdog(HWND::default(), 0, 0, 0);
        let restored = ImmGetContext(hwnd);
        assert_eq!(restored, context);
        let _ = ImmReleaseContext(hwnd, restored);
        assert!(!detach(hwnd));
        assert!(!LOCAL.with(|s| s
            .borrow()
            .evidence
            .recent(hwnd.0 as usize, GetTickCount64())));
        CONTROL.store(5, Ordering::Release); // revision 2, enabled, same connection
        assert!(detach(hwnd));
        assert!(ImmGetContext(hwnd).is_invalid());
        LEASE.store(0, Ordering::Release);
        watchdog(HWND::default(), 0, 0, 0);
        let restored = ImmGetContext(hwnd);
        assert_eq!(restored, context);
        let _ = ImmReleaseContext(hwnd, restored);
        assert!(LOCAL.with(|s| s.borrow().detached.is_none()));
        // A replacement controller may reuse the exact enabled/revision word.
        // No callback ran during the gap: session identity must still restore
        // the old HIMC and discard candidate evidence before another decision.
        CONTROL.store(1, Ordering::Release);
        LEASE.store(GetTickCount64(), Ordering::Release);
        assert!(detach(hwnd));
        LOCAL.with(|s| {
            s.borrow_mut()
                .evidence
                .record(hwnd.0 as usize, GetTickCount64())
        });
        SESSION.fetch_add(1, Ordering::AcqRel);
        sync_control();
        let restored = ImmGetContext(hwnd);
        assert_eq!(restored, context);
        let _ = ImmReleaseContext(hwnd, restored);
        assert!(!LOCAL.with(|s| s
            .borrow()
            .evidence
            .recent(hwnd.0 as usize, GetTickCount64())));
        ImmAssociateContext(hwnd, original);
        let _ = ImmDestroyContext(context);
        DestroyWindow(hwnd).unwrap();
    }
}
