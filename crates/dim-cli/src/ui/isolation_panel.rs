use dim_agent_types::isolation::{
    IsolationEscalation, IsolationPolicy, IsolationPolicySnapshot, Sandbox,
};

use crate::ui::line_input::LineInput;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{bordered, confirm_panel, key_hints};
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::theme::Palette;

enum Page {
    Main,
    Tier,
    Escalation,
    Advanced,
    List(usize),
    Input {
        field: usize,
        index: Option<usize>,
        input: Box<LineInput>,
    },
    ConfirmReset,
    ConfirmDiscard,
}

pub struct IsolationPanel {
    list: ListPanel,
    snapshot: Option<IsolationPolicySnapshot>,
    draft: Option<IsolationPolicy>,
    preset: String,
    page: Page,
    busy: bool,
    message: Option<String>,
}

impl IsolationPanel {
    pub fn new(preset: &str) -> Self {
        Self {
            list: Self::list(),
            snapshot: None,
            draft: None,
            preset: preset.to_owned(),
            page: Page::Main,
            busy: true,
            message: None,
        }
    }

    fn list() -> ListPanel {
        ListPanel::new(
            "Command isolation",
            "↑↓ move · Enter select · Esc back",
            "Loading policy…",
            "",
            "",
            false,
        )
        .without_filter()
    }

    pub fn set_snapshot(&mut self, result: Result<IsolationPolicySnapshot, String>) {
        self.busy = false;
        match result {
            Ok(snapshot) => {
                self.draft = Some(snapshot.policy.clone());
                self.snapshot = Some(snapshot);
                self.page = Page::Main;
                self.message = Some("Applies to new commands and new persistent shells.".into());
            }
            Err(error) => {
                self.page = Page::Main;
                self.message = Some(error);
            }
        }
        self.refresh();
    }

    fn dirty(&self) -> bool {
        match (&self.snapshot, &self.draft) {
            (Some(snapshot), Some(draft)) => snapshot.policy != *draft,
            _ => false,
        }
    }

    fn values(&self, field: usize) -> Option<Vec<String>> {
        let draft = self.draft.as_ref()?;
        match field {
            0 => draft.read_only_paths.as_ref().map(|paths| {
                paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            }),
            1 => draft.writable_paths.as_ref().map(|paths| {
                paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            }),
            _ => draft.environment.clone(),
        }
    }

    fn set_values(&mut self, field: usize, values: Option<Vec<String>>) {
        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        match field {
            0 => {
                draft.read_only_paths =
                    values.map(|values| values.into_iter().map(Into::into).collect())
            }
            1 => {
                draft.writable_paths =
                    values.map(|values| values.into_iter().map(Into::into).collect())
            }
            _ => draft.environment = values,
        }
    }

    fn version_two(&mut self) {
        let (Some(draft), Some(snapshot)) = (&mut self.draft, &self.snapshot) else {
            return;
        };
        if draft.version == 1 {
            draft.version = 2;
            draft.enabled = None;
            draft.network = None;
            draft.sandbox = Some(snapshot.effective.sandbox);
            draft.escalation = Some(snapshot.effective.escalation);
        }
    }

    fn refresh(&mut self) {
        let mut items = Vec::new();
        if let Some(message) = &self.message {
            items.push(Item::heading(message));
        }
        match self.page {
            Page::Main => {
                items.push(Item::heading(format!(
                    "Permission preset: {} (admission only)",
                    self.preset
                )));
                items.push(Item::heading("Execution node: local"));
                if let (Some(snapshot), Some(draft)) = (&self.snapshot, &self.draft) {
                    let sandbox = draft.sandbox.unwrap_or(snapshot.effective.sandbox);
                    let escalation = draft.escalation.unwrap_or(snapshot.effective.escalation);
                    items.push(Item::new(
                        "tier",
                        "Isolation tier",
                        format!("{} ›", sandbox.name()),
                    ));
                    items.push(Item::new(
                        "escalation",
                        "Wider requests",
                        format!("{} ›", escalation_name(escalation)),
                    ));
                    items.push(Item::new(
                        "advanced",
                        "Advanced",
                        "Read paths / Write paths / Environment names ›",
                    ));
                    items.push(Item::heading(format!(
                        "Policy source: {}",
                        match snapshot.source {
                            dim_agent_types::isolation::IsolationPolicySource::Defaults =>
                                "defaults",
                            dim_agent_types::isolation::IsolationPolicySource::File => "file",
                        }
                    )));
                    items.push(Item::heading(
                        "ask delegates to Permission/Approval; it is not always a prompt.",
                    ));
                    items.push(Item::new("save", "Save", ""));
                    items.push(Item::new("reset", "Reset to defaults", ""));
                }
                items.push(Item::new(
                    "reload",
                    "Reload",
                    "Discard edits and read current policy",
                ));
            }
            Page::Tier => {
                for value in ["workspace", "network", "host"] {
                    items.push(Item::new(
                        value,
                        value,
                        if value == "host" {
                            "Ordinary daemon-user access"
                        } else if value == "network" {
                            "Same filesystem grants, plus networking"
                        } else {
                            "Workspace writes, system reads, no network"
                        },
                    ));
                }
            }
            Page::Escalation => {
                for value in ["ask", "deny"] {
                    items.push(Item::new(
                        value,
                        value,
                        if value == "ask" {
                            "Permission/Approval decides"
                        } else {
                            "Refuse wider requests"
                        },
                    ));
                }
            }
            Page::Advanced => {
                for (index, name) in ["Read paths", "Write paths", "Environment names"]
                    .iter()
                    .enumerate()
                {
                    items.push(Item::new(index.to_string(), *name, "›"));
                }
            }
            Page::List(field) => {
                items.push(Item::heading(
                    "Explicit lists replace defaults; empty grants none.",
                ));
                items.push(Item::new("defaults", "Use defaults", ""));
                items.push(Item::new("clear", "Clear list", "Explicit empty list"));
                items.push(Item::new("add", "Add", ""));
                if let Some(values) = self.values(field) {
                    for (index, value) in values.iter().enumerate() {
                        items.push(Item::new(
                            format!("edit:{index}"),
                            value,
                            "Enter edit · Delete remove",
                        ));
                    }
                } else {
                    items.push(Item::heading("Using defaults"));
                }
            }
            _ => return,
        }
        let selected = match (&self.page, &self.snapshot, &self.draft) {
            (Page::Tier, Some(snapshot), Some(draft)) => {
                Some(draft.sandbox.unwrap_or(snapshot.effective.sandbox).name())
            }
            (Page::Escalation, Some(snapshot), Some(draft)) => Some(escalation_name(
                draft.escalation.unwrap_or(snapshot.effective.escalation),
            )),
            _ => None,
        };
        self.list = Self::list();
        self.list.set_items(Ok(items), selected);
    }

    pub fn paste(&mut self, text: &str) {
        if let Page::Input { input, .. } = &mut self.page {
            input.paste(text);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.busy {
            return if key.code == KeyCode::Esc && self.snapshot.is_none() {
                Effect::Close
            } else {
                Effect::None
            };
        }
        if let Page::Input {
            field,
            index,
            input,
        } = &mut self.page
        {
            if key.code == KeyCode::Esc {
                self.page = Page::List(*field);
                self.refresh();
            } else if key.code == KeyCode::Enter {
                let field = *field;
                let index = *index;
                let value = input.text().to_owned();
                if value.is_empty() {
                    return Effect::None;
                }
                let mut values = self.values(field).unwrap_or_default();
                if let Some(index) = index {
                    values[index] = value;
                } else {
                    values.push(value);
                }
                self.set_values(field, Some(values));
                self.page = Page::List(field);
                self.refresh();
            } else {
                input.handle_key(key);
            }
            return Effect::None;
        }
        if matches!(self.page, Page::ConfirmReset | Page::ConfirmDiscard) {
            if key.code == KeyCode::Char('y') {
                if matches!(self.page, Page::ConfirmDiscard) {
                    return Effect::Close;
                }
                if let Some(snapshot) = &self.snapshot {
                    self.busy = true;
                    return Effect::ResetIsolation(snapshot.revision.clone());
                }
            }
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('n') | KeyCode::Enter) {
                self.page = Page::Main;
                self.refresh();
            }
            return Effect::None;
        }
        if let Page::List(field) = self.page
            && key.code == KeyCode::Delete
            && let Some(id) = self.list.selected_id()
            && let Some(index) = id
                .strip_prefix("edit:")
                .and_then(|index| index.parse::<usize>().ok())
        {
            let mut values = self.values(field).unwrap_or_default();
            values.remove(index);
            self.set_values(field, Some(values));
            self.refresh();
            return Effect::None;
        }
        match self.list.handle_key(key) {
            Event::Close => {
                self.page = match self.page {
                    Page::Main if self.dirty() => Page::ConfirmDiscard,
                    Page::Main => return Effect::Close,
                    Page::Tier | Page::Escalation | Page::Advanced => Page::Main,
                    Page::List(_) => Page::Advanced,
                    _ => Page::Main,
                };
            }
            Event::Pick(id) => match self.page {
                Page::Main => match id.as_str() {
                    "tier" => self.page = Page::Tier,
                    "escalation" => self.page = Page::Escalation,
                    "advanced" => self.page = Page::Advanced,
                    "reset" => self.page = Page::ConfirmReset,
                    "reload" => {
                        self.busy = true;
                        return Effect::ReadIsolation;
                    }
                    "save" => {
                        if let (Some(snapshot), Some(draft)) = (&self.snapshot, &self.draft) {
                            self.busy = true;
                            return Effect::SaveIsolation {
                                expected_revision: snapshot.revision.clone(),
                                policy: draft.clone(),
                            };
                        }
                    }
                    _ => {}
                },
                Page::Tier => {
                    self.version_two();
                    if let Some(draft) = &mut self.draft {
                        draft.sandbox = Some(match id.as_str() {
                            "host" => Sandbox::Host,
                            "network" => Sandbox::Network,
                            _ => Sandbox::Workspace,
                        });
                    }
                    self.page = Page::Main;
                }
                Page::Escalation => {
                    self.version_two();
                    if let Some(draft) = &mut self.draft {
                        draft.escalation = Some(if id == "deny" {
                            IsolationEscalation::Deny
                        } else {
                            IsolationEscalation::Ask
                        });
                    }
                    self.page = Page::Main;
                }
                Page::Advanced => {
                    if let Ok(field) = id.parse() {
                        self.page = Page::List(field);
                    }
                }
                Page::List(field) => match id.as_str() {
                    "defaults" => self.set_values(field, None),
                    "clear" => self.set_values(field, Some(Vec::new())),
                    "add" => {
                        self.page = Page::Input {
                            field,
                            index: None,
                            input: Box::new(LineInput::new("")),
                        }
                    }
                    _ => {
                        if let Some(index) = id
                            .strip_prefix("edit:")
                            .and_then(|index| index.parse::<usize>().ok())
                        {
                            let values = self.values(field).unwrap_or_default();
                            self.page = Page::Input {
                                field,
                                index: Some(index),
                                input: Box::new(LineInput::new(&values[index])),
                            };
                        }
                    }
                },
                _ => {}
            },
            Event::None | Event::Refresh => return Effect::None,
        }
        self.refresh();
        Effect::None
    }

    pub fn render(&self, width: usize, rows: usize, palette: &Palette) -> Vec<String> {
        match &self.page {
            Page::Input { input, .. } => {
                let (value, _) = input.view(width.saturating_sub(6));
                bordered(
                    &[value, "Names only; no environment values".into()],
                    width,
                    None,
                    &palette.border,
                    Some("Edit isolation entry"),
                    Some(&key_hints("Enter save entry · Esc cancel", palette)),
                )
            }
            Page::ConfirmReset | Page::ConfirmDiscard => confirm_panel(
                "Command isolation",
                &[if matches!(self.page, Page::ConfirmReset) {
                    "Reset to product defaults?"
                } else {
                    "Discard unsaved changes?"
                }
                .into()],
                &["Yes (y)".into(), "Cancel (n)".into()],
                1,
                "y confirm · Enter/Esc cancel",
                width,
                palette,
            ),
            Page::Main => {
                let metadata = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| {
                        crate::ui::text::wrap_cells(
                            &format!("Policy path: {}", snapshot.path.display()),
                            width.saturating_sub(4).max(1),
                        )
                    })
                    .unwrap_or_default();
                let mut rendered =
                    self.list
                        .render(width, rows.saturating_sub(metadata.len()), palette);
                rendered.extend(metadata);
                rendered
            }
            _ => self.list.render(width, rows, palette),
        }
    }
}

fn escalation_name(value: IsolationEscalation) -> &'static str {
    match value {
        IsolationEscalation::Ask => "ask",
        IsolationEscalation::Deny => "deny",
    }
}
