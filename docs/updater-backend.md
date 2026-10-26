# Updater backend integration

Backend uses official `tauri-plugin-updater` (resolved 2.14.0), never executes frontend-supplied paths/URLs. Default plugin SemVer comparison accepts only newer versions. Only HTTPS artifacts under `https://github.com/KamiyaShiroba/Paxile/releases/download/` are accepted; plugin verifies artifact signature before stopping core, and verifies again on install.

## IPC (frontend integration)
- `check_update` -> `{available: boolean, version: string|null, body: string|null, current_version: string, update_id: string|null}`. Notes are untrusted text: render as text, not raw HTML. Checks serialize with installs; concurrent calls reject. Check failures invalidate previous pending handles. Startup invokes a background check only (never downloads/installs).
- `install_update` args `{updateId: string, confirmed: true, onEvent: Channel}`. Only settings WebView can invoke installation; `updateId` must match latest successful check. Handle is consumed once; failure requires checking again. No URL/version/key/path accepted from frontend.
- Channel messages `{event: 'started', data: {content_length: number|null}}`, `{event: 'progress', data: {chunk_length: number}}`, `{event: 'finished'}`. Finished means download stream finished, not installation complete; signature verification may still fail. Display install state after command transition, not from Finished alone.
- Runtime logs contain actual startup check results, download start and verified installer launch/errors. Installer launch failure leaves core stopped and logs that Paxile restart is needed. No installer is launched by tests.

## Required packaging config (parent-owned tauri.conf.json)
- `bundle.createUpdaterArtifacts: true`
- `bundle.targets: ["nsis"]`
- `bundle.windows.nsis.installMode: "currentUser"` (quiet updater cannot self-elevate)
- `plugins.updater.pubkey`: actual generated public-key CONTENT; never path/private key
- `plugins.updater.endpoints: ["https://github.com/KamiyaShiroba/Paxile/releases/latest/download/latest.json"]`
- `plugins.updater.windows.installMode: "quiet"`
- Recommended newest plugin hardening: `plugins.updater.requireSignedVersion: true`; CI must use a current CLI signing version-bound artifacts. Verify CLI compatibility before enabling.
- Do NOT grant `updater:default` or other updater JS permissions: frontend should only access guarded app commands, not plugin installation commands.
- Publish NSIS installer, its actual `.sig`, and generated `latest.json` with `notes` and `windows-x86_64` (or NSIS-specific) signed artifact entry. Signing secret belongs only in local secure storage/GitHub Actions secrets.

Validation: Rust all-target tests and clippy with `-D warnings` passed. Real release/signature/install end-to-end is intentionally not exercised on the user's machine. Config/key/CI/frontend are parent-owned integration work.
