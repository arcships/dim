//! The `/models` picker (port of `ModelPanel` and `buildModelRows` for the
//! models a daemon offers; reasoning-effort levels and "Edit models" are not
//! built yet).

use crate::list_panel::{Event, Item, ListPanel};
use crate::prompts::Effect;
use crate::session_host::ModelList;
use crate::terminal::KeyEvent;
use crate::theme::Palette;

const HINT: &str = "type filter · enter select · esc clear/close";
const EDIT_MODELS: &str = "__edit_models__";
const SECTION: &str = "Models";

pub struct ModelsPanel {
    list: ListPanel,
    /// Row id -> (provider, model): the id is `provider::model`.
    current: Option<String>,
}

impl ModelsPanel {
    pub fn new() -> Self {
        Self {
            list: ListPanel::new(
                "Select a model",
                HINT,
                "Loading models...",
                "No models available. Connect a provider first.",
                "No matching models.",
                false,
            )
            .with_total_count(),
            current: None,
        }
    }

    pub fn set_models(&mut self, models: Result<ModelList, String>) {
        let current = models
            .as_ref()
            .ok()
            .and_then(|m| m.current.as_ref())
            .map(|(p, m)| key(p, m));
        self.current = current.clone();
        let items = models.map(|list| {
            let mut items: Vec<Item> = Vec::new();
            if !list.models.is_empty() {
                items.push(Item::heading(SECTION));
            }
            for model in &list.models {
                let provider = if model.provider_name.is_empty() {
                    model.provider_id.as_str()
                } else {
                    model.provider_name.as_str()
                };
                let id = key(&model.provider_id, &model.model_id);
                let detail = if Some(&id) == current.as_ref() {
                    "current"
                } else {
                    ""
                };
                let mut row = Item::new(id, format!("{provider} / {}", model.model_id), detail);
                row.search = model.display_name.clone();
                items.push(row);
            }
            let mut edit = Item::new(EDIT_MODELS, "Edit models", "");
            edit.counted = false;
            items.push(edit);
            items
        });
        self.list.set_items(items, current.as_deref());
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match self.list.handle_key(key) {
            Event::None | Event::Refresh => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick(id) if id == EDIT_MODELS => {
                Effect::Notice("Editing models is not available yet".to_owned())
            }
            Event::Pick(id) => match id.split_once("::") {
                Some((provider, model)) => Effect::SelectModel {
                    provider_id: provider.to_owned(),
                    model_id: model.to_owned(),
                },
                None => Effect::None,
            },
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}

fn key(provider: &str, model: &str) -> String {
    format!("{provider}::{model}")
}
