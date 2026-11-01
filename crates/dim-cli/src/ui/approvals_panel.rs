//! Independent permission controls, sharing the Settings pickers.
//!
//! ```text
//! Permissions
//! > File access          Workspace read/write ›
//!   Tool approval        Ask ›
//!   Process isolation    ›
//! ```

use dim_agent_types::{FileAccess, PermissionPolicy, PermissionPreset, ToolApproval};

use crate::permission::{Change, file_label, tool_policy_label};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::settings_panel::Setting;
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::theme::Palette;

#[derive(Clone, Copy)]
enum Page {
    Permissions,
    File,
    Tool,
}

pub struct ApprovalsPanel {
    list: ListPanel,
    policy: PermissionPolicy,
    page: Page,
    standalone: bool,
}

impl ApprovalsPanel {
    pub fn new(policy: &PermissionPolicy) -> Self {
        Self::open(policy, Page::Permissions, false)
    }

    pub fn file(policy: &PermissionPolicy) -> Self {
        Self::open(policy, Page::File, true)
    }

    pub fn tool(policy: &PermissionPolicy) -> Self {
        Self::open(policy, Page::Tool, true)
    }

    fn open(policy: &PermissionPolicy, page: Page, standalone: bool) -> Self {
        let mut panel = Self {
            list: ListPanel::new("Permissions", "", "", "", "", false).without_filter(),
            policy: policy.clone(),
            page,
            standalone,
        };
        panel.refresh();
        panel
    }

    pub fn set_policy(&mut self, policy: &PermissionPolicy) {
        self.policy = policy.clone();
        self.refresh();
    }

    fn refresh(&mut self) {
        let badge = |current| if current { "current" } else { "" };
        let mut notes = Vec::new();
        let (title, items, selected) = match self.page {
            Page::Permissions => (
                "Permissions",
                vec![
                    Item::new(
                        "file",
                        "File access",
                        format!("{} ›", file_label(self.policy.file_access())),
                    ),
                    Item::new(
                        "tool",
                        "Tool approval",
                        format!("{} ›", tool_policy_label(&self.policy)),
                    ),
                    Item::new("isolation", "Process isolation", "›"),
                ],
                None,
            ),
            Page::File => {
                let choices = [
                    FileAccess::ReadOnly,
                    FileAccess::WorkspaceReadWrite,
                    FileAccess::Unrestricted,
                ];
                let current = self.policy.file_access();
                notes = vec![
                    "Read only: no writes.".into(),
                    "Workspace read/write: writes inside; asks outside. System and session files stay protected.".into(),
                    "Unrestricted: no path limits. Tool approval still applies.".into(),
                ];
                let items = choices
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| {
                        Item::new(
                            index.to_string(),
                            file_label(value),
                            badge(value == current),
                        )
                    })
                    .collect();
                (
                    "File access",
                    items,
                    Some(
                        choices
                            .iter()
                            .position(|value| *value == current)
                            .unwrap()
                            .to_string(),
                    ),
                )
            }
            Page::Tool => {
                let current = self.policy.tool_approval();
                notes = vec![
                    "Ask: confirm tools unless explicitly allowed.".into(),
                    "Bypass: skip tool prompts, not file limits, deny rules or isolation.".into(),
                ];
                if self.policy.tool_approval.is_none() {
                    notes.push(
                        match self.policy.preset {
                            PermissionPreset::ReadOnly => {
                                "Legacy defaults: allow reads; deny writes/commands."
                            }
                            PermissionPreset::WorkspaceWrite => {
                                "Legacy defaults: allow reads/writes; ask for commands."
                            }
                            PermissionPreset::FullAccess => "Legacy default: bypass tool approval.",
                        }
                        .into(),
                    );
                }
                let items = vec![
                    Item::new(
                        "ask",
                        "Ask",
                        badge(self.policy.tool_approval == Some(ToolApproval::Ask)),
                    ),
                    Item::new("bypass", "Bypass", badge(current == ToolApproval::Bypass)),
                ];
                (
                    "Tool approval",
                    items,
                    Some(
                        if current == ToolApproval::Ask {
                            "ask"
                        } else {
                            "bypass"
                        }
                        .into(),
                    ),
                )
            }
        };
        self.list = ListPanel::new(
            title,
            "↑↓/Tab move · Enter apply · Esc back",
            "",
            "",
            "",
            false,
        )
        .without_filter()
        .with_notes(notes);
        self.list.set_items(Ok(items), selected.as_deref());
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if key.code == KeyCode::Esc && !self.standalone && !matches!(self.page, Page::Permissions) {
            self.page = Page::Permissions;
            self.refresh();
            return Effect::None;
        }
        match self.list.handle_key(key) {
            Event::Close => Effect::Close,
            Event::Pick(id) => match self.page {
                Page::Permissions => {
                    match id.as_str() {
                        "file" => self.page = Page::File,
                        "tool" => self.page = Page::Tool,
                        "isolation" => return Effect::Setting(Setting::Isolation),
                        _ => return Effect::None,
                    }
                    self.refresh();
                    Effect::None
                }
                Page::File => Effect::SetPermission(Change::FileAccess(match id.as_str() {
                    "0" => FileAccess::ReadOnly,
                    "1" => FileAccess::WorkspaceReadWrite,
                    _ => FileAccess::Unrestricted,
                })),
                Page::Tool => Effect::SetPermission(Change::ToolApproval(if id == "ask" {
                    ToolApproval::Ask
                } else {
                    ToolApproval::Bypass
                })),
            },
            Event::None | Event::Refresh => Effect::None,
        }
    }

    pub fn render(&self, width: usize, rows: usize, palette: &Palette) -> Vec<String> {
        self.list.render(width, rows, palette)
    }
}
