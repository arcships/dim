//! The permission preset, sharing the Settings pickers.
//!
//! ```text
//! Permissions
//! > Read only        reads only; writes and commands are refused
//!   Workspace write  writes inside the workspace; asks outside it
//!   Full access      no path limits; tools run without asking
//!   Command rules    ›
//!   Process isolation  Settings ›
//! ```
//!
//! Permission is the preset (ADR 018). Process isolation stays in Settings.

use dim_agent_types::{PermissionPolicy, PermissionPreset};

use crate::model::session::Area;
use crate::permission::PRESETS;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::KeyEvent;
use crate::ui::theme::Palette;

const HINT: &str = "↑↓/Tab move · Enter apply · Esc close";

pub struct ApprovalsPanel {
    list: ListPanel,
    policy: PermissionPolicy,
}

impl ApprovalsPanel {
    pub fn new(policy: &PermissionPolicy) -> Self {
        let mut panel = Self {
            list: ListPanel::new("Permissions", HINT, "", "", "", false).without_filter(),
            policy: policy.clone(),
        };
        panel.refresh();
        panel
    }

    pub fn set_policy(&mut self, policy: &PermissionPolicy) {
        self.policy = policy.clone();
        self.refresh();
    }

    fn refresh(&mut self) {
        let current = self.policy.preset;
        let items = PRESETS
            .into_iter()
            .map(|preset| {
                Item::new(
                    preset_name(preset),
                    preset_label(preset),
                    if preset == current { "current" } else { "" },
                )
            })
            .chain([
                Item::new("rules", "Command rules", "›"),
                Item::new("isolation", "Process isolation", "Settings ›"),
            ])
            .collect::<Vec<_>>();
        let notes = PRESETS
            .into_iter()
            .map(|preset| format!("{}: {}", preset_label(preset), consequence(preset)))
            .chain(std::iter::once(
                "The preset fixes the path boundary.".to_owned(),
            ))
            .collect();
        self.list = ListPanel::new("Permissions", HINT, "", "", "", false)
            .without_filter()
            .with_notes(notes);
        self.list.set_items(Ok(items), Some(preset_name(current)));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match self.list.handle_key(key) {
            Event::Close => Effect::Close,
            Event::Pick(id) => match id.as_str() {
                "rules" => Effect::OpenArea {
                    area: Area::Permissions,
                    note: None,
                },
                "isolation" => Effect::Setting(crate::ui::settings_panel::Setting::Isolation),
                name => match PRESETS
                    .into_iter()
                    .find(|preset| preset_name(*preset) == name)
                {
                    Some(preset) => Effect::SetPermission(preset),
                    None => Effect::None,
                },
            },
            Event::None | Event::Refresh => Effect::None,
        }
    }

    pub fn render(&self, width: usize, rows: usize, palette: &Palette) -> Vec<String> {
        self.list.render(width, rows, palette)
    }
}

fn preset_name(preset: PermissionPreset) -> &'static str {
    crate::permission::name(preset)
}

fn preset_label(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "Read only",
        PermissionPreset::WorkspaceWrite => "Workspace write",
        PermissionPreset::FullAccess => "Full access",
    }
}

fn consequence(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "reads only; writes and commands are refused",
        PermissionPreset::WorkspaceWrite => "writes inside the workspace; asks outside it",
        PermissionPreset::FullAccess => "no path limits; tools run without asking",
    }
}
