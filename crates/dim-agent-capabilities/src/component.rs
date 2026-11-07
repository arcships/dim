//! The components RuntimeManager installs and the checks their owners run on
//! them. A component is a directory of libraries and models that the helper of
//! its owning plugin reads — the OCR bundle, say — never a command some process
//! runs. RuntimeManager owns the installation and answers [`ComponentSource`]
//! with what it installed; the component's owner answers [`ComponentVerifier`]
//! for its name, so an install activates nothing this build cannot check.
//!
//! Both are ports: RuntimeManager provides the source and reads the verifier
//! optionally, so neither plugin depends on the other, and a reader that finds
//! no port reports the component as absent or the install as unverifiable
//! rather than failing to load.
use rutis::host::BoxFuture;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What RuntimeManager knows about one component kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentState {
    /// Not installed: nothing to load.
    Off,
    /// An acquisition is in flight; nothing is active until it ends.
    Acquiring,
    /// The acquisition or its verification failed, with the reason to show.
    Failed(String),
    /// Installed and verified: `root` is the directory the helper reads, and
    /// `version` is the component version the installation records.
    Ready { root: PathBuf, version: String },
}

/// The components RuntimeManager installed. Read at each use, so an install
/// reaches the next call without a reload.
pub trait ComponentSource: Send + Sync + 'static {
    /// The state of the component `name`, or `None` when this build has no
    /// component of that kind.
    fn component(&self, name: &str) -> Option<ComponentState>;
}

/// The [`ComponentSource`] a plugin reads from its context.
pub struct ComponentSourceHost(pub Arc<dyn ComponentSource>);

impl ComponentSourceHost {
    /// The same states however often it is read, for an assembly that installs
    /// no components: a test, or an evaluation.
    pub fn fixed(components: Vec<(&'static str, ComponentState)>) -> Self {
        struct Fixed(Vec<(String, ComponentState)>);
        impl ComponentSource for Fixed {
            fn component(&self, name: &str) -> Option<ComponentState> {
                self.0
                    .iter()
                    .find(|(known, _)| known == name)
                    .map(|(_, state)| state.clone())
            }
        }
        Self(Arc::new(Fixed(
            components
                .into_iter()
                .map(|(name, state)| (name.to_owned(), state))
                .collect(),
        )))
    }
}

/// The check a component's owner runs on the directory RuntimeManager just
/// unpacked, before that directory is activated: the owner's own check, so a
/// bundle that cannot load on this host is refused instead of activated. A
/// build with no verifier for `name` refuses the install.
pub trait ComponentVerifier: Send + Sync + 'static {
    /// `Ok` activates the directory; `Err` is the reason the install failed.
    fn verify<'a>(&'a self, name: &'a str, root: &'a Path) -> BoxFuture<'a, Result<(), String>>;
}

/// The [`ComponentVerifier`] RuntimeManager reads when it activates a component.
pub struct ComponentVerifierHost(pub Arc<dyn ComponentVerifier>);

#[cfg(test)]
mod tests {
    use super::{ComponentSourceHost, ComponentState};
    use std::path::PathBuf;

    #[test]
    fn a_fixed_source_answers_each_name_with_the_same_state_each_time() {
        let host = ComponentSourceHost::fixed(vec![
            ("ocr", ComponentState::Off),
            (
                "other",
                ComponentState::Ready {
                    root: PathBuf::from("/bundle"),
                    version: "1".to_owned(),
                },
            ),
        ]);
        assert_eq!(host.0.component("ocr"), Some(ComponentState::Off));
        assert_eq!(host.0.component("ocr"), host.0.component("ocr"));
        assert_eq!(
            host.0.component("other"),
            Some(ComponentState::Ready {
                root: PathBuf::from("/bundle"),
                version: "1".to_owned(),
            })
        );
        assert_eq!(host.0.component("absent"), None);

        let failed =
            ComponentSourceHost::fixed(vec![("ocr", ComponentState::Failed("gone".into()))]);
        assert_eq!(failed.0.component("ocr"), failed.0.component("ocr"));
        assert_eq!(
            failed.0.component("ocr"),
            Some(ComponentState::Failed("gone".into()))
        );
    }
}
