//! Dim's identity on its model requests: the User-Agent, `X-Title` and
//! `HTTP-Referer` every request carries unless its provider names its own.
//!
//! The headers go in the provider layer of [`crate::driver`], so the library's
//! own User-Agent parts follow them, and a provider's per-call header still
//! overrides them.
//!
//! The override belongs to the Aimux part that serves one App Server, not to
//! the process: two embedded App Servers, each with its own product home, hold
//! their own, and one closing leaves the other's in place.
use aimux_provider_utils::HeaderMapOpt;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

/// The title dim sends with its requests.
const X_TITLE: &str = "DimCode";
/// The attribution URL dim sends with its requests.
const REFERER: &str = "https://dimagent.com/";
/// The variable a host process reads once and passes to its model hosts.
pub const ENVIRONMENT_VARIABLE: &str = "DIM_USER_AGENT";
/// The longest override [`checked_user_agent`] accepts.
const MAX_USER_AGENT: usize = 512;

/// `dim/<version> DimAgent/<version>`: `dim` first, so a gateway that reads
/// the first token names dim, then the older relay identity.
fn default_user_agent() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("dim/{version} DimAgent/{version}")
}

/// `value` when it is an override a request can carry: 1 to 512 printable
/// ASCII characters.
fn checked_user_agent(value: &str) -> Option<String> {
    ((1..=MAX_USER_AGENT).contains(&value.len())
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)))
    .then(|| value.to_owned())
}

/// A value a request cannot carry, with the message the startup inputs use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidUserAgent;

impl std::fmt::Display for InvalidUserAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DIM_USER_AGENT must be 1 to 512 printable ASCII characters")
    }
}

impl std::error::Error for InvalidUserAgent {}

/// The overrides in effect for one model host, highest scope first.
#[derive(Default)]
struct Identity {
    /// [`ENVIRONMENT_VARIABLE`]'s value, as the host process read it.
    environment: Option<String>,
    /// The `provider` settings source's `userAgent`.
    configured: Option<String>,
}

impl Identity {
    /// The User-Agent every model request carries: the environment's
    /// override, then the configured one, then [`default_user_agent`].
    fn user_agent(&self) -> String {
        self.environment
            .clone()
            .or_else(|| self.configured.clone())
            .unwrap_or_else(default_user_agent)
    }
}

/// The identity one Aimux part serves: the value its own model requests carry,
/// shared with the `provider` settings row that writes the configured
/// override.
///
/// Cloning shares the state, so a row holds the same one the driver reads.
#[derive(Clone)]
pub struct ModelIdentity(Arc<RwLock<Identity>>);

impl Default for ModelIdentity {
    /// The built-in identity: no override from the environment or a setting.
    fn default() -> Self {
        Self::new(None)
    }
}

impl ModelIdentity {
    /// The identity with `environment`'s override, if the host process set
    /// one.
    pub fn new(environment: Option<String>) -> Self {
        Self(Arc::new(RwLock::new(Identity {
            environment,
            configured: None,
        })))
    }

    /// The User-Agent every model request through this part carries.
    pub fn user_agent(&self) -> String {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .user_agent()
    }

    /// The three headers this part adds at the provider layer.
    pub(crate) fn headers(&self) -> HeaderMapOpt {
        HashMap::from([
            ("user-agent".into(), Some(self.user_agent())),
            ("x-title".into(), Some(X_TITLE.into())),
            ("http-referer".into(), Some(REFERER.into())),
        ])
    }

    /// Publishes the configured override: the `provider` settings source's
    /// `userAgent`. The environment's value still wins over it.
    pub fn set_configured(&self, user_agent: Option<String>) {
        self.0
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .configured = user_agent;
    }
}

impl std::fmt::Debug for ModelIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelIdentity")
            .field("user_agent", &self.user_agent())
            .finish()
    }
}

/// [`ENVIRONMENT_VARIABLE`]'s value as a host process reads it: `None` when it
/// is unset, and a value a request cannot carry is refused before the process
/// serves anything.
pub fn environment_user_agent(
    variable: Option<String>,
) -> Result<Option<String>, InvalidUserAgent> {
    variable
        .map(|value| checked_user_agent(&value).ok_or(InvalidUserAgent))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_override_is_refused_when_a_request_cannot_carry_it() {
        assert_eq!(environment_user_agent(None), Ok(None));
        assert_eq!(
            environment_user_agent(Some("private-client/7".into())).unwrap(),
            Some("private-client/7".into())
        );
        // A space is printable ASCII, so a value may carry one; a control
        // character, a byte outside ASCII and an empty value may not.
        for refused in ["", "dim\u{e9}", "a\nb", "a\u{7f}b"] {
            let error = environment_user_agent(Some(refused.into())).unwrap_err();
            assert!(
                !error.to_string().contains(refused) || refused.is_empty(),
                "{refused:?}: the refusal names no value"
            );
        }
    }

    #[test]
    fn an_override_is_one_to_512_printable_ascii_characters() {
        assert_eq!(checked_user_agent("dim/1.0").as_deref(), Some("dim/1.0"));
        assert!(
            checked_user_agent(&"a".repeat(MAX_USER_AGENT)).is_some(),
            "the longest accepted value is accepted"
        );
        // A space is printable ASCII and a value may carry one; an empty
        // value, a control character and a byte outside ASCII may not.
        for refused in ["", "a\nb", "a\u{7f}b", "dím/1.0"] {
            assert_eq!(checked_user_agent(refused), None, "{refused:?}");
        }
        assert_eq!(
            checked_user_agent(&"a".repeat(MAX_USER_AGENT + 1)),
            None,
            "a value one over the limit is refused, not trimmed"
        );
    }

    #[test]
    fn the_environment_override_wins_over_the_configured_one() {
        let identity = ModelIdentity::new(Some("private-client/7".into()));
        identity.set_configured(Some("gateway-client/9".into()));
        assert_eq!(identity.user_agent(), "private-client/7");
        assert_eq!(identity.headers()["x-title"].as_deref(), Some("DimCode"));

        identity.set_configured(None);
        assert_eq!(identity.user_agent(), "private-client/7");

        // Two parts of one process hold their own override.
        let other = ModelIdentity::new(None);
        other.set_configured(Some("first-client/1".into()));
        assert_eq!(other.user_agent(), "first-client/1");
        assert_eq!(identity.user_agent(), "private-client/7");
        other.set_configured(None);
        assert_eq!(other.user_agent(), default_user_agent());
        assert_eq!(identity.user_agent(), "private-client/7");
    }
}
