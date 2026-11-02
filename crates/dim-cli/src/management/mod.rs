//! The area adapters behind the native client's management surfaces
//! (cli-commands.md "Management surfaces").
//!
//! One adapter per management area talks to the daemon through the client
//! connection, so a subcommand needs no Session; turns its owner's reply into
//! one [`ManagementView`] at that boundary; performs the area's actions; and
//! refuses an action whose methods the daemon does not advertise, without
//! falling back. The panel and the subcommands both call [`apply`] and know no
//! area's methods.
//!
//! Each area is one table: the read that reports it and the method behind each
//! kind it has. [`supported`], [`actions`] and the views read that table, so
//! adding an area, or a kind to an area, writes the method once.

pub mod editor;
pub mod hooks;
pub mod skills;

use std::path::Path;

use crate::client::Client;
use crate::model::session::{
    ActionKind, Area, ManagementAction, ManagementEntry, ManagementGroup, ManagementRefusal,
    ManagementRequest, ManagementResult,
};

/// One area's management methods: the read that reports the area, and the
/// method behind each kind it has.
struct Methods {
    catalog: &'static str,
    kinds: &'static [(ActionKind, &'static str)],
}

impl Methods {
    fn method(&self, kind: ActionKind) -> Option<&'static str> {
        self.kinds
            .iter()
            .find(|(has, _)| *has == kind)
            .map(|(_, method)| *method)
    }
}

const SKILLS: Methods = Methods {
    catalog: "skills.catalog",
    kinds: &[
        (ActionKind::Toggle, "skills.set_enabled"),
        (ActionKind::Reload, "skills.refresh"),
    ],
};

const HOOKS: Methods = Methods {
    catalog: "hooks.catalog",
    kinds: &[
        (ActionKind::Toggle, "hooks.set_enabled"),
        (ActionKind::Edit, "hooks.save"),
        // A reload re-reads the document, so it needs no rescan method.
        (ActionKind::Reload, "hooks.catalog"),
    ],
};

fn methods_of(area: Area) -> &'static Methods {
    match area {
        Area::Skills => &SKILLS,
        Area::Hooks => &HOOKS,
    }
}

/// The kind an action exercises; a read has none.
fn kind(action: &ManagementAction) -> Option<ActionKind> {
    match action {
        ManagementAction::List => None,
        ManagementAction::Toggle { .. } => Some(ActionKind::Toggle),
        ManagementAction::Reload => Some(ActionKind::Reload),
        ManagementAction::Save { .. } => Some(ActionKind::Edit),
    }
}

/// Whether a kind acts on the area itself rather than on one entry.
fn acts_on_the_area(kind: ActionKind) -> bool {
    matches!(kind, ActionKind::Reload)
}

/// The kinds of `available` that act on the area, in their order.
pub(super) fn area_kinds(available: &[ActionKind]) -> Vec<ActionKind> {
    available
        .iter()
        .copied()
        .filter(|kind| acts_on_the_area(*kind))
        .collect()
}

/// The kinds of `available` that act on one entry, in their order.
pub(super) fn entry_kinds(available: &[ActionKind]) -> Vec<ActionKind> {
    available
        .iter()
        .copied()
        .filter(|kind| !acts_on_the_area(*kind))
        .collect()
}

/// The action kinds the daemon can serve for `area`, in the order a panel
/// offers them. An area the daemon cannot read offers none.
pub fn actions(client: &Client, area: Area) -> Vec<ActionKind> {
    let methods = methods_of(area);
    if !client.supports(&[methods.catalog]) {
        return Vec::new();
    }
    methods
        .kinds
        .iter()
        .filter(|(_, method)| client.supports(&[method]))
        .map(|(kind, _)| *kind)
        .collect()
}

/// Refuses a request the daemon cannot serve: an area without the method, or
/// a kind the area does not have. The client never falls back to a file,
/// another method or the host's own environment.
pub fn supported(client: &Client, request: &ManagementRequest) -> Result<(), String> {
    match kind(&request.action) {
        Some(kind) => supports_kind(client, request.area, kind),
        None => {
            let methods = methods_of(request.area);
            if client.supports(&[methods.catalog]) {
                Ok(())
            } else {
                Err(unsupported(request.area))
            }
        }
    }
}

/// Refuses a kind the daemon cannot serve for `area`, with the area's
/// sentence, for a surface that checks before it gathers anything.
pub fn supports_kind(client: &Client, area: Area, kind: ActionKind) -> Result<(), String> {
    let methods = methods_of(area);
    if !client.supports(&[methods.catalog]) {
        return Err(unsupported(area));
    }
    match methods.method(kind) {
        Some(method) if client.supports(&[method]) => Ok(()),
        _ => Err(unsupported(area)),
    }
}

fn unsupported(area: Area) -> String {
    format!("This daemon cannot manage {}.", area.name())
}

/// Applies `request` to its area and reports the area afterwards.
pub async fn apply(
    client: &mut Client,
    request: &ManagementRequest,
    cwd: &Path,
) -> ManagementResult {
    if let Err(error) = supported(client, request) {
        return ManagementResult::Refused(ManagementRefusal::Refused(error));
    }
    let actions = actions(client, request.area);
    match request.area {
        Area::Skills => match skills::apply(client, &request.action, cwd).await {
            skills::Outcome::Catalog(catalog) => {
                ManagementResult::View(skills::view(&catalog, &actions))
            }
            skills::Outcome::ReadFailed(error) => ManagementResult::Applied { read_error: error },
            skills::Outcome::Refused(message) => {
                ManagementResult::Refused(ManagementRefusal::Refused(message))
            }
        },
        Area::Hooks => match hooks::apply(client, &request.action, cwd).await {
            Ok(catalog) => ManagementResult::View(hooks::view(&catalog, &actions)),
            Err(hooks::Failure::ReadFailed(error)) => {
                ManagementResult::Applied { read_error: error }
            }
            Err(hooks::Failure::Refused(message)) => {
                ManagementResult::Refused(ManagementRefusal::Refused(message))
            }
            Err(hooks::Failure::Conflict { message, catalog }) => {
                ManagementResult::Refused(ManagementRefusal::Conflict {
                    message,
                    view: catalog.map(|catalog| Box::new(hooks::view(&catalog, &actions))),
                })
            }
        },
    }
}

/// Entries collected under the headings their owner reports, in the order it
/// reports them.
#[derive(Default)]
struct Groups(Vec<ManagementGroup>);

impl Groups {
    fn push(&mut self, heading: &str, entry: ManagementEntry) {
        match self.0.iter_mut().find(|group| group.heading == heading) {
            Some(group) => group.entries.push(entry),
            None => self.0.push(ManagementGroup {
                heading: heading.to_owned(),
                entries: vec![entry],
            }),
        }
    }

    fn into_vec(self) -> Vec<ManagementGroup> {
        self.0
    }
}
