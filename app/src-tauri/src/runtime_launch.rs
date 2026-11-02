//! Probe launch construction, kept independent of Tauri for non-launching tests.
use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, Stdio},
};

pub(super) fn command(
    probe: &Path,
    config: &Path,
    debug_log: Option<&OsStr>,
) -> Result<Command, String> {
    let enabled = debug_log == Some(OsStr::new("1"));
    // Resolve before changing cwd: relative EXE/config paths otherwise point into diagnostics.
    let probe = if enabled {
        std::path::absolute(probe).map_err(|e| e.to_string())?
    } else {
        probe.to_path_buf()
    };
    let config = if enabled {
        std::path::absolute(config).map_err(|e| e.to_string())?
    } else {
        config.to_path_buf()
    };
    let mut command = Command::new(probe);
    command.args(["watch", "--managed", "--spy", "--block"]);
    if enabled {
        let directory = config.parent().ok_or("配置目录不可用")?.join("diagnostics");
        std::fs::create_dir_all(&directory)
            .map_err(|e| format!("无法创建诊断日志目录 {}：{e}", directory.display()))?;
        command.current_dir(directory);
    } else {
        command.arg("--no-file");
    }
    command
        .arg("--config")
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_working_directory_is_created_beside_config() {
        let root =
            std::env::temp_dir().join(format!("paxile-launch-path-test-{}", std::process::id()));
        let config = root.join("config.json");
        let command = command(Path::new("probe.exe"), &config, Some(OsStr::new("1"))).unwrap();
        assert_eq!(
            command.get_current_dir(),
            Some(root.join("diagnostics").as_path())
        );
        assert!(root.join("diagnostics").is_dir());
        assert_eq!(
            command.get_program(),
            std::path::absolute("probe.exe").unwrap()
        );
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.last().unwrap(), &config.as_os_str());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_empty_zero_and_other_values_keep_no_file() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some(" 1"),
            Some("1 "),
        ] {
            let command = command(
                Path::new("probe.exe"),
                Path::new("config.json"),
                value.map(OsStr::new),
            )
            .unwrap();
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(
                args,
                [
                    "watch",
                    "--managed",
                    "--spy",
                    "--block",
                    "--no-file",
                    "--config",
                    "config.json"
                ]
            );
            assert!(command.get_current_dir().is_none());
        }
    }

    #[test]
    fn explicit_one_enables_file_logging() {
        let root =
            std::env::temp_dir().join(format!("paxile-launch-opt-in-test-{}", std::process::id()));
        let command = command(
            Path::new("probe.exe"),
            &root.join("config.json"),
            Some(OsStr::new("1")),
        )
        .unwrap();
        assert!(
            !command.get_args().any(|arg| arg == "--no-file"),
            "PAXILE_DEBUG_LOG=1 must opt into file logging"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
