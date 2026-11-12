//! Session permission presets and links to user-wide settings.

use dim_agent_types::{PermissionPolicy, PermissionPreset};

use crate::model::session::Area;
use crate::permission::PRESETS;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::KeyEvent;
use crate::ui::theme::Palette;

pub struct ApprovalsPanel {
    list: ListPanel,
    preset: PermissionPreset,
}

impl ApprovalsPanel {
    pub fn new(policy: &PermissionPolicy) -> Self {
        let mut panel = Self {
            list: ListPanel::new("Permissions", "", "", "", "", false)
                .without_filter()
                .with_inline_rows()
                .with_section_headings(),
            preset: policy.preset,
        };
        panel.refresh();
        panel
    }

    pub fn set_policy(&mut self, policy: &PermissionPolicy) {
        self.preset = policy.preset;
        self.refresh();
    }

    fn refresh(&mut self) {
        let current = self.preset;
        let selected = self.list.selected_id();
        let items = std::iter::once(Item::heading("Permission mode"))
            .chain(PRESETS.into_iter().map(|preset| {
                let marker = if preset == current { "(*)" } else { "( )" };
                Item::new(
                    preset_name(preset),
                    format!("{marker} {}", preset_label(preset)),
                    "",
                )
            }))
            .chain([
                Item::heading("Related settings"),
                Item::new("rules", "Command rules", "›"),
                Item::new("isolation", "Process isolation", "›"),
            ])
            .collect::<Vec<_>>();
        self.list.set_items(
            Ok(items),
            Some(selected.as_deref().unwrap_or(preset_name(current))),
        );
        self.focus_hint();
    }

    fn focus_hint(&mut self) {
        let selected = self.list.selected_id();
        let (action, note) = match selected.as_deref() {
            Some("rules") => (
                "open",
                "User-wide command rules. Allow skips asks, never denials.".to_owned(),
            ),
            Some("isolation") => (
                "open",
                "Opens Settings; user-wide, for new processes; independent of permission mode."
                    .to_owned(),
            ),
            name => {
                let preset = PRESETS
                    .into_iter()
                    .find(|preset| Some(preset_name(*preset)) == name)
                    .expect("permission rows name a preset");
                (
                    if preset == self.preset {
                        "keep mode"
                    } else {
                        "use mode"
                    },
                    format!(
                        "Current session; remembered for new sessions. {}",
                        consequence(preset)
                    ),
                )
            }
        };
        self.list
            .set_hint(format!("↑↓/Tab move · Enter {action} · Esc close"));
        self.list.set_notes(vec![note]);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let event = self.list.handle_key(key);
        self.focus_hint();
        match event {
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
        PermissionPreset::ReadOnly => "Writes and commands denied; external reads may ask.",
        PermissionPreset::WorkspaceWrite => {
            "Workspace writes allowed; commands, network and ungranted outside access usually ask."
        }
        PermissionPreset::FullAccess => {
            "Skips preset approval; pending calls on this branch are reevaluated. Explicit denies and isolation remain."
        }
    }
}
