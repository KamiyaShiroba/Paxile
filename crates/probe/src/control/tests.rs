use super::*;
#[test]
fn external_cli_controls_live_backend_without_stdin() {
    let state = Arc::new(Mutex::new(BackendControl::new(true)));
    let server = Server::start(state.clone()).unwrap();
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("paxile-probe.exe");
    for (command, enabled) in [
        ("off", false),
        ("on", true),
        ("toggle", false),
        ("status", false),
    ] {
        let output = std::process::Command::new(&exe)
            .args([
                "control",
                command,
                "--session",
                &std::process::id().to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["status"]["requested_enabled"], enabled);
        assert!(response["status"]["applied_enabled"].is_null());
        assert_eq!(state.lock().unwrap().status().requested_enabled, enabled);
    }
    let config = paxile_policy::Config {
        allow_exe: vec!["safe.exe".into()],
        ..Default::default()
    };
    let response = request(
        std::process::id(),
        &format!("reload {}", serde_json::to_string(&config).unwrap()),
    )
    .unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(state.lock().unwrap().policy().allow_exe, ["safe.exe"]);
    assert!(request(std::process::id(), "invalid").unwrap()["ok"] == false);
    drop(server);
    let state = Arc::new(Mutex::new(BackendControl::new(false)));
    let _server = Server::start(state.clone()).unwrap();
    assert_eq!(request(std::process::id(), "on").unwrap()["ok"], false);
    assert!(!state.lock().unwrap().status().requested_enabled);
}
