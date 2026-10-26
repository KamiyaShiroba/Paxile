//! One-shot confirmation binding, independent of networking and installers.

pub struct Pending<T> {
    next_id: u64,
    value: Option<(String, T)>,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self {
            next_id: 0,
            value: None,
        }
    }
}

impl<T> Pending<T> {
    pub fn clear(&mut self) {
        self.value = None;
    }

    pub fn replace(&mut self, update: T) -> String {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.value = Some((id.clone(), update));
        id
    }

    pub fn take(&mut self, id: &str, confirmed: bool) -> Result<T, String> {
        if !confirmed {
            return Err("Explicit update confirmation is required".into());
        }
        let Some((expected, _)) = self.value.as_ref() else {
            return Err("No pending update; check again".into());
        };
        if id != expected {
            return Err("Update confirmation is stale; check again".into());
        }
        Ok(self.value.take().expect("checked pending value").1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_is_required_and_does_not_consume_pending_update() {
        let mut pending = Pending::default();
        let id = pending.replace(7);
        assert!(pending.take(&id, false).is_err());
        assert_eq!(pending.take(&id, true), Ok(7));
        assert!(pending.take(&id, true).is_err());
    }

    #[test]
    fn stale_id_is_rejected_without_consuming_new_update() {
        let mut pending = Pending::default();
        let old = pending.replace(1);
        let new = pending.replace(2);
        assert!(pending.take(&old, true).is_err());
        assert_eq!(pending.take(&new, true), Ok(2));
    }

    #[test]
    fn failed_recheck_clears_the_previous_offer() {
        let mut pending = Pending::default();
        let id = pending.replace(1);
        pending.clear();
        assert!(pending.take(&id, true).is_err());
    }

    #[test]
    fn install_without_a_successful_check_is_rejected() {
        let mut pending = Pending::<()>::default();
        assert_eq!(
            pending.take("1", true),
            Err("No pending update; check again".into())
        );
    }
}
