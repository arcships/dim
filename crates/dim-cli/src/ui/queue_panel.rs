use crate::model::view::ViewSnapshot;
use crate::sanitize::safe;
use crate::ui::i18n::t;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;

pub struct QueuePanel {
    session_id: String,
    projection_id: u64,
    focus: String,
    list: ListPanel,
}

impl QueuePanel {
    pub fn new(snapshot: &ViewSnapshot) -> Self {
        let mut panel = Self {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            focus: snapshot.focus.clone(),
            list: ListPanel::new(
                "Queue",
                "",
                "",
                "No queued messages.",
                "No queued messages.",
                false,
            )
            .without_filter(),
        };
        panel.set_rows(snapshot);
        panel
    }

    pub fn current(&self, snapshot: &ViewSnapshot) -> bool {
        self.session_id == snapshot.session_id
            && self.projection_id == snapshot.projection_id
            && self.focus == snapshot.focus
    }

    pub fn set_rows(&mut self, snapshot: &ViewSnapshot) {
        let selected = self.list.selected_id();
        let items = snapshot
            .queued
            .iter()
            .map(|input| {
                let text = input.display.as_deref().unwrap_or(&input.text);
                Item::new(
                    input.id.clone(),
                    safe(&text.split_whitespace().collect::<Vec<_>>().join(" ")),
                    t(if input.held {
                        "Held · awaiting application"
                    } else if snapshot.running {
                        "Incorporating · awaiting model consumption"
                    } else {
                        "Unread · waiting for your next message"
                    }),
                )
            })
            .collect();
        self.list.set_items(Ok(items), selected.as_deref());
        self.set_hint(snapshot);
    }

    fn set_hint(&mut self, snapshot: &ViewSnapshot) {
        let held = self.list.selected_id().is_some_and(|id| {
            snapshot
                .queued
                .iter()
                .any(|input| input.id == id && input.held)
        });
        self.list.set_hint(
            t(if held {
                "Enter steer · x delete held · Esc close"
            } else {
                "Esc close"
            })
            .to_owned(),
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent, snapshot: &ViewSnapshot) -> Effect {
        if key.code == KeyCode::Char('x') && key.mods == Modifiers::NONE {
            return self
                .list
                .selected_id()
                .map_or(Effect::None, |input_id| Effect::InputControl {
                    input_id,
                    action: "delete",
                });
        }
        let effect = match self.list.handle_key(key) {
            Event::Pick { id: input_id, .. } => Effect::InputControl {
                input_id,
                action: "steer",
            },
            Event::Close => Effect::Close,
            Event::None | Event::Refresh => Effect::None,
        };
        self.set_hint(snapshot);
        effect
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        self.list.cursor(width)
    }

    pub fn render(&self, width: usize, rows: usize, palette: &Palette) -> Vec<String> {
        self.list.render(width, rows, palette)
    }
}
