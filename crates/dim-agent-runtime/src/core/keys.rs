use crate::{AgentError, Result};

/// Event keys a subscription or `before` hook applies to: an exact key,
/// `*` for every key, or `domain/*` for every key under `domain/`.
#[derive(Clone)]
pub(crate) struct KeyFilter(Vec<String>);
impl KeyFilter {
    pub(crate) fn new(keys: &[&str]) -> Result<Self> {
        for key in keys {
            let wildcard = key.find('*');
            if wildcard
                .is_some_and(|at| at + 1 != key.len() || (*key != "*" && !key.ends_with("/*")))
            {
                return Err(AgentError::InvalidInput(format!(
                    "event key {key:?} must be exact, `*` or `domain/*`"
                )));
            }
        }
        Ok(Self(keys.iter().map(|k| k.to_string()).collect()))
    }
    pub(crate) fn matches(&self, key: &str) -> bool {
        self.0.iter().any(|filter| match filter.strip_suffix('*') {
            Some(prefix) => key.starts_with(prefix),
            None => filter == key,
        })
    }
}
