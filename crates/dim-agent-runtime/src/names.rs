//! Services by name ([How Dim uses rutis §3](../../../docs/design/rutis-usage.md#3-services-by-name)).
//!
//! A service is a name in one group: the process, [`SESSION_GROUP`] or
//! [`BRANCH_GROUP`]. The catalog holds each name once with its group. A Rust
//! part is a rutis `host::DynPlugin` that keeps the keys of the services it
//! depends on, declares them in `injects` and reads them with `require_at`
//! in `apply`. [`Names`] gives those keys: the Rust binding takes a service's name from its type, as
//! `#[rutis::service]` does (`TreeCommands` is `treeCommands`), its group from
//! the catalog, and its key for each copy from the instances the copy runs in.
//! A part therefore names no group: where its row is placed decides, and a
//! row placed outside a group one of its names belongs to fails at
//! composition, naming that group.

use crate::*;
use rutis::Error;
use rutis::host::{ErrorKind, InstanceId, Key};
use rutis_loader::LoaderError;
pub use rutis_loader::ServiceCatalog;
use rutis_loader::resolve::Build;
use std::any::{TypeId, type_name};
use std::result::Result;

/// The service name of the Rust type `T`: its type name with the first word
/// lowercased, as `#[rutis::service]` names a type (`Timeline` is
/// `timeline`, `dyn LoopState` `loopState`, `HTTPClient` `httpClient`).
pub fn name_of<T: ?Sized>() -> String {
    name_of_type(type_name::<T>())
}

/// [`name_of`] for a type known by its name ([`rutis::TypeKey::type_name`]).
pub fn name_of_type(full: &str) -> String {
    let full = full.strip_prefix("dyn ").unwrap_or(full);
    let path = full.split(['<', ' ']).next().unwrap_or(full);
    let ident = path.rsplit("::").next().unwrap_or(path);
    let chars: Vec<char> = ident.chars().collect();
    let upper = chars.iter().take_while(|c| c.is_uppercase()).count();
    let lower = if upper > 1 && upper < chars.len() && chars[upper].is_lowercase() {
        upper - 1
    } else {
        upper.max(1).min(chars.len())
    };
    chars
        .iter()
        .enumerate()
        .flat_map(|(index, c)| {
            if index < lower {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![*c]
            }
        })
        .collect()
}

/// Registers the name of `T` in `group`, or at the process for `None`.
/// Registering it again in the same group does nothing, so each crate's
/// list of names can be applied by every installer that needs it.
///
/// # Panics
///
/// When the name is already registered in another group: a name belongs to
/// one group.
pub fn register<T: ?Sized + Send + Sync + 'static>(
    catalog: &mut ServiceCatalog,
    group: Option<&str>,
) {
    let name = name_of::<T>();
    let known = match registered_key(catalog, &name) {
        Err(LoaderError::UnknownService(_)) => None,
        Ok(key) if group.is_some() && key.type_id() != TypeId::of::<T>() => None,
        Ok(_) => Some(None),
        Err(LoaderError::OutsideInstance { group, .. }) => Some(Some(group)),
        Err(error) => panic!("{name}: {error}"),
    };
    match known {
        None => match group {
            None => catalog.register::<T>(name),
            Some(group) => catalog.register_instance::<T>(name, group),
        },
        Some(known) => {
            assert_eq!(
                known.as_deref(),
                group,
                "the service {name} is registered in two groups"
            );
            catalog
        }
    };
}

/// A catalog for an assembly, holding the runtime's own names. Every name it
/// does not register is shared across languages.
pub fn catalog() -> ServiceCatalog {
    let mut catalog = ServiceCatalog::new();
    register::<crate::PartRegistry>(&mut catalog, None);
    let session = Some(SESSION_GROUP);
    register::<SessionConfig>(&mut catalog, session);
    catalog.register_shared_instance(name_of::<AgentConfig>(), SESSION_GROUP);
    register::<SessionIdentity>(&mut catalog, session);
    register::<SessionLifecycle>(&mut catalog, session);
    register::<FaultReporter>(&mut catalog, session);
    register::<TimelineFactory>(&mut catalog, session);
    register::<dyn TimelineStore>(&mut catalog, session);
    register::<dyn TimelineTreeControl>(&mut catalog, session);
    register::<SessionLoader>(&mut catalog, session);
    let branch = Some(BRANCH_GROUP);
    register::<Timeline>(&mut catalog, branch);
    register::<Dispatcher>(&mut catalog, branch);
    register::<TimelineIdentity>(&mut catalog, branch);
    register::<BranchLifecycle>(&mut catalog, branch);
    register::<BranchOpening>(&mut catalog, branch);
    register::<dyn LoopState>(&mut catalog, branch);
    register::<dyn ConfigureControl>(&mut catalog, branch);
    catalog
}

/// Where one copy of a part runs, to find the keys of the names it depends
/// on.
pub struct Names<'a> {
    catalog: &'a ServiceCatalog,
    place: Place<'a>,
    faces: Option<&'a crate::Faces>,
}
enum Place<'a> {
    /// A copy running in these instances.
    Copy(&'a Build),
    /// The composition check of a part placed inside `groups`; keys use a
    /// placeholder instance.
    Check {
        groups: &'a [&'a str],
        instance: Option<InstanceId>,
    },
}
impl<'a> Names<'a> {
    /// The names of a copy running in `build`'s instances.
    pub fn copy(catalog: &'a ServiceCatalog, build: &'a Build) -> Self {
        Self {
            catalog,
            place: Place::Copy(build),
            faces: None,
        }
    }
    /// The names of a copy running in `build`'s instances, rejecting the
    /// internal faces a leaf cannot borrow (#1289).
    pub fn copy_with_faces(
        catalog: &'a ServiceCatalog,
        build: &'a Build,
        faces: &'a crate::Faces,
    ) -> Self {
        Self {
            catalog,
            place: Place::Copy(build),
            faces: Some(faces),
        }
    }
    /// The composition check of a part placed inside `groups`, innermost
    /// first; `instance` stands in for each of them (none at the process).
    pub fn check(
        catalog: &'a ServiceCatalog,
        groups: &'a [&'a str],
        instance: Option<InstanceId>,
    ) -> Self {
        Self {
            catalog,
            place: Place::Check { groups, instance },
            faces: None,
        }
    }
    /// The instance of the enclosing `group` the copy runs in.
    pub fn instance(&self, group: &str) -> Result<InstanceId, Error> {
        match &self.place {
            Place::Copy(build) => build.instance(group),
            Place::Check { groups, instance } if groups.contains(&group) => {
                instance.ok_or_else(|| outside(group))
            }
            Place::Check { .. } => Err(outside(group)),
        }
    }
    /// The key of `T`'s name for this copy.
    fn reject_internal(&self, name: &str) -> Result<(), Error> {
        if let Some(reason) = self.faces.and_then(|faces| faces.reason(name)) {
            return Err(rutis::Error::from(ErrorKind::PluginFailed(
                format!("internal_service: {name} is internal: {reason}").into(),
            )));
        }
        Ok(())
    }
    pub fn key<T: ?Sized + 'static>(&self) -> Result<Key<T>, Error> {
        let name = name_of::<T>();
        self.reject_internal(&name)?;
        let failed = |message: String| rutis::Error::from(ErrorKind::PluginFailed(message.into()));
        let group = match self.catalog.key_in(&name, &Build::default()) {
            Ok(key) => {
                self.check_type::<T>(&name, &key)?;
                return Ok(Key::of());
            }
            Err(LoaderError::OutsideInstance { group, .. }) => group,
            Err(LoaderError::UnknownService(_)) => {
                return Err(failed(format!("{name} is not a registered service")));
            }
            Err(error) => return Err(failed(error.to_string())),
        };
        let instance = match &self.place {
            Place::Copy(build) => {
                let key = self
                    .catalog
                    .key_in(&name, build)
                    .map_err(|_| misplaced(&name, &group))?;
                self.check_type::<T>(&name, &key)?;
                build.instance(&group)?
            }
            Place::Check { groups, instance } => {
                if !groups.contains(&group.as_str()) {
                    return Err(misplaced(&name, &group));
                }
                instance.ok_or_else(|| misplaced(&name, &group))?
            }
        };
        Ok(Key::of().instance(instance))
    }
    /// Whether the assembly names `T`: a part whose dependency on it is
    /// optional includes it only then ([`Self::when`]).
    pub fn registered<T: ?Sized + 'static>(&self) -> bool {
        !matches!(
            registered_key(self.catalog, &name_of::<T>()),
            Err(LoaderError::UnknownService(_))
        )
    }
    /// The key of the service `name` as the catalog keys it for this copy.
    /// A name that crosses languages is at its dispatcher's key, so `T` is
    /// then `dyn HostDispatch`: there a provider of any language is found.
    /// [`Self::key`] is the form for a name registered with a Rust type.
    pub fn named<T: ?Sized + 'static>(&self, name: &str) -> Result<Key<T>, Error> {
        self.reject_internal(name)?;
        let failed = |message: String| rutis::Error::from(ErrorKind::PluginFailed(message.into()));
        let key = match self.catalog.key_in(name, &Build::default()) {
            Ok(key) => key,
            Err(LoaderError::OutsideInstance { group, .. }) => match &self.place {
                Place::Copy(build) => self
                    .catalog
                    .key_in(name, build)
                    .map_err(|_| misplaced(name, &group))?,
                Place::Check { groups, instance } => {
                    let instance = instance
                        .filter(|_| groups.contains(&group.as_str()))
                        .ok_or_else(|| misplaced(name, &group))?;
                    return Ok(Key::dynamic(name.to_owned()).instance(instance));
                }
            },
            Err(LoaderError::UnknownService(_)) => {
                return Err(failed(format!("{name} is not a registered service")));
            }
            Err(error) => return Err(failed(error.to_string())),
        };
        if key.type_id() != TypeId::of::<T>() {
            return Err(failed(format!(
                "{name} is a {}, not a {}",
                key.type_name(),
                type_name::<T>()
            )));
        }
        let typed = match key.name() {
            Some(qualifier) => Key::<T>::dynamic(qualifier.to_owned()),
            None => Key::of(),
        };
        let typed = match key.instance_id() {
            Some(instance) => typed.instance(instance),
            None => typed,
        };
        if typed.erase() != key {
            return Err(failed(format!(
                "{name} is at {}, which a typed key cannot name",
                key.describe()
            )));
        }
        Ok(typed)
    }
    /// The key of this branch's state cell of type `S`, which the Session
    /// and branch facades read (`Session::get`/`on`). State cells are keyed
    /// by type in their branch, not named; only a branch part provides one.
    pub fn state_cell<S: Clone + Send + Sync + 'static>(&self) -> Result<Key<StateCell<S>>, Error> {
        let instance = self.instance(BRANCH_GROUP).map_err(|_| {
            rutis::Error::from(ErrorKind::PluginFailed(
                format!(
                    "the state cell of {} is a branch's: place the part inside the branch group",
                    type_name::<S>()
                )
                .into(),
            ))
        })?;
        Ok(Key::of().instance(instance))
    }
    /// A name registered for a Rust service of another type is a programming
    /// error; a shared name crosses languages and has no Rust type here.
    fn check_type<T: ?Sized + 'static>(
        &self,
        name: &str,
        key: &rutis::host::TypeKey,
    ) -> Result<(), Error> {
        if self.catalog.is_shared(name) || key.type_id() == TypeId::of::<T>() {
            return Ok(());
        }
        Err(rutis::Error::from(ErrorKind::PluginFailed(
            format!(
                "{name} is a {}, not a {}",
                key.type_name(),
                type_name::<T>()
            )
            .into(),
        )))
    }
}
/// The catalog's key for `name` outside any instance, or `UnknownService`
/// when the host did not register the name: rutis shares every name it does
/// not register, at `host_key(name)`, so that key alone says nothing was
/// registered. A name Dim registers as shared is read with [`Names::named`].
pub fn registered_key(
    catalog: &ServiceCatalog,
    name: &str,
) -> std::result::Result<rutis::host::TypeKey, LoaderError> {
    match catalog.key_in(name, &Build::default()) {
        Ok(key) if key == rutis_bridge::session::host_key(name).erase() => {
            Err(LoaderError::UnknownService(vec![name.to_owned()]))
        }
        other => other,
    }
}
fn outside(group: &str) -> Error {
    rutis::Error::from(ErrorKind::PluginFailed(
        format!("the part is not inside a {group} instance").into(),
    ))
}
fn misplaced(name: &str, group: &str) -> Error {
    rutis::Error::from(ErrorKind::PluginFailed(
        format!("{name} is a {group} service: place the part inside the {group} group").into(),
    ))
}

/// Fails unless the catalog registers `T`'s name in `group`: what a facade
/// reads of a Session or branch is a service of that group.
pub(crate) fn in_group<T: ?Sized + 'static>(
    catalog: &ServiceCatalog,
    group: &str,
) -> crate::Result<()> {
    named_in_group(catalog, &name_of::<T>(), group)
}
/// [`in_group`] for a name.
pub(crate) fn named_in_group(
    catalog: &ServiceCatalog,
    name: &str,
    group: &str,
) -> crate::Result<()> {
    match catalog.key_in(name, &Build::default()) {
        Err(LoaderError::OutsideInstance { group: found, .. }) if found == group => Ok(()),
        _ => Err(crate::AgentError::InvalidInput(format!(
            "{name} is not a {group} service"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_service_macro() {
        assert_eq!(name_of::<Timeline>(), "timeline");
        assert_eq!(name_of::<TimelineIdentity>(), "timelineIdentity");
        assert_eq!(name_of::<dyn LoopState>(), "loopState");
        assert_eq!(name_of::<dyn TimelineStore>(), "timelineStore");
        assert_eq!(name_of::<StateCell<u8>>(), "stateCell");
        struct HTTPClient;
        assert_eq!(name_of::<HTTPClient>(), "httpClient");
    }
}
