//! The `/models` picker (port of `ModelPanel` and `buildModelRows` for the
//! models a daemon offers; "Edit models" is not built yet), and its
//! "Subagent models" action when the daemon manages that policy:
//!
//! ```text
//! Select a model
//! > Provider / model          ZDR  ‹ high ›  current
//!   Other provider / model    low
//!   Edit models
//!   Subagent models: Agent choice
//! Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close
//!
//! Subagent models
//!   Agent choice (default)
//!   Inherit requesting branch
//!   Fixed model > the model list again
//!   Applies to new members, including named Agents.
//!   Existing members keep their models.
//! ```
//!
//! ←/→ cycles the highlighted model's advertised reasoning efforts and wraps.
//! The choice stays on that row while the list keeps it: a replaced list — one
//! read after the catalog changed included — keeps the choices whose model is
//! still listed and still advertises that effort, and drops the others; Ctrl+X
//! drops the highlighted row's choice with its saved preferences. Enter adopts
//! the model; a cycled effort is saved as its `reasoningEffort` and sent with
//! the model's other saved preferences. A model with no advertised effort
//! ignores the arrows. Typing in the filter keeps ←/→ for the cursor.
//!
//! Choosing a policy writes only `collaboration.set_model_policy`; the leader's
//! model and the provider default are untouched.

use std::collections::BTreeMap;

use crate::model::session::{
    EmptyProvider, EmptyReason, ModelItem, ModelList, ModelPolicyUpdate, SubagentModelPolicy,
};
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use dim_agent_types::ModelId;

const HINT: &str = "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close";
const POLICY_HINT: &str = "Enter select · Esc back";
const EDIT_MODELS: &str = "__edit_models__";
const SUBAGENT_MODELS: &str = "__subagent_models__";
const SECTION: &str = "Models";
const AGENT: &str = "agent";
const INHERIT: &str = "inherit";
const FIXED: &str = "fixed";

#[derive(Clone, Copy, PartialEq)]
enum View {
    Models,
    Policy,
    /// The model list again, choosing the fixed subagent model.
    Fixed,
}

/// The policy as last read; `revision` is `None` after a conflict or
/// failure, when a choice rereads instead of saving.
struct Policy {
    revision: Option<String>,
    current: Option<SubagentModelPolicy>,
}

pub struct ModelsPanel {
    view: View,
    list: ListPanel,
    listed: Option<Result<ModelList, String>>,
    /// `None` while the daemon does not manage the policy: no action.
    policy: Option<Policy>,
    /// Effort ←/→ chose for `provider::model`, kept across list replacements
    /// while the row is listed and advertises it.
    chosen: BTreeMap<String, String>,
}

impl ModelsPanel {
    pub fn new() -> Self {
        Self {
            view: View::Models,
            list: list(View::Models),
            listed: None,
            policy: None,
            chosen: BTreeMap::new(),
        }
    }

    pub fn set_models(&mut self, models: Result<ModelList, String>) {
        // A replaced list keeps the choices its rows still offer: a cycled
        // effort survives a later read, while a row that is gone, or no longer
        // advertises it, drops it.
        self.chosen.retain(|id, effort| {
            models.as_ref().is_ok_and(|list| {
                list.models.iter().any(|model| {
                    key(&model.provider_id, &model.model_id) == *id
                        && model.efforts.iter().any(|advertised| advertised == effort)
                })
            })
        });
        self.listed = Some(models);
        self.rebuild(None);
    }

    /// Returns the notice the update calls for.
    pub fn set_policy(&mut self, update: ModelPolicyUpdate) -> Option<String> {
        let saved = matches!(update, ModelPolicyUpdate::Read { saved: true, .. });
        let policy = self.policy.get_or_insert(Policy {
            revision: None,
            current: None,
        });
        let notice = match update {
            ModelPolicyUpdate::Read {
                revision,
                policy: current,
                ..
            } => {
                policy.revision = Some(revision);
                policy.current = Some(current);
                saved.then(|| t("Subagent model policy saved").to_owned())
            }
            ModelPolicyUpdate::Conflict => {
                policy.revision = None;
                Some(
                    t("Subagent model policy changed elsewhere; choose again to reread it")
                        .to_owned(),
                )
            }
            ModelPolicyUpdate::Failed(error) => {
                policy.revision = None;
                Some(tf("Subagent model policy: {error}", &[("error", &error)]))
            }
        };
        if saved {
            self.open(View::Models, Some(SUBAGENT_MODELS));
        } else {
            self.rebuild(None);
        }
        notice
    }

    fn current(&self) -> Option<&SubagentModelPolicy> {
        self.policy.as_ref()?.current.as_ref()
    }

    fn open(&mut self, view: View, preselect: Option<&str>) {
        self.view = view;
        self.list = list(view);
        self.rebuild(preselect);
    }

    /// Fills the list of the current view, keeping its selection.
    fn rebuild(&mut self, preselect: Option<&str>) {
        let selected = self.list.selected_id();
        let mode = match self.current() {
            Some(SubagentModelPolicy::Agent) => Some(AGENT),
            Some(SubagentModelPolicy::Inherit) => Some(INHERIT),
            Some(SubagentModelPolicy::Fixed { .. }) => Some(FIXED),
            None => None,
        };
        let marked = match (self.view, self.current()) {
            (View::Models, _) => self
                .listed
                .as_ref()
                .and_then(|listed| listed.as_ref().ok()?.current.as_ref())
                .map(|(p, m)| key(p, m)),
            (View::Fixed, Some(SubagentModelPolicy::Fixed { model })) => {
                Some(key(&model.provider_id, &model.model_id))
            }
            (View::Fixed, _) => None,
            (View::Policy, _) => mode.map(str::to_owned),
        };
        let focus = preselect
            .map(str::to_owned)
            .or_else(|| selected.clone())
            .or_else(|| marked.clone());
        let show_effort = self.view == View::Models;
        let items = match self.view {
            View::Policy => Some(Ok(self.policy_rows(mode))),
            View::Models | View::Fixed => self.listed.as_ref().map(|listed| {
                listed.clone().map(|list| {
                    let mut items = model_rows(
                        &list.models,
                        marked.as_deref(),
                        show_effort.then_some(focus.as_deref()).flatten(),
                        show_effort.then_some(&self.chosen),
                    );
                    items.extend(empty_rows(&list.empty));
                    if self.view == View::Models {
                        items.push(Item::new(EDIT_MODELS, t("Edit models"), ""));
                        if let Some(policy) = &self.policy {
                            let label = self.label(policy.current.as_ref());
                            items.push(Item::new(
                                SUBAGENT_MODELS,
                                tf("Subagent models: {policy}", &[("policy", &label)]),
                                "",
                            ));
                        }
                    }
                    items
                })
            }),
        };
        if let Some(items) = items {
            let preselect = preselect.or(selected.as_deref()).or(marked.as_deref());
            self.list.set_items(items, preselect);
        }
    }

    fn policy_rows(&self, mode: Option<&str>) -> Vec<Item> {
        let detail = |id| if mode == Some(id) { t("current") } else { "" };
        let mut fixed = Item::new(FIXED, t("Fixed model"), detail(FIXED));
        if let Some(SubagentModelPolicy::Fixed { model }) = self.current() {
            fixed.detail = format!("{}  {}", self.model_name(model), t("current"));
        }
        vec![
            Item::new(AGENT, t("Agent choice (default)"), detail(AGENT)),
            Item::new(INHERIT, t("Inherit requesting branch"), detail(INHERIT)),
            fixed,
            Item::heading(""),
            Item::heading(t("Applies to new members, including named Agents.")),
            Item::heading(t("Existing members keep their models.")),
        ]
    }

    fn models(&self) -> &[ModelItem] {
        match &self.listed {
            Some(Ok(list)) => &list.models,
            _ => &[],
        }
    }

    fn model_name(&self, model: &ModelId) -> String {
        let provider = self
            .models()
            .iter()
            .find(|item| item.provider_id == model.provider_id)
            .map_or(model.provider_id.as_str(), provider_name);
        format!("{provider} / {}", model.model_id)
    }

    fn label(&self, policy: Option<&SubagentModelPolicy>) -> String {
        match policy {
            Some(SubagentModelPolicy::Agent) => t("Agent choice").to_owned(),
            Some(SubagentModelPolicy::Inherit) => t("Inherit requesting branch").to_owned(),
            Some(SubagentModelPolicy::Fixed { model }) => self.model_name(model),
            None => t("unknown").to_owned(),
        }
    }

    /// Saves `policy`, or rereads first when the revision is not known.
    fn choose(&self, policy: SubagentModelPolicy) -> Effect {
        match self.policy.as_ref().and_then(|p| p.revision.clone()) {
            Some(expected_revision) => Effect::SetModelPolicy {
                expected_revision,
                policy,
            },
            None => Effect::ReadModelPolicy,
        }
    }

    /// A paste goes into the filter.
    pub fn paste(&mut self, text: &str) {
        self.list.paste(text);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.view == View::Models
            && key.mods.0 == 0
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
            && self.list.filter_is_empty()
        {
            return self.cycle(key.code == KeyCode::Right);
        }
        if self.view == View::Models
            && key.code == KeyCode::Char('x')
            && key.mods.contains(Modifiers::CTRL)
        {
            // Clearing the saved preferences drops the row's cycled effort
            // too: the notice asks for the model to be selected again so it
            // adopts its defaults, and that only holds if the cycle is gone.
            let Some(id) = self.list.selected_id() else {
                return Effect::None;
            };
            self.chosen.remove(&id);
            self.rebuild(None);
            return self
                .models()
                .iter()
                .find(|model| id == self::key(&model.provider_id, &model.model_id))
                .map(|model| Effect::ClearModelPreferences {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    expected_version: model.provider_version,
                })
                .unwrap_or(Effect::None);
        }
        let before = self.list.selected_id();
        let event = self.list.handle_key(key);
        if self.view == View::Models && event == Event::None && self.list.selected_id() != before {
            self.rebuild(None);
        }
        match (self.view, event) {
            (_, Event::None | Event::Refresh) => Effect::None,
            (View::Models, Event::Close) => Effect::Close,
            (View::Policy, Event::Close) => {
                self.open(View::Models, Some(SUBAGENT_MODELS));
                Effect::None
            }
            (View::Fixed, Event::Close) => {
                self.open(View::Policy, Some(FIXED));
                Effect::None
            }
            (View::Models, Event::Pick(id)) if id == EDIT_MODELS => {
                Effect::Notice(t("Editing models is not available yet").to_owned())
            }
            (View::Models, Event::Pick(id)) if id == SUBAGENT_MODELS => {
                self.open(View::Policy, None);
                Effect::None
            }
            (View::Policy, Event::Pick(id)) if id == FIXED => {
                self.open(View::Fixed, None);
                Effect::None
            }
            (View::Policy, Event::Pick(id)) if id == INHERIT => {
                self.choose(SubagentModelPolicy::Inherit)
            }
            (View::Policy, Event::Pick(_)) => self.choose(SubagentModelPolicy::Agent),
            (view, Event::Pick(id)) => match id.split_once("::") {
                Some((provider, model)) if view == View::Fixed => {
                    self.choose(SubagentModelPolicy::Fixed {
                        model: ModelId {
                            provider_id: provider.to_owned(),
                            model_id: model.to_owned(),
                        },
                    })
                }
                Some((provider, model)) => Effect::SelectModel {
                    provider_id: provider.to_owned(),
                    model_id: model.to_owned(),
                    reasoning_effort: self.chosen.get(&id).cloned(),
                },
                None => Effect::None,
            },
        }
    }

    fn cycle(&mut self, forward: bool) -> Effect {
        let Some(id) = self.list.selected_id() else {
            return Effect::None;
        };
        let effort = {
            let Some(model) = self
                .models()
                .iter()
                .find(|model| key(&model.provider_id, &model.model_id) == id)
            else {
                return Effect::None;
            };
            if model.efforts.is_empty() {
                return Effect::None;
            }
            let current = self
                .chosen
                .get(&id)
                .map(String::as_str)
                .or(model.saved_effort.as_deref())
                .or(model.default_effort.as_deref());
            let index = current.and_then(|value| model.efforts.iter().position(|e| e == value));
            let len = model.efforts.len();
            let next = match index {
                Some(index) if forward => (index + 1) % len,
                Some(index) => (index + len - 1) % len,
                None if forward => 0,
                None => len - 1,
            };
            model.efforts[next].clone()
        };
        self.chosen.insert(id, effort);
        self.rebuild(None);
        Effect::None
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        self.list.cursor(width)
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}

fn list(view: View) -> ListPanel {
    let models = |title, hint| {
        ListPanel::new(
            title,
            hint,
            "Loading models...",
            "No models available. Connect a provider first.",
            "No matching models.",
            false,
        )
    };
    match view {
        View::Models => models("Select a model", HINT),
        View::Fixed => models("Fixed subagent model", POLICY_HINT),
        View::Policy => {
            ListPanel::new("Subagent models", POLICY_HINT, "", "", "", false).without_filter()
        }
    }
}

fn model_rows(
    models: &[ModelItem],
    marked: Option<&str>,
    focus: Option<&str>,
    chosen: Option<&BTreeMap<String, String>>,
) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    if !models.is_empty() {
        items.push(Item::heading(t(SECTION)));
    }
    for model in models {
        let id = key(&model.provider_id, &model.model_id);
        let mut row = Item::new(
            id.clone(),
            format!("{} / {}", provider_name(model), model.model_id),
            effort_detail(
                model,
                chosen
                    .and_then(|chosen| chosen.get(&id))
                    .map(String::as_str),
                Some(id.as_str()) == focus,
                Some(id.as_str()) == marked,
            ),
        );
        row.search = model.display_name.clone();
        if model.zdr {
            row.detail = if row.detail.is_empty() {
                "ZDR".to_owned()
            } else {
                format!("ZDR  {}", row.detail)
            };
            row.search.push_str(" ZDR");
        }
        items.push(row);
    }
    items
}

fn effort_detail(model: &ModelItem, chosen: Option<&str>, focused: bool, current: bool) -> String {
    let shown = chosen
        .or(model.saved_effort.as_deref())
        .or(model.default_effort.as_deref());
    let effort = if model.efforts.is_empty() {
        String::new()
    } else if focused {
        match shown {
            Some(value) => format!("‹ {value} ›"),
            None => "‹ — ›".to_owned(),
        }
    } else {
        shown.unwrap_or("").to_owned()
    };
    match (effort.is_empty(), current) {
        (true, true) => t("current").to_owned(),
        (false, true) => format!("{effort}  {}", t("current")),
        (false, false) => effort,
        (true, false) => String::new(),
    }
}

/// A provider that offers nothing selectable keeps a muted row, after the
/// models, naming it, why and the command that shows the details.
fn empty_rows(providers: &[EmptyProvider]) -> Vec<Item> {
    providers
        .iter()
        .map(|provider| {
            let name = ("provider", provider.provider_name.as_str());
            let id = ("id", provider.provider_id.as_str());
            let text = match provider.reason {
                EmptyReason::NoModels { listable: true } => tf(
                    "{provider}: no models · dimcode models {id} --refresh",
                    &[name, id],
                ),
                EmptyReason::NoModels { listable: false } => tf(
                    "{provider}: no models · add a model with provider.models.create",
                    &[name],
                ),
                EmptyReason::Mixed {
                    unavailable,
                    disabled,
                } => {
                    let unavailable = unavailable.to_string();
                    let disabled = disabled.to_string();
                    tf(
                        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}",
                        &[name, ("unavailable", &unavailable), ("disabled", &disabled), id],
                    )
                }
                EmptyReason::Unavailable { count: 1 } => tf(
                    "{provider}: 1 model unavailable · dimcode models {id}",
                    &[name, id],
                ),
                EmptyReason::Unavailable { count } => {
                    let count = count.to_string();
                    tf(
                        "{provider}: {count} models unavailable · dimcode models {id}",
                        &[name, ("count", &count), id],
                    )
                }
                EmptyReason::Disabled { count: 1 } => tf(
                    "{provider}: 1 model disabled · dimcode models {id}",
                    &[name, id],
                ),
                EmptyReason::Disabled { count } => {
                    let count = count.to_string();
                    tf(
                        "{provider}: {count} models disabled · dimcode models {id}",
                        &[name, ("count", &count), id],
                    )
                }
                EmptyReason::Unreadable => tf(
                    "{provider}: catalog unreadable · dimcode models {id}",
                    &[name, id],
                ),
            };
            let mut row = Item::heading(text);
            row.sticky = true;
            row
        })
        .collect()
}

fn provider_name(model: &ModelItem) -> &str {
    if model.provider_name.is_empty() {
        model.provider_id.as_str()
    } else {
        model.provider_name.as_str()
    }
}

fn key(provider: &str, model: &str) -> String {
    format!("{provider}::{model}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::terminal::Modifiers;
    use crate::ui::theme::{Appearance, ColorMode};

    fn key_of(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn item(
        id: &str,
        efforts: &[&str],
        saved: Option<&str>,
        default_effort: Option<&str>,
    ) -> ModelItem {
        item_for("dim", "Dim OAuth", id, efforts, saved, default_effort)
    }

    fn item_for(
        provider: &str,
        provider_name: &str,
        id: &str,
        efforts: &[&str],
        saved: Option<&str>,
        default_effort: Option<&str>,
    ) -> ModelItem {
        ModelItem {
            provider_id: provider.into(),
            provider_name: provider_name.into(),
            provider_version: 1,
            model_id: id.into(),
            display_name: id.into(),
            zdr: false,
            efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            default_effort: default_effort.map(str::to_owned),
            saved_effort: saved.map(str::to_owned),
        }
    }

    fn panel() -> ModelsPanel {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            models: vec![
                item("flash", &["low", "high"], Some("low"), Some("low")),
                item("plain", &[], None, None),
            ],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
        }));
        panel
    }

    fn drawn(panel: &ModelsPanel) -> String {
        panel
            .render(
                80,
                16,
                &crate::ui::theme::palette::terminal_palette(
                    ColorMode::TrueColor,
                    Appearance::Dark,
                ),
            )
            .join("\n")
    }

    #[test]
    fn zdr_tags_filter_select_and_follow_catalog_replacement() {
        let mut panel = panel();
        let mut models = panel.models().to_vec();
        models[0].zdr = true;
        panel.set_models(Ok(ModelList {
            models: models.clone(),
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("ZDR  ‹ low ›"), "{screen}");
        panel.paste("zdr");
        let screen = drawn(&panel);
        assert!(screen.contains("flash"), "{screen}");
        assert!(!screen.contains("plain"), "{screen}");
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel { ref model_id, .. } if model_id == "flash"
        ));
        models[0].zdr = false;
        panel.set_models(Ok(ModelList {
            models,
            current: None,
            empty: Vec::new(),
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("No matching models."), "{screen}");
        assert!(!screen.contains("ZDR"), "{screen}");
    }

    #[test]
    fn arrows_cycle_the_highlighted_models_effort_and_enter_adopts_it() {
        let mut panel = panel();
        let screen = drawn(&panel);
        assert!(screen.contains("‹ low ›"), "{screen}");
        assert!(screen.contains("←/→"), "{screen}");
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(drawn(&panel).contains("‹ high ›"));
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(drawn(&panel).contains("‹ low ›"));
        assert_eq!(panel.handle_key(key_of(KeyCode::Left)), Effect::None);
        assert!(drawn(&panel).contains("‹ high ›"));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                ref provider_id,
                ref model_id,
                ref reasoning_effort,
            } if provider_id == "dim" && model_id == "flash" && reasoning_effort.as_deref() == Some("high")
        ));
    }

    #[test]
    fn a_model_without_effort_ignores_arrows_and_enter_keeps_saved_preferences() {
        let mut panel = panel();
        assert_eq!(panel.handle_key(key_of(KeyCode::Down)), Effect::None);
        assert!(!drawn(&panel).contains('‹'), "{}", drawn(&panel));
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                ref model_id,
                reasoning_effort: None,
                ..
            } if model_id == "plain"
        ));
    }

    #[test]
    fn the_filter_keeps_left_and_right_for_the_cursor() {
        let mut panel = panel();
        panel.handle_key(key_of(KeyCode::Char('f')));
        assert_eq!(panel.handle_key(key_of(KeyCode::Left)), Effect::None);
        assert!(drawn(&panel).contains("‹ low ›"), "{}", drawn(&panel));
        assert!(drawn(&panel).contains("> f"));
    }

    /// A re-read replaces the list while the picker is open; the effort the
    /// user cycled on a row that is still listed survives it.
    #[test]
    fn a_replaced_list_keeps_a_choice_its_row_still_offers() {
        let mut panel = panel();
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(drawn(&panel).contains("‹ high ›"));
        panel.set_models(Ok(ModelList {
            models: vec![
                item("flash", &["low", "high"], Some("low"), Some("low")),
                item("plain", &[], None, None),
                item_for(
                    "other",
                    "Other",
                    "model-x",
                    &["low", "high"],
                    None,
                    Some("low"),
                ),
            ],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
        }));
        assert!(drawn(&panel).contains("‹ high ›"), "{}", drawn(&panel));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                ref model_id,
                ref reasoning_effort,
                ..
            } if model_id == "flash" && reasoning_effort.as_deref() == Some("high")
        ));
    }

    /// A row whose model no longer advertises the chosen effort drops it.
    #[test]
    fn a_replaced_list_drops_a_choice_the_model_no_longer_offers() {
        let mut panel = panel();
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(drawn(&panel).contains("‹ high ›"));
        panel.set_models(Ok(ModelList {
            models: vec![item("flash", &["low"], Some("low"), Some("low"))],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
        }));
        assert!(drawn(&panel).contains("‹ low ›"), "{}", drawn(&panel));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                reasoning_effort: None,
                ..
            }
        ));
    }

    /// Ctrl+X drops the row's cycled effort, so the model adopts its defaults
    /// when it is selected again after the re-read.
    #[test]
    fn clearing_saved_preferences_drops_the_cycled_effort() {
        let mut panel = panel();
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(drawn(&panel).contains("‹ high ›"));
        assert!(matches!(
            panel.handle_key(KeyEvent::new(KeyCode::Char('x'), Modifiers::CTRL)),
            Effect::ClearModelPreferences { ref model_id, .. } if model_id == "flash"
        ));
        assert!(drawn(&panel).contains("‹ low ›"), "{}", drawn(&panel));
        panel.set_models(Ok(ModelList {
            models: vec![
                item("flash", &["low", "high"], None, Some("low")),
                item("plain", &[], None, None),
            ],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
        }));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                reasoning_effort: None,
                ..
            }
        ));
    }
}
