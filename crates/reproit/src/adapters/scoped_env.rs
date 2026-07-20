//! Scope-bound process environment overrides.

/// Restores every environment variable to its prior value on drop.
///
/// Environment mutation is process-global, so callers must not run scopes
/// concurrently. The guard makes sequential target execution deterministic,
/// including early-return paths.
pub(crate) struct ScopedEnv {
    prior: Vec<(String, Option<String>)>,
}

impl ScopedEnv {
    pub(crate) fn set(vars: Vec<(String, String)>) -> Self {
        let mut prior = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            prior.push((key.clone(), std::env::var(&key).ok()));
            std::env::set_var(&key, value);
        }
        Self { prior }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, previous) in self.prior.drain(..) {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_set_and_unset_values() {
        const SET: &str = "REPROIT_SCOPED_ENV_SET_TEST";
        const UNSET: &str = "REPROIT_SCOPED_ENV_UNSET_TEST";
        std::env::set_var(SET, "before");
        std::env::remove_var(UNSET);
        {
            let _scope = ScopedEnv::set(vec![
                (SET.to_string(), "during".to_string()),
                (UNSET.to_string(), "during".to_string()),
            ]);
            assert_eq!(std::env::var(SET).as_deref(), Ok("during"));
            assert_eq!(std::env::var(UNSET).as_deref(), Ok("during"));
        }
        assert_eq!(std::env::var(SET).as_deref(), Ok("before"));
        assert!(std::env::var(UNSET).is_err());
        std::env::remove_var(SET);
    }
}
