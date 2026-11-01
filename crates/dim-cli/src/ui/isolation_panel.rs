use crate::model::session::{ExecutionSnapshot, SandboxBackend};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::confirm_panel;
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::theme::Palette;

pub struct IsolationPanel {
    list: ListPanel,
    snapshot: Option<ExecutionSnapshot>,
    draft: Option<ExecutionSnapshot>,
    backend: Option<SandboxBackend>,
    busy: bool,
    message: Option<String>,
    confirm_reset: bool,
    confirm_discard: bool,
}

impl IsolationPanel {
    pub fn new() -> Self {
        Self {
            list: Self::list(),
            snapshot: None,
            draft: None,
            backend: None,
            busy: true,
            message: None,
            confirm_reset: false,
            confirm_discard: false,
        }
    }

    fn list() -> ListPanel {
        ListPanel::new(
            "Process isolation",
            "↑↓ move · Enter select · Esc close",
            "Loading process isolation…",
            "",
            "",
            false,
        )
        .without_filter()
    }

    pub fn set_snapshot(&mut self, result: Result<ExecutionSnapshot, String>) {
        self.busy = false;
        self.confirm_reset = false;
        match result {
            Ok(snapshot) => {
                self.draft = Some(snapshot.clone());
                self.snapshot = Some(snapshot);
                self.message = None;
            }
            Err(error) => {
                self.message = Some(error);
            }
        }
        self.refresh();
    }

    /// The host's sandbox-backend answer; the read happens with the switches.
    /// A failed read adds no line and leaves the switches usable.
    pub fn set_backend(&mut self, result: Result<SandboxBackend, String>) {
        if let Ok(backend) = result {
            self.backend = Some(backend);
            self.refresh();
        }
    }

    fn dirty(&self) -> bool {
        match (&self.snapshot, &self.draft) {
            (Some(snapshot), Some(draft)) => {
                snapshot.sandbox != draft.sandbox || snapshot.network != draft.network
            }
            _ => false,
        }
    }

    fn refresh(&mut self) {
        let mut items = Vec::new();
        let mut notes = Vec::new();
        if let Some(message) = &self.message {
            items.push(Item::heading(message));
        }
        if let Some(SandboxBackend::Unavailable { reason }) = &self.backend {
            items.push(Item::heading(format!(
                "Sandbox is unavailable on this host: {reason}"
            )));
        }
        if let (Some(_snapshot), Some(draft)) = (&self.snapshot, &self.draft) {
            notes.push("Applies to new processes and shells.".into());
            if !draft.sandbox {
                notes.push("Off: commands use your OS permissions, not file-tool limits.".into());
            }
            for (id, label, selected) in [
                ("off", "Off", !draft.sandbox),
                (
                    "no-network",
                    "Sandboxed · No network",
                    draft.sandbox && !draft.network,
                ),
                (
                    "network",
                    "Sandboxed · Network allowed",
                    draft.sandbox && draft.network,
                ),
            ] {
                items.push(Item::new(id, label, if selected { "selected" } else { "" }));
            }
            items.push(Item::new("save", "Save", ""));
            items.push(Item::new("reset", "Reset to defaults", ""));
        }
        items.push(Item::new(
            "reload",
            "Reload",
            "Discard edits and read the current settings",
        ));
        let selected = self.list.selected_id();
        self.list = Self::list().with_notes(notes);
        self.list.set_items(Ok(items), selected.as_deref());
    }

    pub fn paste(&mut self, _text: &str) {}

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.busy {
            return if key.code == KeyCode::Esc && self.snapshot.is_none() {
                Effect::Close
            } else {
                Effect::None
            };
        }
        if self.confirm_reset || self.confirm_discard {
            if key.code == KeyCode::Char('y') {
                if self.confirm_discard {
                    return Effect::Close;
                }
                if let Some(snapshot) = &self.snapshot {
                    self.busy = true;
                    return Effect::ResetIsolation(snapshot.revision.clone());
                }
            }
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('n') | KeyCode::Enter) {
                self.confirm_reset = false;
                self.confirm_discard = false;
                self.refresh();
            }
            return Effect::None;
        }
        match self.list.handle_key(key) {
            Event::Close => {
                if self.dirty() {
                    self.confirm_discard = true;
                } else {
                    return Effect::Close;
                }
            }
            Event::Pick(id) => match id.as_str() {
                "off" => {
                    if let Some(draft) = &mut self.draft {
                        draft.sandbox = false;
                    }
                }
                "no-network" | "network" => {
                    if let Some(draft) = &mut self.draft {
                        draft.sandbox = true;
                        draft.network = id == "network";
                    }
                }
                "save" => {
                    if let (Some(snapshot), Some(draft)) = (&self.snapshot, &self.draft) {
                        self.busy = true;
                        return Effect::SaveIsolation {
                            expected_revision: snapshot.revision.clone(),
                            sandbox: draft.sandbox,
                            network: draft.network,
                        };
                    }
                }
                "reset" => self.confirm_reset = true,
                "reload" => {
                    self.busy = true;
                    return Effect::ReadIsolation;
                }
                _ => {}
            },
            Event::None | Event::Refresh => return Effect::None,
        }
        self.refresh();
        Effect::None
    }

    pub fn render(&self, width: usize, rows: usize, palette: &Palette) -> Vec<String> {
        if self.confirm_reset || self.confirm_discard {
            return confirm_panel(
                "Process isolation",
                &[if self.confirm_reset {
                    "Reset the execution switches to defaults?"
                } else {
                    "Discard unsaved changes?"
                }
                .into()],
                &["Yes (y)".into(), "Cancel (n)".into()],
                1,
                "y confirm · Enter/Esc cancel",
                width,
                palette,
            );
        }
        self.list.render(width, rows, palette)
    }
}
