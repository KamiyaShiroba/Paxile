use paxile_policy::Config;

pub fn commit(
    previous: &Config,
    next: &Config,
    mut apply: impl FnMut(&Config) -> Result<(), String>,
    mut persist: impl FnMut(&Config) -> Result<(), String>,
) -> Result<(), String> {
    next.validate()?;
    if let Err(error) = apply(next) {
        return Err(match apply(previous) {
            Ok(()) => format!("{error}; previous runtime request restored"),
            Err(rollback) => {
                format!("{error}; ROLLBACK FAILED: {rollback}; runtime state is unknown")
            }
        });
    }
    if let Err(error) = persist(next) {
        return Err(match apply(previous) {
            Ok(()) => format!("{error}; previous runtime request restored"),
            Err(rollback) => {
                format!("{error}; ROLLBACK FAILED: {rollback}; runtime state is unknown")
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn persistence_failure_restores_runtime_and_exposes_failed_rollback() {
        let previous = Config::default();
        let next = Config {
            enabled: true,
            ..previous.clone()
        };
        let seen = RefCell::new(Vec::new());
        let error = commit(
            &previous,
            &next,
            |c| {
                seen.borrow_mut().push(c.enabled);
                if c.enabled {
                    Ok(())
                } else {
                    Err("pipe gone".into())
                }
            },
            |_| Err("disk denied".into()),
        )
        .unwrap_err();
        assert_eq!(*seen.borrow(), [true, false]);
        assert!(error.contains("disk denied"));
        assert!(error.contains("ROLLBACK FAILED"));
        assert!(error.contains("unknown"));
    }

    #[test]
    fn persistence_failure_restores_previous_rules_and_switch() {
        let previous = Config::default();
        let next = Config {
            enabled: true,
            allow_exe: vec!["new.exe".into()],
            ..previous.clone()
        };
        let seen = RefCell::new(Vec::new());
        let error = commit(
            &previous,
            &next,
            |c| {
                seen.borrow_mut().push((c.enabled, c.allow_exe.clone()));
                Ok(())
            },
            |_| Err("rename denied".into()),
        )
        .unwrap_err();
        assert_eq!(
            *seen.borrow(),
            [(true, vec!["new.exe".to_string()]), (false, vec![])]
        );
        assert!(error.contains("previous runtime request restored"));
        assert!(!error.contains("ROLLBACK FAILED"));
    }

    #[test]
    fn invalid_config_never_reaches_control_or_persistence() {
        let previous = Config::default();
        let next = Config {
            profile: "missing".into(),
            ..previous.clone()
        };
        assert!(commit(
            &previous,
            &next,
            |_| panic!("control"),
            |_| panic!("persist")
        )
        .is_err());
    }

    #[test]
    fn successful_request_is_persisted_only_after_control() {
        let calls = RefCell::new(Vec::new());
        let config = Config::default();
        commit(
            &config,
            &config,
            |_| {
                calls.borrow_mut().push("apply");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("persist");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*calls.borrow(), ["apply", "persist"]);
    }

    #[test]
    fn failed_control_is_compensated_without_persisting() {
        let previous = Config::default();
        let next = Config {
            enabled: true,
            ..previous.clone()
        };
        let seen = RefCell::new(Vec::new());
        let result = commit(
            &previous,
            &next,
            |c| {
                seen.borrow_mut().push(c.enabled);
                if c.enabled {
                    Err("lost reply".into())
                } else {
                    Ok(())
                }
            },
            |_| panic!("must not persist rejected control"),
        );
        assert!(result.unwrap_err().contains("lost reply"));
        assert_eq!(*seen.borrow(), [true, false]);
    }
}
