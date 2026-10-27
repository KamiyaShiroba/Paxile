use super::*;

#[test]
fn first_key_replay_covers_every_canvas_composition() {
    // The key the IME claimed for the composition we cancelled is replayed.
    let key = PendingKey {
        scan: 0x23,
        extended: false,
        at: 1_000,
    };
    assert_eq!(should_replay(Some(key), 1_200, false), Some(key));
    // Text intent (candidate evidence) must never be replayed.
    assert_eq!(should_replay(Some(key), 1_200, true), None);
    // No key was claimed (composition opened by something else).
    assert_eq!(should_replay(None, 1_200, false), None);
    // The stored key belongs to an older composition.
    assert_eq!(should_replay(Some(key), 1_401, false), None);
}

#[test]
fn a_claimed_key_is_settled_only_after_the_grace_period() {
    let key = PendingKey {
        scan: 0x23,
        extended: false,
        at: 1_000,
    };
    assert!(!claim_expired(key, 1_000 + CLAIM_GRACE_MS - 1));
    assert!(claim_expired(key, 1_000 + CLAIM_GRACE_MS));
}

#[test]
fn clearing_replay_state_prevents_a_second_replay() {
    LOCAL.with(|s| {
        let mut s = s.borrow_mut();
        s.key = Some(PendingKey {
            scan: 0x23,
            extended: false,
            at: 500,
        });
        s.key_hwnd = 0x1234;
    });
    clear_replay();
    LOCAL.with(|s| {
        let borrowed = s.borrow();
        assert_eq!(borrowed.key, None);
        assert_eq!(borrowed.key_hwnd, 0);
        assert_eq!(should_replay(borrowed.key, 600, false), None);
    });
}
use windows::core::w;
use windows::Win32::UI::Input::Ime::{ImmCreateContext, ImmDestroyContext};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
};
#[test]
fn hidden_window_context_restored_on_expired_lease() {
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
