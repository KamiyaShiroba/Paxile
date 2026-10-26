//! paxile-probe: read-only observer for figuring out when a drawing app
//! (default: CLIP STUDIO PAINT) is "typing text" vs "taking shortcuts".
//!
//!   paxile-probe snapshot [--children]   dump the target's window forest
//!   paxile-probe [watch] [--keys] [--all] stream focus/caret/IME/window events

mod control;
mod events;
mod log;

mod snapshot;
mod spy;
mod watch;
mod win;

use std::process::ExitCode;

const USAGE: &str = "\
usage: paxile-probe [watch|snapshot] [options]
       paxile-probe control on|off|toggle|status --session PROBE_PID

  watch (default)    stream window/focus/caret/IME events of the target
  snapshot           print every window the target owns, grouped by owner

options:
  --exe NAME         target executable (default CLIPStudioPaint.exe)
  --pid N            target a specific pid instead of --exe
  --wait             wait for --exe to appear instead of exiting when it is not running
  --keys             also log key down/up while the target is foreground
  --spy              load paxile_spy.dll into the target's UI threads (read-only
                     message hooks: IME / focus / key messages, IME context state)
  --block            authorize runtime mutation, initially enabled
                     console: /block on|off|toggle|status
  --profile NAME     blocking profile (default: auto)
  --allow-exe NAME   whitelist executable (repeatable)
  --config PATH      JSON backend presets and whitelist
  --all              also log noisy events (state/name/value/selection)
  --poll MS          GUI/IME state poll interval, default 50
  --children         snapshot: include child HWND trees
  --no-file          don't write logs/*.jsonl
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let mut cmd = "watch".to_string();
    if let Some(a) = args.peek() {
        if !a.starts_with('-') {
            cmd = args.next().unwrap();
        }
    }

    if cmd == "control" {
        return control::cli(&args.collect::<Vec<_>>());
    }
    let mut exe = "CLIPStudioPaint.exe".to_string();
    let mut pid: Option<u32> = None;
    let (mut wait, mut keys, mut spy, mut all, mut children, mut to_file, mut block) =
        (false, false, false, false, false, true, false);
    let mut managed = false;
    let mut profile = "auto".to_string();
    let mut allow_exe = Vec::new();
    let mut config = None;
    let mut poll_ms = 50u32;

    while let Some(a) = args.next() {
        let mut val = |name: &str| {
            args.next().unwrap_or_else(|| {
                eprintln!("{name} needs a value\n\n{USAGE}");
                std::process::exit(2)
            })
        };
        match a.as_str() {
            "--exe" => exe = val("--exe"),
            "--pid" => pid = val("--pid").parse().ok(),
            "--wait" => wait = true,
            "--poll" => poll_ms = val("--poll").parse().unwrap_or(50).max(10),
            "--keys" => keys = true,
            "--spy" => spy = true,
            "--block" => block = true,
            "--managed" => managed = true,
            "--profile" => profile = val("--profile"),
            "--allow-exe" => allow_exe.push(val("--allow-exe")),
            "--config" => config = Some(val("--config")),
            "--all" => all = true,
            "--children" => children = true,
            "--no-file" => to_file = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let mut policy: paxile_policy::Config = match config {
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("invalid config: {e}");
                return ExitCode::from(2);
            }
        },
        None => paxile_policy::Config::default(),
    };
    // A config file can never opt into mutation: the command line must do so.
    policy.enabled = block && (!managed || policy.enabled);
    if profile != "auto" {
        policy.profile = profile;
    }
    policy.allow_exe.extend(allow_exe);
    if let Err(e) = policy.validate() {
        eprintln!("invalid policy: {e}");
        return ExitCode::from(2);
    }

    let pids = match pid {
        Some(p) => vec![p],
        None if managed => vec![],
        None => win::pids_by_exe(&exe),
    };

    let log = match log::Logger::new(&cmd, to_file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("can't open log file: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cmd.as_str() {
        "snapshot" => {
            if pids.is_empty() {
                eprintln!("{exe} is not running");
                return ExitCode::FAILURE;
            }
            let mut log = log;
            snapshot::run(&pids, children, &mut log);
            log.flush();
            if let Some(p) = &log.path {
                println!("jsonl -> {}", p.display());
            }
        }
        "watch" => watch::run(
            watch::Opts {
                exe,
                managed,
                authorized: block,
                wait,
                all,
                keys,
                spy,
                poll_ms,
                policy,
            },
            pids,
            log,
        ),
        other => {
            eprintln!("unknown command {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
