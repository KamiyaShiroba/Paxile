use super::*;
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use windows::Win32::System::Threading::GetCurrentProcessId;

#[test]
fn duplex_pipe_transports_config_lease_and_stop() {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let pid = unsafe { GetCurrentProcessId() };
    let config = paxile_policy::Config {
        enabled: true,
        allow_exe: vec!["safe.exe".into()],
        ..Default::default()
    };
    let control = Arc::new(std::sync::Mutex::new(paxile_policy::BackendControl::new(
        true,
    )));
    control.lock().unwrap().initialize(config.clone());
    let server_control = control.clone();
    // Start the actual DLL before any controller exists. The old worker gives
    // up permanently after 30 * 100 ms; this deliberately exceeds that window.
    unsafe {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::CWPSTRUCT;
        let dll = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("paxile_spy.dll");
        let wide = dll.as_os_str().encode_wide_nul();
        let module = LoadLibraryW(PCWSTR(wide.as_ptr())).expect("build matching DLL before test");
        let hook: unsafe extern "system" fn(
            i32,
            WPARAM,
            LPARAM,
        ) -> windows::Win32::Foundation::LRESULT =
            std::mem::transmute(GetProcAddress(module, s!("paxile_callwnd_proc")).unwrap());
        let message = CWPSTRUCT {
            message: 0x0282,
            wParam: WPARAM(9),
            ..Default::default()
        };
        hook(0, WPARAM(0), LPARAM(&message as *const _ as isize));
    }
    std::thread::sleep(std::time::Duration::from_millis(3500));
    let server =
        std::thread::spawn(move || serve(tx, vec![pid], config, server_stop, server_control));
    let mut pipe = None;
    for _ in 0..100 {
        if let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(r"\\.\pipe\paxile-spy")
        {
            pipe = Some(f);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut pipe = pipe.expect("pipe server ready within 1s");

    let mut size = [0; 4];
    pipe.read_exact(&mut size).unwrap();

    let mut bytes = vec![0; u32::from_le_bytes(size) as usize];
    pipe.read_exact(&mut bytes).unwrap();

    let c: paxile_policy::Config = serde_json::from_slice(&bytes).unwrap();
    assert!(c.enabled);
    assert_eq!(c.allow_exe, ["safe.exe"]);
    let mut lease = [0; 9];
    pipe.read_exact(&mut lease).unwrap();

    assert_eq!(lease[0], 1);
    control
        .lock()
        .unwrap()
        .execute(paxile_policy::ControlCommand::Off)
        .unwrap();
    pipe.read_exact(&mut lease).unwrap();
    assert_eq!(lease[0], 2);
    assert_eq!(u64::from_le_bytes(lease[1..].try_into().unwrap()), 1);
    control
        .lock()
        .unwrap()
        .execute(paxile_policy::ControlCommand::On)
        .unwrap();
    pipe.read_exact(&mut lease).unwrap();
    assert_eq!(lease[0], 1);
    assert_eq!(u64::from_le_bytes(lease[1..].try_into().unwrap()), 2);
    use std::io::Write;
    pipe.write_all(b"{\"hello\":true,\"synthetic\":true}\n")
        .unwrap();
    let mut early_dll_hello = false;
    let mut worker_tid = None;
    loop {
        let line = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
        if event["synthetic"] == true {
            assert_eq!(event, serde_json::json!({"hello":true,"synthetic":true}));
            break;
        }
        if event["hello"] == true {
            assert_eq!(event["pid"].as_u64(), Some(pid as u64));
            assert!(event["tid"].as_u64().is_some_and(|tid| tid > 0));
            worker_tid = event["tid"].as_u64();
            early_dll_hello = true;
        }
    }
    // Exercise the actual built DLL and both exported hooks in this test's UI
    // thread. The hidden STATIC window is never focused or shown.
    unsafe {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::WindowsAndMessaging::*;
        let dll = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("paxile_spy.dll");
        let wide = dll.as_os_str().encode_wide_nul();
        let module =
            LoadLibraryW(PCWSTR(wide.as_ptr())).expect("cargo build --workspace before tests");
        let sent: HOOKPROC = std::mem::transmute::<unsafe extern "system" fn() -> isize, HOOKPROC>(
            GetProcAddress(module, s!("paxile_callwnd_proc")).unwrap(),
        );
        let posted: HOOKPROC = std::mem::transmute::<unsafe extern "system" fn() -> isize, HOOKPROC>(
            GetProcAddress(module, s!("paxile_getmsg_proc")).unwrap(),
        );
        let a = SetWindowsHookExW(
            WH_CALLWNDPROC,
            sent,
            HINSTANCE(module.0),
            GetCurrentThreadId(),
        )
        .unwrap();
        let b = SetWindowsHookExW(
            WH_GETMESSAGE,
            posted,
            HINSTANCE(module.0),
            GetCurrentThreadId(),
        )
        .unwrap();
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Paxile hook test"),
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
        SendMessageW(hwnd, WM_IME_NOTIFY, WPARAM(9), LPARAM(0));
        PostMessageW(hwnd, WM_IME_STARTCOMPOSITION, WPARAM(0), LPARAM(0)).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let (mut hello, mut send, mut post) = (early_dll_hello, false, false);
        while std::time::Instant::now() < deadline && !(hello && send && post) {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            for line in rx.try_iter() {
                let r: serde_json::Value = serde_json::from_str(&line).unwrap();
                if r["hello"] == true {
                    assert_eq!(r["pid"].as_u64(), Some(pid as u64));
                    assert!(r["tid"].as_u64().is_some_and(|tid| tid > 0));
                    worker_tid = r["tid"].as_u64();
                    hello = true;
                }
                send |= r["src"] == "send" && r["msg"] == "WM_IME_NOTIFY";
                post |= r["src"] == "post" && r["msg"] == "WM_IME_STARTCOMPOSITION";
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        UnhookWindowsHookEx(a).unwrap();
        UnhookWindowsHookEx(b).unwrap();
        DestroyWindow(hwnd).unwrap();
        assert!(
            (hello && send && post),
            "actual DLL records missing: hello={hello} send={send} post={post}"
        );
        // The same actual DLL worker/pipe must survive both runtime transitions.
        for (command, revision, enabled) in [
            (paxile_policy::ControlCommand::Off, 3, false),
            (paxile_policy::ControlCommand::On, 4, true),
        ] {
            control.lock().unwrap().execute(command).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut received = false;
            while std::time::Instant::now() < deadline && !received {
                for line in rx.try_iter() {
                    let r: serde_json::Value = serde_json::from_str(&line).unwrap();
                    received |= r["backend"] == "control_received"
                        && r["revision"] == revision
                        && r["enabled"] == enabled;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(received, "DLL did not receive runtime revision {revision}");
        }
    }
    let mut revised = control.lock().unwrap().policy();
    revised.allow_exe = vec!["updated.exe".into()];
    control.lock().unwrap().reload(revised).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut applied = false;
    while std::time::Instant::now() < deadline && !applied {
        for line in rx.try_iter() {
            let r: serde_json::Value = serde_json::from_str(&line).unwrap();
            applied |= r["backend"] == "config_received" && r["revision"] == 5;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        applied,
        "same DLL must accept updated rules without restarting"
    );
    // The synthetic legacy peer never acknowledges type-3 config frames.
    // It must produce a visible diagnostic, unlike the matching DLL above.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut unconfirmed = false;
    while std::time::Instant::now() < deadline && !unconfirmed {
        for line in rx.try_iter() {
            let event: serde_json::Value = serde_json::from_str(&line).unwrap();
            unconfirmed |= event["runtime_unconfirmed"]
                .as_str()
                .is_some_and(|message| message.contains("unsupported older pinned DLL"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        unconfirmed,
        "unacknowledged legacy DLL revision must not be silent"
    );
    stop.store(true, Ordering::Release);
    let _ = server.join();
    for _ in 0..100 {
        pipe.read_exact(&mut lease).unwrap();
        if lease[0] == 3 {
            let mut length = [0; 4];
            pipe.read_exact(&mut length).unwrap();
            let mut payload = vec![0; u32::from_le_bytes(length) as usize];
            pipe.read_exact(&mut payload).unwrap();
        }
        if lease[0] == 0 {
            break;
        }
    }
    assert_eq!(lease[0], 0, "server did not send stop");
    drop(pipe);
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Recreate the controller (not the target/DLL), resetting revision 5 to 0.
    // The persistent worker must initiate another validated hello unaided.
    for authorized in [false, true] {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let control = Arc::new(std::sync::Mutex::new(paxile_policy::BackendControl::new(
            authorized,
        )));
        let config = paxile_policy::Config {
            enabled: authorized,
            allow_exe: vec!["new-controller.exe".into()],
            ..Default::default()
        };
        control.lock().unwrap().initialize(config.clone());
        let server_stop = stop.clone();
        let server_control = control.clone();
        let server =
            std::thread::spawn(move || serve(tx, vec![pid], config, server_stop, server_control));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let mut hello = false;
        let mut revision_zero = false;
        while std::time::Instant::now() < deadline && !(hello && (revision_zero || !authorized)) {
            for line in rx.try_iter() {
                let event: serde_json::Value = serde_json::from_str(&line).unwrap();
                if event["hello"] == true {
                    assert_eq!(event["pid"].as_u64(), Some(pid as u64));
                    assert_eq!(
                        event["tid"].as_u64(),
                        worker_tid,
                        "same persistent worker must reconnect"
                    );
                    hello = true;
                }
                revision_zero |= event["backend"] == "control_received"
                    && event["revision"] == 0
                    && event["enabled"] == authorized;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(hello, "same DLL did not reconnect to recreated controller");
        if authorized {
            assert!(revision_zero, "new controller revision 0 was not accepted");
            for (command, revision, enabled) in [
                (paxile_policy::ControlCommand::Off, 1, false),
                (paxile_policy::ControlCommand::On, 2, true),
            ] {
                control.lock().unwrap().execute(command).unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                let mut received = false;
                while std::time::Instant::now() < deadline && !received {
                    for line in rx.try_iter() {
                        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
                        received |= event["backend"] == "control_received"
                            && event["revision"] == revision
                            && event["enabled"] == enabled;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(received, "reconnected DLL missed revision {revision}");
            }
        } else {
            assert!(control
                .lock()
                .unwrap()
                .execute(paxile_policy::ControlCommand::On)
                .is_err());
        }
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut disconnected = false;
        while std::time::Instant::now() < deadline && !disconnected {
            for line in rx.try_iter() {
                let event: serde_json::Value = serde_json::from_str(&line).unwrap();
                disconnected |= event["disconnected"] == true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(disconnected);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
