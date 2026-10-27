# Experimental IME backend

Build and verify on Windows x64:

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If a previously injected, process-pinned DLL locks `target/debug/paxile_spy.dll`, do not kill the user's application to rebuild. Use `--locked --target-dir target/runtime-control` for all three commands; the new matching EXE/DLL pair is then in `target/runtime-control/debug`. The old target still needs a user-managed restart before loading the new DLL.

The probe integration test loads `paxile_spy.dll` beside the selected target directory's EXE, so build before testing on a clean checkout. It starts that DLL with no controller for 3.5 seconds, verifies late connection and both exported hooks on a hidden STATIC window, then stops and recreates controllers twice without restarting the target process or worker (asserting identical target PID and worker TID). It verifies revision reset, read-only startup denial, and off/on after reconnect. The IMM test creates a separate HIMC and verifies exact restoration on disable, expired lease, and a changed session generation even when the new controller reuses the same control word, with stale candidate evidence cleared. Tests do not focus, show, or inject input into a user's application. For an isolated matching build/test pair use `cargo build --workspace --locked --target-dir target/reconnect`, then `cargo test --workspace --locked --target-dir target/reconnect` and `cargo clippy --workspace --all-targets --locked --target-dir target/reconnect -- -D warnings`. These are hidden-window/transport regressions, not live CSP acceptance.

## Usage

```sh
target/debug/paxile-probe.exe watch --block --profile csp-experimental
target/debug/paxile-probe.exe watch --block --allow-exe CLIPStudioPaint.exe
target/debug/paxile-probe.exe watch --block --config backend.example.json
```

Default watch and `--spy` remain read-only. `--block` implies the spy. Only `--block` enables mutation, even if JSON says `enabled: true`. `--allow-exe` is repeatable, case-insensitive, and additive with JSON. An allowlisted executable never receives cancellation or detach calls. Unknown executable names, controls, profiles without an executable match, standard Edit/RichEdit controls, any system caret, and recent candidate evidence fail open. `--pid` identifies transport recipients; the DLL independently checks its actual executable basename against policy.

## Backend API and config

### Runtime control (no restart within a watch session)

Start `watch --block` once to authorize mutation. Type `/block off`, `/block on`, `/block toggle`, or `/block status` in that console. Other lines still produce MARK records. Disabling does **not** stop hooks, the DLL worker, event collection, or its connection. Re-enabling uses the same DLL and connection.

For automation or closed stdin, use the probe PID printed as `external --session N` (not the target application PID):

```sh
target/runtime-control/debug/paxile-probe.exe control off --session 12345
target/runtime-control/debug/paxile-probe.exe control on --session 12345
target/runtime-control/debug/paxile-probe.exe control toggle --session 12345
target/runtime-control/debug/paxile-probe.exe control status --session 12345
```

Replace 12345 with the running probe PID. Commands return one JSON response. `ok:false` returns exit 1; invalid syntax, absent session, and timeout return exit 2. Control sessions use `\\.\pipe\paxile-control-PROBE_PID`, reject remote clients, accept only exact on/off/toggle/status UTF-8 requests, cap requests at 64 bytes and responses at 4096 bytes, and bound a connection to 500 ms and the client operation to 2 seconds. Default Windows pipe ACLs apply; this is a local single-user prototype, not an authenticated service or a privilege boundary. Only one spy session is supported by the existing fixed DLL event pipe even though control routing uses the probe PID.

The reusable `paxile_policy::BackendControl` API accepts `ControlCommand::{On,Off,Toggle,Status}` through `execute` and exposes `ControlStatus`. Both console and external controls use this state. `authorized` is immutable startup permission; ordinary watch and `--spy` reject enabling even if configuration requests it. `requested_enabled` and monotonic `revision` describe intent, **not successful HIMC restoration**. `applied_enabled:null` deliberately means unknown: the prototype does not aggregate acknowledgements from every target UI thread. `control_received` records acknowledge DLL worker receipt only, not owner-thread application. A disconnected DLL or hung UI can therefore still have pending work.

The duplex heartbeat is now a 9-byte frame: command 0=stop, 1=enabled, 2=disabled followed by little-endian u64 revision. A valid disabled frame still renews the 750 ms connection lease. The DLL separately latches startup authorization, so an enable frame cannot upgrade read-only configuration. Every revision transition clears thread-local candidate evidence and restores any saved HIMC **on the window owner thread**, at the next hook callback or the existing 100 ms watchdog. Rapid off/on changes are safe even if a UI thread misses the intermediate disabled frame: the newer revision forces restoration and evidence reset before a new decision. Heartbeat delivery adds roughly 100 ms plus the 20 ms worker polling interval; timer restoration adds up to roughly 100 ms on a responsive message-pumping thread. A hung thread cannot restore until it resumes. No control worker calls IMM mutation APIs.

Automated coverage includes actual external CLI subprocesses against a live local control server without stdin, read-only authorization rejection, off/on revisions over a real pipe and actual built DLL without reconnecting, and real hidden-window HIMC detach → runtime disable/restore → enable/detach → lease-expiry/restore. These are backend tests, not a new live CSP/IME acceptance run.

`crates/policy` is a reusable platform-independent library exposing `Config`, `Preset`, `Observation`, `Decision`, and `CandidateEvidence`. Future GUI/server clients can use it directly. No HTTP server or GUI is included. `Config::validate` rejects unknown profiles, duplicate preset names, and unrestricted class patterns. Unknown JSON fields are rejected.

`backend.example.json` shows the complete format. A preset restricts executable basenames and class patterns (exact match or trailing `*` prefix). Custom presets replace the built-in preset list when the `presets` property is supplied. They use the same experimental IMM signal adapter; a different application protocol requires a new platform adapter, not merely a class pattern. Photoshop has no verified adapter and fails open by default. Whitelisting Photoshop is supported.

## Signal adapter and lifecycle

The CSP adapter uses a same-thread, same-focused-window candidate-position notification (`WM_IME_NOTIFY`, wParam 9) seen up to 100 ms before a queued `WM_IME_STARTCOMPOSITION`. Evidence is cleared on click/focus/deactivation transitions. This accommodates the observed 0.976–5.203 ms candidate-before-start ordering. A posted start is evaluated after the rest of WH_GETMESSAGE; sent starts fail open. No IME messages are swallowed or rewritten.

For an eligible non-text hypothesis, the hook cancels composition with `ImmNotifyIME(NI_COMPOSITIONSTR, CPS_CANCEL)` and detaches with `ImmAssociateContext(hwnd, NULL)` on that window's owner thread. Cancellation failure fails open. The original HIMC is saved and restored on click, focus, deactivation, language change, destruction, delayed candidate evidence, or expired connection lease. The owner-thread timer additionally checks focus, foreground process, and caret changes. It cannot run while the target UI thread is hung; it restores when that thread pumps messages again. The DLL is pinned for process lifetime once a timer is registered, avoiding callback code unloading after hooks are removed.

The local-only duplex pipe checks the connecting client's OS-reported PID against selected targets before sending a bounded JSON config frame. Heartbeats renew a 750 ms mutation lease; stop sends a zero-command frame, and the probe allows 1.2 seconds for cleanup before unhooking. Abnormal disconnect is fail-open when the owner next pumps messages. The DLL worker now survives controller shutdown and retries the pipe every 500 ms indefinitely (100 ms cooldown after a rejected/disconnected session); an initially absent controller may appear later than three seconds. Each connection validates a fresh bounded configuration handshake (two-second read deadlines) and latches that connection's startup authorization. Heartbeats and rule reloads cannot upgrade a read-only connection. Disconnect clears configuration, lease, control and queued records and advances a separate session generation: even if a new controller reuses revision zero or an identical control word, owner threads restore saved HIMCs and clear candidate evidence before evaluating the new session. The worker retains one module reference, not one per reconnect; timer callbacks still pin the DLL for process lifetime. This is not an authenticated multi-user service: the fixed pipe name permits only one controller session reliably, and a malicious local pipe server is outside this prototype's security scope. A matching rebuilt DLL reconnects without restarting the target; an **older DLL already pinned in a target cannot be hot-replaced**, and that target still needs a user-managed restart once to load this fix. Rebuild the EXE and DLL together.

## Known limitations and manual acceptance

This is a backend prototype, not verified CSP compatibility. The classification heuristic is supported by captured traces, but live composition cancellation, shortcut behavior, and application-specific text transitions remain unverified. Text detection is conservative, yet CSP custom controls without caret/candidate evidence may still be misclassified. JSON presets are an explicit experimental override.

**First-key replay is deliberately absent.** The key that triggered composition may be lost; later keys can pass while the context is detached. Neither replay nor production-ready shortcut preservation is claimed. The original posted start still dispatches after cancellation; an application's handling may create additional state, requiring manual validation.

Manual CSP acceptance: enable a Chinese IME, press canvas shortcuts (B/T/Q/G), click the text tool and type Chinese, edit standard dialogs, switch focus/apps/input languages, then stop the probe and confirm IME restoration. Also terminate the probe abnormally and confirm restoration after the lease expires. Capture backend `decision`, `detached`, and `restored` JSONL records. Sort traces by QPC rather than arrival order. Validate both allowlisted and unknown apps cause zero mutations. No automated test claims these live application outcomes.
