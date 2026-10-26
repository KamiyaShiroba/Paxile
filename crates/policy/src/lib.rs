//! Platform-independent backend configuration and fail-open decisions.
use serde::{Deserialize, Serialize};

pub const CSP_CLASS: &str = "742DEA58-ED6B-4402-BC11-20DFC6D08040";

/// Runtime intent, independent of hook transport and UI-thread application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlCommand {
    On,
    Off,
    Toggle,
    Status,
}
impl std::str::FromStr for ControlCommand {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            "toggle" => Ok(Self::Toggle),
            "status" => Ok(Self::Status),
            _ => Err("expected on|off|toggle|status".into()),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ControlStatus {
    pub authorized: bool,
    pub requested_enabled: bool,
    pub revision: u64,
    /// No global UI-thread acknowledgement is claimed by this prototype.
    pub applied_enabled: Option<bool>,
}
pub struct BackendControl {
    status: ControlStatus,
    policy: Config,
}
impl BackendControl {
    pub fn new(authorized: bool) -> Self {
        Self {
            status: ControlStatus {
                authorized,
                requested_enabled: authorized,
                revision: 0,
                applied_enabled: None,
            },
            policy: Config::default(),
        }
    }
    pub fn status(&self) -> ControlStatus {
        self.status
    }
    pub fn initialize(&mut self, mut policy: Config) {
        policy.enabled = self.status.authorized;
        self.policy = policy;
    }
    pub fn policy(&self) -> Config {
        self.policy.clone()
    }
    pub fn reload(&mut self, mut policy: Config) -> Result<(), String> {
        policy.validate()?;
        policy.enabled = self.status.authorized;
        self.policy = policy;
        self.status.revision += 1;
        Ok(())
    }
    pub fn execute(&mut self, command: ControlCommand) -> Result<ControlStatus, String> {
        let enabled = match command {
            ControlCommand::On => true,
            ControlCommand::Off => false,
            ControlCommand::Toggle => !self.status.requested_enabled,
            ControlCommand::Status => return Ok(self.status),
        };
        if enabled && !self.status.authorized {
            return Err("mutation requires watch --block at startup".into());
        }
        if enabled != self.status.requested_enabled {
            self.status.revision += 1;
            self.status.requested_enabled = enabled;
        }
        Ok(self.status)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub profile: String,
    pub allow_exe: Vec<String>,
    pub presets: Vec<Preset>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: "auto".into(),
            allow_exe: vec![],
            presets: vec![Preset {
                name: "csp-experimental".into(),
                executables: vec!["CLIPStudioPaint.exe".into()],
                non_text_classes: vec![format!("{CSP_CLASS}*")],
            }],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    pub executables: Vec<String>,
    /// Exact class or prefix followed by '*'. Only use with an app adapter.
    pub non_text_classes: Vec<String>,
}
#[derive(Default, Debug)]
pub struct Observation {
    pub class: String,
    pub caret: bool,
    pub candidate: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.profile != "auto" && !self.presets.iter().any(|p| p.name == self.profile) {
            return Err(format!("unknown profile: {}", self.profile));
        }
        let mut names = std::collections::HashSet::new();
        for p in &self.presets {
            if p.name.is_empty() || p.name == "auto" || !names.insert(&p.name) {
                return Err("preset names must be unique, nonempty, and not auto".into());
            }
            if p.executables.is_empty()
                || p.non_text_classes.iter().any(|c| c.is_empty() || c == "*")
            {
                return Err("presets require executables and specific class patterns".into());
            }
        }
        Ok(())
    }
    pub fn decide(&self, exe: &str, o: &Observation) -> Decision {
        let class = o.class.to_ascii_lowercase();
        if !self.enabled
            || self.allow_exe.iter().any(|a| a.eq_ignore_ascii_case(exe))
            || o.caret
            || o.candidate
            || class == "edit"
            || class.starts_with("richedit")
        {
            return Decision::Allow;
        }
        for p in &self.presets {
            if (self.profile == "auto" || self.profile == p.name)
                && p.executables.iter().any(|e| e.eq_ignore_ascii_case(exe))
                && p.non_text_classes.iter().any(|c| {
                    c.strip_suffix('*')
                        .map_or(o.class == *c, |prefix| o.class.starts_with(prefix))
                })
            {
                return Decision::Block;
            }
        }
        Decision::Allow
    }
}

/// Per-thread candidate evidence. It expires and cannot cross a focus/window boundary.
#[derive(Default, Debug)]
pub struct CandidateEvidence {
    hwnd: usize,
    at_ms: u64,
}
impl CandidateEvidence {
    pub fn record(&mut self, hwnd: usize, now_ms: u64) {
        self.hwnd = hwnd;
        self.at_ms = now_ms;
    }
    pub fn recent(&self, hwnd: usize, now_ms: u64) -> bool {
        hwnd != 0 && self.hwnd == hwnd && now_ms.saturating_sub(self.at_ms) <= 100
    }
    pub fn clear(&mut self) {
        self.hwnd = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_replaces_rules_without_granting_authorization() {
        let mut control = BackendControl::new(false);
        let config = Config {
            enabled: true,
            allow_exe: vec!["safe.exe".into()],
            ..Default::default()
        };
        control.reload(config).unwrap();
        assert!(!control.status().requested_enabled);
        assert_eq!(control.policy().allow_exe, ["safe.exe"]);
        assert_eq!(control.status().revision, 1);
        assert!(control.execute(ControlCommand::On).is_err());
    }
    #[test]
    fn runtime_control_requires_startup_authorization_and_tracks_revisions() {
        let mut readonly = BackendControl::new(false);
        assert!(readonly.execute(ControlCommand::On).is_err());
        assert!(readonly.execute(ControlCommand::Toggle).is_err());
        assert!(
            !readonly
                .execute(ControlCommand::Status)
                .unwrap()
                .requested_enabled
        );
        let mut control = BackendControl::new(true);
        let off = control.execute(ControlCommand::Off).unwrap();
        assert!(!off.requested_enabled);
        assert_eq!(off.revision, 1);
        let on = control.execute(ControlCommand::Toggle).unwrap();
        assert!(on.requested_enabled);
        assert_eq!(on.revision, 2);
        assert_eq!(control.execute(ControlCommand::Status).unwrap().revision, 2);
        assert!("enable".parse::<ControlCommand>().is_err());
    }
    #[test]
    fn read_only_and_whitelist_never_block() {
        let mut c = Config::default();
        let o = Observation {
            class: CSP_CLASS.into(),
            ..Default::default()
        };
        assert_eq!(c.decide("CLIPStudioPaint.exe", &o), Decision::Allow);
        c.enabled = true;
        assert_eq!(c.decide("CLIPStudioPaint.exe", &o), Decision::Block);
        c.allow_exe.push("clipstudiopaint.EXE".into());
        assert_eq!(c.decide("CLIPStudioPaint.exe", &o), Decision::Allow);
    }

    #[test]
    fn text_signals_and_unknown_software_fail_open() {
        let c = Config {
            enabled: true,
            ..Default::default()
        };
        for class in ["Edit", "RichEdit20W", "RICHEDIT50W", "UnrecognizedWidget"] {
            assert_eq!(
                c.decide(
                    "CLIPStudioPaint.exe",
                    &Observation {
                        class: class.into(),
                        ..Default::default()
                    }
                ),
                Decision::Allow
            );
        }
        for (caret, candidate) in [(true, false), (false, true)] {
            assert_eq!(
                c.decide(
                    "CLIPStudioPaint.exe",
                    &Observation {
                        class: CSP_CLASS.into(),
                        caret,
                        candidate
                    }
                ),
                Decision::Allow
            );
        }
        assert_eq!(
            c.decide(
                "Photoshop.exe",
                &Observation {
                    class: CSP_CLASS.into(),
                    ..Default::default()
                }
            ),
            Decision::Allow
        );
    }
    #[test]
    fn candidate_is_scoped_to_window_and_100ms() {
        let mut e = CandidateEvidence::default();
        e.record(42, 1000);
        assert!(e.recent(42, 1005));
        assert!(e.recent(42, 1100));
        assert!(!e.recent(42, 1101));
        assert!(!e.recent(43, 1005));
        e.clear();
        assert!(!e.recent(42, 1005));
    }
    #[test]
    fn rejects_ambiguous_config() {
        let mut c = Config {
            profile: "photoshop".into(),
            ..Default::default()
        };
        assert!(c.validate().is_err());
        assert!(serde_json::from_str::<Config>(r#"{"enabeld":true}"#).is_err());
        c.profile = "auto".into();
        c.presets[0].non_text_classes = vec!["*".into()];
        assert!(c.validate().is_err());
    }
}
