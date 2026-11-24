//! The `/models` dialog: the conversation model candidates, the subagent
//! model policy and the Providers' model management, in three fixed
//! sections. Only the candidate viewport scrolls; the policy and the
//! management entry stay where they are.
//!
//! ```text
//! Models
//! Conversation model
//! > Search models
//! › Dim OAuth / deepseek-v4.1-flash      ‹ high ›  current
//!   Dim OAuth / gpt-6-astra              medium
//!   … more models as the list scrolls …
//! Selected: Dim OAuth / deepseek-v4.1-flash  unidentified
//! Subagent models
//! › Agent choice                                         Enter
//! Model management
//! › Manage models                                 3 providers
//!   4 with missing limits / 27 unavailable
//! Enter select + default · ←/→ effort · Tab section · Esc clear/close
//!
//! Subagent models
//!   Agent choice (default)
//!   Inherit requesting branch
//!   Fixed model > the model list again
//!   Applies to new members, including named Agents.
//!   Existing members keep their models.
//!
//! Manage models
//! › Test              ready
//!   Relay             3 with missing limits
//!   Manual            no models
//! Test: 3 models need a window · dimcode models test --set-missing-limits
//! Enter manage · Esc back
//! ```
//!
//! Tab/Shift+Tab moves between the three sections and Up/Down stays inside
//! the focused one. The search filters the candidates only; the sections
//! stay where they are. Searches and forward scrolling read bounded batches;
//! empty batches are traversed without exposing a navigation row. Escape from a
//! child page returns one level and restores the query, selection and list
//! position; at the root it clears the search first, then closes.
//!
//! ←/→ cycles the highlighted candidate's advertised reasoning efforts and
//! wraps. The choice stays on that row while the list keeps it: a replaced
//! list — one read after the catalog changed included — keeps the choices
//! whose model is still listed and still advertises that effort, and drops
//! the others; Ctrl+X drops the highlighted row's choice with its saved
//! preferences. Enter adopts the model and writes it as the default the next
//! Session in the launch workspace reads; a cycled effort is saved as its
//! `reasoningEffort` and sent with the model's other saved preferences.
//!
//! Enter on the policy section opens the existing Agent choice / Inherit
//! requesting branch / Fixed model flow. Enter on the management section
//! opens the Provider list, whose rows name each Provider and its state;
//! choosing one opens that Provider's existing management area, and closing
//! it returns here. Choosing a policy writes only
//! `collaboration.set_model_policy`; the leader's model and the provider
//! default are untouched.

use std::collections::BTreeMap;

use crate::model::session::{
    Area, EmptyProvider, EmptyReason, ModelItem, ModelList, ModelOutcome, ModelPolicyUpdate,
    NeedsLimitsProvider, SubagentModelPolicy,
};
use crate::ui::i18n::{t, tf};
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{IDLE_MARKER, SELECTED_MARKER, wrap_words};
use crate::ui::rows::title_plain;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::{display_width, truncate_cells};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};
use dim_agent_types::ModelId;

const HINT: &str = "Enter select + default · ←/→ effort · Tab section · Esc clear/close";
const POLICY_SECTION_HINT: &str = "Enter change · Tab section · Esc clear/close";
const MANAGEMENT_SECTION_HINT: &str = "Enter manage · Tab section · Esc clear/close";
const POLICY_HINT: &str = "Enter select · Esc back";
const MANAGE_HINT: &str = "Enter manage · Esc back";
const CONVERSATION: &str = "Conversation model";
const AGENT: &str = "agent";
const INHERIT: &str = "inherit";
const FIXED: &str = "fixed";

#[derive(Clone, Copy, PartialEq)]
enum View {
    Models,
    Policy,
    /// The model list again, choosing the fixed subagent model.
    Fixed,
    /// The Provider list management starts from.
    Manage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Model,
    Policy,
    Management,
}

/// The policy as last read; `revision` is `None` after a conflict or
/// failure, when a choice rereads instead of saving.
struct Policy {
    revision: Option<String>,
    current: Option<SubagentModelPolicy>,
}

pub struct ModelsPanel {
    view: View,
    /// The root's list, kept while a child view is open, so returning
    /// restores its query, selection and window.
    root: ListPanel,
    list: ListPanel,
    section: Section,
    listed: Option<Result<ModelList, String>>,
    /// `None` while the daemon does not manage the policy: no action.
    policy: Option<Policy>,
    /// Effort ←/→ chose for `provider::model`, kept across list replacements
    /// while the row is listed and advertises it.
    chosen: BTreeMap<String, String>,
    pages: Vec<ModelList>,
    refill: usize,
    requesting: bool,
}

impl ModelsPanel {
    pub fn new() -> Self {
        Self {
            view: View::Models,
            root: list(View::Models, Section::Model),
            list: list(View::Models, Section::Model),
            section: Section::Model,
            listed: None,
            policy: None,
            chosen: BTreeMap::new(),
            pages: Vec::new(),
            refill: 0,
            requesting: false,
        }
    }

    fn query(&self) -> &str {
        match self.view {
            View::Models | View::Fixed => self.list.query(),
            View::Policy | View::Manage => self.root.query(),
        }
    }

    pub fn refresh(&mut self) -> Effect {
        self.refill = self.pages.len();
        self.pages.clear();
        self.request(None)
    }

    pub fn page(
        &mut self,
        query: &str,
        after: Option<ModelId>,
        result: Result<Vec<ModelList>, String>,
    ) -> Effect {
        if query != self.query()
            || (after.is_some()
                && after.as_ref() != self.pages.last().and_then(|page| page.next.as_ref()))
        {
            return Effect::None;
        }
        let pages = match result {
            Ok(pages) => pages,
            Err(error) => {
                if after.is_none() || self.refill != 0 || self.models().is_empty() {
                    self.pages.clear();
                    self.set_models(Err(error.clone()));
                }
                self.refill = 0;
                self.requesting = false;
                return Effect::Notice(error);
            }
        };
        if after.is_none() {
            self.pages.clear();
        }
        self.pages.extend(pages);
        let last = self.pages.last().unwrap();
        let empty = last.models.is_empty();
        let next = last.next.clone();
        if self.pages.len() < self.refill
            && let Some(next) = next
        {
            return self.request(Some(next));
        }
        self.refill = 0;
        let mut combined = self.pages[0].clone();
        for page in &self.pages[1..] {
            combined.models.extend(page.models.iter().cloned());
            combined.empty.extend(page.empty.iter().cloned());
            combined.limits.extend(page.limits.iter().cloned());
            combined.next.clone_from(&page.next);
        }
        self.set_models(Ok(combined));
        if let Some(next) = next
            && empty
        {
            if self.models().is_empty() && matches!(self.view, View::Models | View::Fixed) {
                self.list.loading();
            }
            return self.request(Some(next));
        }
        Effect::None
    }

    fn set_models(&mut self, models: Result<ModelList, String>) {
        if models.as_ref().is_ok_and(|page| page.query != self.query()) {
            return;
        }
        if !self.requesting && models.is_err() && self.listed.as_ref().is_some_and(Result::is_ok) {
            return;
        }
        self.requesting = false;
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
        let manages = models.as_ref().is_ok_and(|list| list.manage);
        self.listed = Some(models);
        if self.view == View::Manage && !manages {
            self.back_to_root(Section::Management);
            return;
        }
        self.normalize_section();
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
            self.back_to_root(Section::Policy);
        } else {
            self.rebuild(None);
        }
        notice
    }

    fn current(&self) -> Option<&SubagentModelPolicy> {
        self.policy.as_ref()?.current.as_ref()
    }

    /// Opens a child view, keeping the root's list for the way back.
    fn open_child(&mut self, view: View, preselect: Option<&str>) {
        if self.view == View::Models {
            std::mem::swap(&mut self.list, &mut self.root);
        }
        self.view = view;
        self.list = list(view, self.section);
        if matches!(view, View::Models | View::Fixed)
            && let Some(Ok(page)) = &self.listed
        {
            self.list.paste(&page.query);
        }
        self.rebuild(preselect);
    }

    /// Returns to the root, with the list and position it had, focused on
    /// `section` when that section is still there.
    fn back_to_root(&mut self, section: Section) {
        if self.view != View::Models {
            std::mem::swap(&mut self.list, &mut self.root);
            self.view = View::Models;
        }
        self.section = section;
        self.normalize_section();
        self.sync_hint();
        self.rebuild(None);
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
            (View::Manage, _) => None,
        };
        let focus = preselect
            .map(str::to_owned)
            .or_else(|| selected.clone())
            .or_else(|| marked.clone());
        let effort = self.view == View::Models;
        let items = match self.view {
            View::Policy => Some(Ok(self.policy_rows(mode))),
            View::Models | View::Fixed => self.listed.as_ref().map(|listed| {
                listed.as_ref().map_err(Clone::clone).map(|list| {
                    model_rows(
                        &list.models,
                        marked.as_deref(),
                        if effort { focus.as_deref() } else { None },
                        if effort { Some(&self.chosen) } else { None },
                    )
                })
            }),
            View::Manage => self
                .listed
                .as_ref()
                .map(|listed| listed.as_ref().map_err(Clone::clone).map(provider_rows)),
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

    /// Whether the daemon manages Providers, so the management section offers
    /// its entry.
    fn manages(&self) -> bool {
        matches!(&self.listed, Some(Ok(list)) if list.manage)
    }

    /// The sections this panel draws, in order.
    fn sections(&self) -> Vec<Section> {
        let mut sections = vec![Section::Model];
        if self.policy.is_some() {
            sections.push(Section::Policy);
        }
        if self.manages() {
            sections.push(Section::Management);
        }
        sections
    }

    fn normalize_section(&mut self) {
        if !self.sections().contains(&self.section) {
            self.section = Section::Model;
        }
    }

    fn sync_hint(&mut self) {
        if self.view == View::Models {
            self.list.set_hint(t(hint(self.section)).to_owned());
        }
    }

    fn focus_next(&mut self, forward: bool) {
        let sections = self.sections();
        let at = sections
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % sections.len()
        } else {
            (at + sections.len() - 1) % sections.len()
        };
        self.section = sections[next];
        self.sync_hint();
    }

    /// The candidate the current filter shows under the selection.
    fn selected_model(&self) -> Option<&ModelItem> {
        if self.view != View::Models {
            return None;
        }
        let id = self.list.selected_id()?;
        self.models()
            .iter()
            .find(|model| key(&model.provider_id, &model.model_id) == id)
    }

    fn selected_provider(&self) -> Option<Provider> {
        if self.view != View::Manage {
            return None;
        }
        let list = match &self.listed {
            Some(Ok(list)) => list,
            _ => return None,
        };
        let id = self.list.selected_id()?;
        providers(list)
            .into_iter()
            .find(|p| provider_key(&p.id) == id)
    }

    /// The selected candidate in words: what it is and how its id was
    /// identified. The row itself shows the effort and the current marker.
    fn summary(&self, model: &ModelItem) -> String {
        let mut line = format!("{} / {}", provider_name(model), model.model_id);
        let outcome = outcome_text(&model.outcome);
        if !outcome.is_empty() {
            line.push_str("  ");
            line.push_str(&outcome);
        }
        tf("Selected: {model}", &[("model", &line)])
    }

    /// One line per condition the candidate list no longer carries: a
    /// Provider's missing limits, and why it offers nothing selectable.
    fn diagnostics(&self) -> Vec<String> {
        let list = match &self.listed {
            Some(Ok(list)) => list,
            _ => return Vec::new(),
        };
        let mut missing = 0;
        let mut unavailable = 0;
        let mut disabled = 0;
        let mut empty = 0;
        let mut unreadable = 0;
        for limits in &list.limits {
            missing += limits.count;
        }
        for provider in &list.empty {
            match provider.reason {
                EmptyReason::NoModels { .. } => empty += 1,
                EmptyReason::Mixed {
                    unavailable: u,
                    disabled: d,
                } => {
                    unavailable += u;
                    disabled += d;
                }
                EmptyReason::Unavailable { count } => unavailable += count,
                EmptyReason::Disabled { count } => disabled += count,
                EmptyReason::Unreadable => unreadable += 1,
            }
        }
        let count = |value: usize| value.to_string();
        let mut parts = Vec::new();
        if missing > 0 {
            let value = count(missing);
            parts.push(tf("{count} with missing limits", &[("count", &value)]));
        }
        if unavailable > 0 {
            let value = count(unavailable);
            parts.push(tf("{count} unavailable", &[("count", &value)]));
        }
        if disabled > 0 {
            let value = count(disabled);
            parts.push(tf("{count} disabled", &[("count", &value)]));
        }
        if empty > 0 {
            let value = count(empty);
            parts.push(tf("{count} without models", &[("count", &value)]));
        }
        if unreadable > 0 {
            let value = count(unreadable);
            parts.push(tf("{count} unreadable", &[("count", &value)]));
        }
        parts
    }

    /// The recovery guidance of every Provider, for a daemon that offers no
    /// management actions.
    fn management_guidance(&self) -> Vec<String> {
        let list = match &self.listed {
            Some(Ok(list)) => list,
            _ => return Vec::new(),
        };
        providers(list)
            .into_iter()
            .flat_map(|provider| provider.guidance())
            .collect()
    }

    fn footer(&self, inner: usize, rows: usize, p: &Palette) -> Vec<String> {
        if rows < 12 {
            return self.compact_footer(inner, p);
        }
        let mut lines = Vec::new();
        if let Some(model) = self.selected_model() {
            let mut summary = self.summary(model);
            if model.provider_id == crate::login::CHATGPT_PROVIDER_ID {
                summary = format!("{} · {summary}", t("Using ChatGPT plan"));
            }
            lines.push(fg(&truncate_cells(&summary, inner), &p.muted));
        }
        if let Some(policy) = &self.policy {
            lines.push(title_plain(t("Subagent models"), inner, p));
            let label = self.label(policy.current.as_ref());
            lines.push(action_row(
                &label,
                "Enter",
                self.section == Section::Policy,
                inner,
                p,
            ));
        }
        let guidance = self.management_guidance();
        if self.manages() || !guidance.is_empty() {
            lines.push(title_plain(t("Model management"), inner, p));
            if self.manages() {
                let count = match &self.listed {
                    Some(Ok(list)) => providers(list).len(),
                    _ => 0,
                };
                let detail = if count == 1 {
                    t("1 provider").to_owned()
                } else {
                    let count = count.to_string();
                    tf("{count} providers", &[("count", &count)])
                };
                lines.push(action_row(
                    t("Manage models"),
                    &detail,
                    self.section == Section::Management,
                    inner,
                    p,
                ));
                let parts = self.diagnostics();
                if !parts.is_empty() {
                    for row in wrap_words(&parts.join(" / "), inner.saturating_sub(2).max(1)) {
                        lines.push(format!("  {}", fg(&row, &p.muted)));
                    }
                }
            } else {
                for line in guidance {
                    for row in wrap_words(&line, inner.saturating_sub(2).max(1)) {
                        lines.push(format!("  {}", fg(&row, &p.muted)));
                    }
                }
            }
        }
        lines
    }

    /// The footer a terminal too short for the section titles keeps: the
    /// diagnostics, then the entry of the focused section last, so the
    /// section the user is in survives the row budget.
    fn compact_footer(&self, inner: usize, p: &Palette) -> Vec<String> {
        let mut lines = Vec::new();
        if self.manages() {
            let parts = self.diagnostics();
            if !parts.is_empty() {
                for row in wrap_words(&parts.join(" / "), inner.saturating_sub(2).max(1)) {
                    lines.push(format!("  {}", fg(&row, &p.muted)));
                }
            }
        } else {
            for line in self.management_guidance() {
                for row in wrap_words(&line, inner.saturating_sub(2).max(1)) {
                    lines.push(format!("  {}", fg(&row, &p.muted)));
                }
            }
        }
        match self.section {
            Section::Policy => {
                if let Some(policy) = &self.policy {
                    let label = self.label(policy.current.as_ref());
                    lines.push(action_row(&label, "Enter", true, inner, p));
                }
            }
            Section::Management if self.manages() => {
                let count = match &self.listed {
                    Some(Ok(list)) => providers(list).len(),
                    _ => 0,
                };
                let detail = if count == 1 {
                    t("1 provider").to_owned()
                } else {
                    let count = count.to_string();
                    tf("{count} providers", &[("count", &count)])
                };
                lines.push(action_row(t("Manage models"), &detail, true, inner, p));
            }
            _ => {}
        }
        lines
    }
    fn choose(&self, policy: SubagentModelPolicy) -> Effect {
        match self.policy.as_ref().and_then(|p| p.revision.clone()) {
            Some(expected_revision) => Effect::SetModelPolicy {
                expected_revision,
                policy,
            },
            None => Effect::ReadModelPolicy,
        }
    }

    fn request(&mut self, after: Option<ModelId>) -> Effect {
        self.requesting = true;
        Effect::QueryModels {
            query: self.query().to_owned(),
            after,
        }
    }

    fn search(&mut self) -> Effect {
        self.pages.clear();
        self.refill = 0;
        self.list.loading();
        self.request(None)
    }

    /// A paste goes into the filter.
    pub fn paste(&mut self, text: &str) -> Effect {
        if self.view == View::Models {
            self.section = Section::Model;
            self.sync_hint();
        }
        self.list.paste(text);
        if !matches!(self.view, View::Models | View::Fixed) {
            return Effect::None;
        }
        self.search()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.view == View::Models {
            match key.code {
                KeyCode::Tab => {
                    self.focus_next(true);
                    return Effect::None;
                }
                KeyCode::BackTab => {
                    self.focus_next(false);
                    return Effect::None;
                }
                KeyCode::Enter if self.section != Section::Model => {
                    return match self.section {
                        Section::Policy => {
                            self.open_child(View::Policy, None);
                            Effect::None
                        }
                        Section::Management if self.manages() => {
                            self.open_child(View::Manage, None);
                            Effect::None
                        }
                        _ => Effect::None,
                    };
                }
                KeyCode::Esc => {}
                _ if self.section != Section::Model => return Effect::None,
                KeyCode::Left | KeyCode::Right if key.mods.0 == 0 => {
                    return self.cycle(key.code == KeyCode::Right);
                }
                KeyCode::Char('x') if key.mods.contains(Modifiers::CTRL) => {
                    return self.clear_preferences();
                }
                _ => {}
            }
        }
        let before = self.list.selected_id();
        let query = self.list.query().to_owned();
        let event = self.list.handle_key(key);
        if matches!(self.view, View::Models | View::Fixed) && query != self.list.query() {
            return self.search();
        }
        if self.view == View::Models
            && self.section == Section::Model
            && event == Event::None
            && self.list.selected_id() != before
        {
            self.rebuild(None);
        }
        if matches!(self.view, View::Models | View::Fixed)
            && matches!(
                key.code,
                KeyCode::Down | KeyCode::PageDown | KeyCode::End | KeyCode::Tab
            )
            && !self.requesting
            && self.list.near_end()
            && let Some(next) = self.pages.last().and_then(|page| page.next.clone())
        {
            return self.request(Some(next));
        }
        self.dispatch(event)
    }

    fn dispatch(&mut self, event: Event) -> Effect {
        match (self.view, event) {
            (_, Event::None | Event::Refresh) => Effect::None,
            (View::Models, Event::Close) => Effect::Close,
            (View::Policy, Event::Close) => {
                self.back_to_root(Section::Policy);
                Effect::None
            }
            (View::Fixed, Event::Close) => {
                let changed = self.list.query() != self.root.query();
                self.open_child(View::Policy, Some(FIXED));
                if changed {
                    self.pages.clear();
                    self.refill = 0;
                    self.request(None)
                } else {
                    Effect::None
                }
            }
            (View::Manage, Event::Close) => {
                self.back_to_root(Section::Management);
                Effect::None
            }
            (View::Policy, Event::Pick { id, .. }) if id == FIXED => {
                self.open_child(View::Fixed, None);
                Effect::None
            }
            (View::Policy, Event::Pick { id, .. }) if id == INHERIT => {
                self.choose(SubagentModelPolicy::Inherit)
            }
            (View::Policy, Event::Pick { .. }) => self.choose(SubagentModelPolicy::Agent),
            (View::Manage, Event::Pick { id, .. }) => id
                .strip_prefix("__provider__::")
                .map(|provider_id| Effect::OpenArea {
                    area: Area::Models {
                        provider_id: provider_id.to_owned(),
                    },
                    note: None,
                })
                .unwrap_or(Effect::None),
            (view, Event::Pick { id, label }) => match id.split_once("::") {
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
                    label,
                },
                None => Effect::None,
            },
        }
    }

    fn clear_preferences(&mut self) -> Effect {
        // Clearing the saved preferences drops the row's cycled effort
        // too: the notice asks for the model to be selected again so it
        // adopts its defaults, and that only holds if the cycle is gone.
        if self.requesting {
            return Effect::None;
        }
        let Some(id) = self.list.selected_id() else {
            return Effect::None;
        };
        let Some(model) = self
            .models()
            .iter()
            .find(|model| key(&model.provider_id, &model.model_id) == id)
        else {
            return Effect::None;
        };
        let effect = Effect::ClearModelPreferences {
            query: self.query().to_owned(),
            batches: self.pages.len().max(1),
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            expected_version: model.provider_version,
        };
        self.requesting = true;
        self.chosen.remove(&id);
        self.rebuild(None);
        effect
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
        if self.view == View::Models && self.section != Section::Model {
            return None;
        }
        self.list.cursor(width)
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        if self.view == View::Manage {
            let mut footer = Vec::new();
            if let Some(provider) = self.selected_provider() {
                for line in provider.guidance() {
                    for row in wrap_words(&line, inner.saturating_sub(2).max(1)) {
                        footer.push(format!("  {}", fg(&row, &p.muted)));
                    }
                }
                if !footer.is_empty() {
                    footer.insert(0, String::new());
                }
            }
            return self.list.render_with_footer(width, rows, p, &footer);
        }
        if self.view == View::Models {
            let footer = self.footer(inner, rows, p);
            return self.list.render_with_footer(width, rows, p, &footer);
        }
        self.list.render(width, rows, p)
    }
}

fn list(view: View, section: Section) -> ListPanel {
    let models = |title, hint| {
        let mut panel = ListPanel::new(
            title,
            t(hint),
            "Loading models...",
            "No models available. Connect a provider first.",
            "No matching models.",
            false,
        );
        panel.remote_filter(true);
        panel
    };
    match view {
        View::Models => models("Models", hint(section)).with_lead(t(CONVERSATION)),
        View::Fixed => models("Fixed subagent model", POLICY_HINT),
        View::Policy => {
            ListPanel::new("Subagent models", t(POLICY_HINT), "", "", "", false).without_filter()
        }
        View::Manage => ListPanel::new(
            "Manage models",
            t(MANAGE_HINT),
            "Loading providers...",
            "No matching providers.",
            "No matching providers.",
            false,
        )
        .with_inline_rows(),
    }
}

fn hint(section: Section) -> &'static str {
    match section {
        Section::Model => HINT,
        Section::Policy => POLICY_SECTION_HINT,
        Section::Management => MANAGEMENT_SECTION_HINT,
    }
}

fn model_rows(
    models: &[ModelItem],
    marked: Option<&str>,
    focus: Option<&str>,
    chosen: Option<&BTreeMap<String, String>>,
) -> Vec<Item> {
    models
        .iter()
        .map(|model| {
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
            row
        })
        .collect()
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
        shown.unwrap_or("—").to_owned()
    };
    match (effort.is_empty(), current) {
        (true, true) => t("current").to_owned(),
        (false, true) => format!("{effort}  {}", t("current")),
        (false, false) => effort,
        (true, false) => String::new(),
    }
}

/// What identification made of one row's id, as the selected model's summary
/// says it.
fn outcome_text(outcome: &ModelOutcome) -> String {
    match outcome {
        ModelOutcome::Identified {
            provider_id,
            model_id,
            inferred: false,
        } => tf(
            "identified: {model} at {provider}",
            &[("model", model_id), ("provider", provider_id)],
        ),
        ModelOutcome::Identified {
            provider_id,
            model_id,
            inferred: true,
        } => tf(
            "inferred from several rows: {model} at {provider}",
            &[("model", model_id), ("provider", provider_id)],
        ),
        ModelOutcome::Ambiguous { candidates } => tf(
            "ambiguous: {candidates}",
            &[("candidates", &candidates.join(", "))],
        ),
        ModelOutcome::Unidentified => t("unidentified").to_owned(),
    }
}

/// One Provider the management list names, from the models it offers and the
/// diagnostics the picker no longer carries.
struct Provider {
    id: String,
    name: String,
    models: usize,
    limits: Option<NeedsLimitsProvider>,
    empty: Option<EmptyProvider>,
}

impl Provider {
    /// Whether it offers a selectable model.
    fn state(&self) -> String {
        let mut parts = Vec::new();
        if self.models > 0 {
            parts.push(t("ready").to_owned());
        }
        if let Some(limits) = &self.limits {
            let count = limits.count.to_string();
            parts.push(tf("{count} with missing limits", &[("count", &count)]));
        }
        if let Some(empty) = &self.empty {
            parts.push(empty_state(&empty.reason));
        }
        parts.join(" · ")
    }

    /// What to do about a Provider no candidate row can be selected from.
    fn guidance(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(limits) = &self.limits {
            lines.push(limits_text(limits));
        }
        if let Some(empty) = &self.empty {
            lines.push(empty_text(empty));
        }
        lines
    }
}

fn provider_key(id: &str) -> String {
    format!("__provider__::{id}")
}

fn providers(list: &ModelList) -> Vec<Provider> {
    let mut providers: Vec<Provider> = Vec::new();
    let slot = |id: &str, name: &str, providers: &mut Vec<Provider>| match providers
        .iter()
        .position(|provider| provider.id == id)
    {
        Some(at) => at,
        None => {
            providers.push(Provider {
                id: id.to_owned(),
                name: if name.is_empty() {
                    id.to_owned()
                } else {
                    name.to_owned()
                },
                models: 0,
                limits: None,
                empty: None,
            });
            providers.len() - 1
        }
    };
    for model in &list.models {
        let at = slot(&model.provider_id, &model.provider_name, &mut providers);
        providers[at].models += 1;
    }
    for limits in &list.limits {
        let at = slot(&limits.provider_id, &limits.provider_name, &mut providers);
        if let Some(total) = &mut providers[at].limits {
            total.count += limits.count;
            for candidate in &limits.candidates {
                if !total.candidates.contains(candidate) {
                    total.candidates.push(candidate.clone());
                }
            }
        } else {
            providers[at].limits = Some(limits.clone());
        }
    }
    let counts = |reason| match reason {
        EmptyReason::Unavailable { count } => (count, 0),
        EmptyReason::Disabled { count } => (0, count),
        EmptyReason::Mixed {
            unavailable,
            disabled,
        } => (unavailable, disabled),
        EmptyReason::NoModels { .. } | EmptyReason::Unreadable => (0, 0),
    };
    for empty in &list.empty {
        let at = slot(&empty.provider_id, &empty.provider_name, &mut providers);
        if let Some(total) = &mut providers[at].empty {
            let (u1, d1) = counts(total.reason);
            let (u2, d2) = counts(empty.reason);
            total.reason = match (u1 + u2, d1 + d2) {
                (0, 0) => empty.reason,
                (0, count) => EmptyReason::Disabled { count },
                (count, 0) => EmptyReason::Unavailable { count },
                (unavailable, disabled) => EmptyReason::Mixed {
                    unavailable,
                    disabled,
                },
            };
        } else {
            providers[at].empty = Some(empty.clone());
        }
    }
    providers
}

fn provider_rows(list: &ModelList) -> Vec<Item> {
    providers(list)
        .into_iter()
        .map(|provider| {
            let mut row = Item::new(
                provider_key(&provider.id),
                provider.name.clone(),
                provider.state(),
            );
            row.search = provider.id;
            row
        })
        .collect()
}

/// A Provider that offers nothing selectable, in the Provider row's words.
fn empty_state(reason: &EmptyReason) -> String {
    match reason {
        EmptyReason::NoModels { .. } => t("no models").to_owned(),
        EmptyReason::Mixed {
            unavailable,
            disabled,
        } => {
            let unavailable = unavailable.to_string();
            let disabled = disabled.to_string();
            tf(
                "{unavailable} unavailable, {disabled} disabled",
                &[("unavailable", &unavailable), ("disabled", &disabled)],
            )
        }
        EmptyReason::Unavailable { count } => {
            let count = count.to_string();
            tf("{count} unavailable", &[("count", &count)])
        }
        EmptyReason::Disabled { count } => {
            let count = count.to_string();
            tf("{count} disabled", &[("count", &count)])
        }
        EmptyReason::Unreadable => t("catalog unreadable").to_owned(),
    }
}

/// A Provider that offers nothing selectable, naming it, why and the command
/// that shows the details.
fn empty_text(provider: &EmptyProvider) -> String {
    let name = ("provider", provider.provider_name.as_str());
    let id = ("id", provider.provider_id.as_str());
    match provider.reason {
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
                &[
                    name,
                    ("unavailable", &unavailable),
                    ("disabled", &disabled),
                    id,
                ],
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
    }
}

/// A Provider whose catalog holds models no source declares a window for,
/// counting them and naming the command that declares both limits.
fn limits_text(provider: &NeedsLimitsProvider) -> String {
    let name = ("provider", provider.provider_name.as_str());
    let id = ("id", provider.provider_id.as_str());
    let mut text = if provider.count == 1 {
        tf(
            "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits",
            &[name, id],
        )
    } else {
        let count = provider.count.to_string();
        tf(
            "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits",
            &[name, ("count", &count), id],
        )
    };
    // The reason and the rows that disagree appear together, so a user
    // choosing what an id stands for reads the candidates here rather than
    // only in the refusal text.
    if !provider.candidates.is_empty() {
        text.push_str(&format!(" · {}", provider.candidates.join(", ")));
    }
    text
}

/// A section's single action row: its label on the left, its detail at the
/// right edge, marked while the section has the focus.
fn action_row(label: &str, detail: &str, focused: bool, inner: usize, p: &Palette) -> String {
    let marker = if focused {
        SELECTED_MARKER
    } else {
        IDLE_MARKER
    };
    let room = inner.saturating_sub(display_width(marker));
    let detail = truncate_cells(detail, room.saturating_sub(2));
    let detail_room = display_width(&detail);
    let label = truncate_cells(label, room.saturating_sub(detail_room + 2));
    let gap = room.saturating_sub(display_width(&label) + detail_room);
    let label = if focused {
        paint(
            &format!("{marker}{label}"),
            Paint {
                fg: Some(&p.accent),
                bold: true,
                ..Paint::default()
            },
        )
    } else {
        fg(&format!("{marker}{label}"), &p.text_soft)
    };
    if detail.is_empty() {
        return label;
    }
    format!("{label}{}{}", " ".repeat(gap), fg(&detail, &p.muted))
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
    use crate::ui::theme::palette::terminal_palette;
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
            outcome: ModelOutcome::Identified {
                provider_id: provider.into(),
                model_id: id.into(),
                inferred: false,
            },
        }
    }

    fn panel() -> ModelsPanel {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![
                item("flash", &["low", "high"], Some("low"), Some("low")),
                item("plain", &[], None, None),
            ],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        panel
    }

    fn managed() -> ModelsPanel {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            manage: true,
            models: vec![
                item_for("dim", "Dim OAuth", "flash", &[], Some("low"), Some("low")),
                item_for("other", "Other", "plain", &[], None, None),
            ],
            current: Some(("dim".into(), "flash".into())),
            empty: vec![EmptyProvider {
                provider_id: "third".into(),
                provider_name: "Third".into(),
                reason: EmptyReason::NoModels { listable: false },
            }],
            limits: vec![NeedsLimitsProvider {
                provider_id: "other".into(),
                provider_name: "Other".into(),
                count: 2,
                candidates: Vec::new(),
            }],
            ..Default::default()
        }));
        panel
    }

    fn policy(panel: &mut ModelsPanel) {
        panel.set_policy(ModelPolicyUpdate::Read {
            revision: "1".into(),
            policy: SubagentModelPolicy::Agent,
            saved: false,
        });
    }

    fn drawn(panel: &ModelsPanel) -> String {
        crate::ui::render::panel::strip_escapes(
            &panel
                .render(
                    80,
                    24,
                    &terminal_palette(ColorMode::TrueColor, Appearance::Dark),
                )
                .join("\n"),
        )
    }

    #[test]
    fn zdr_tags_filter_select_and_follow_catalog_replacement() {
        let mut panel = panel();
        let mut models = panel.models().to_vec();
        models[0].zdr = true;
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: models.clone(),
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("ZDR  ‹ low ›"), "{screen}");
        assert_eq!(
            panel.paste("zdr"),
            Effect::QueryModels {
                query: "zdr".into(),
                after: None
            }
        );
        panel.set_models(Ok(ModelList {
            query: "zdr".into(),
            models: vec![models[0].clone()],
            ..Default::default()
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("flash"), "{screen}");
        assert!(!screen.contains("plain"), "{screen}");
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel { ref model_id, .. } if model_id == "flash"
        ));
        panel.set_models(Ok(ModelList {
            query: "zdr".into(),
            next: None,
            manage: false,
            models: Vec::new(),
            current: None,
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("No matching models."), "{screen}");
        assert!(!screen.contains("ZDR"), "{screen}");
    }

    /// The selected model's summary says how its id was identified, in the
    /// four wordings: one row, several agreeing rows, a disagreement, and
    /// nothing. The candidate rows themselves stay short.
    #[test]
    fn the_selected_models_summary_says_how_its_id_was_identified() {
        let mut panel = ModelsPanel::new();
        let outcome = |provider: &str, outcome| ModelItem {
            provider_id: provider.into(),
            provider_name: provider.into(),
            provider_version: 1,
            model_id: "listed-id".into(),
            display_name: "listed-id".into(),
            zdr: false,
            efforts: Vec::new(),
            default_effort: None,
            saved_effort: None,
            outcome,
        };
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![
                outcome(
                    "one",
                    ModelOutcome::Identified {
                        provider_id: "moonshot".into(),
                        model_id: "kimi-k3".into(),
                        inferred: false,
                    },
                ),
                outcome(
                    "several",
                    ModelOutcome::Identified {
                        provider_id: "openai".into(),
                        model_id: "gpt-5.5".into(),
                        inferred: true,
                    },
                ),
                outcome(
                    "ambiguous",
                    ModelOutcome::Ambiguous {
                        candidates: vec![
                            "ark-code/ark-code-latest".into(),
                            "byteplus-coding-plan/ark-code-latest".into(),
                        ],
                    },
                ),
                outcome("nothing", ModelOutcome::Unidentified),
            ],
            current: None,
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        // Wide enough that the candidate a summary names is not cut off.
        let screen = |panel: &ModelsPanel| {
            panel
                .render(
                    160,
                    24,
                    &terminal_palette(ColorMode::TrueColor, Appearance::Dark),
                )
                .join("\n")
        };
        for (wording, at) in [
            ("identified: kimi-k3 at moonshot", 0),
            ("inferred from several rows: gpt-5.5 at openai", 1),
            (
                "ambiguous: ark-code/ark-code-latest, byteplus-coding-plan/ark-code-latest",
                2,
            ),
            ("unidentified", 3),
        ] {
            for _ in 0..at {
                panel.handle_key(key_of(KeyCode::Down));
            }
            let screen = screen(&panel);
            assert!(screen.contains("Selected:"), "{screen}");
            assert!(screen.contains(wording), "{screen}");
            // The candidates themselves carry no identification text.
            assert!(
                !screen
                    .lines()
                    .find(|line| line.contains("/ listed-id"))
                    .unwrap_or_default()
                    .contains("identified"),
                "{screen}"
            );
            for _ in 0..at {
                panel.handle_key(key_of(KeyCode::Up));
            }
        }
    }

    /// A row that offers efforts and has none chosen, saved or declared says
    /// that a choice exists, where a row that offers none says nothing.
    #[test]
    fn an_unfocused_row_that_offers_efforts_and_has_none_selected_says_so() {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![
                item("plain", &[], None, None),
                item("sonnet", &["low", "high"], None, None),
            ],
            current: None,
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        let screen = drawn(&panel);
        let row = screen
            .lines()
            .find(|line| line.contains("sonnet"))
            .unwrap_or_else(|| panic!("{screen}"));
        assert!(row.contains('—'), "{screen}");
        assert!(!row.contains("low") && !row.contains("high"), "{screen}");
        let plain = screen
            .lines()
            .find(|line| line.contains("plain"))
            .unwrap_or_else(|| panic!("{screen}"));
        assert!(!plain.contains('—'), "{screen}");
    }

    /// Focused, the row brackets that placeholder the way it brackets an
    /// effort it would send.
    #[test]
    fn a_focused_row_that_offers_efforts_and_has_none_selected_brackets_it() {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![
                item("plain", &[], None, None),
                item("sonnet", &["low", "high"], None, None),
            ],
            current: None,
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        assert_eq!(panel.handle_key(key_of(KeyCode::Down)), Effect::None);
        let screen = drawn(&panel);
        let row = screen
            .lines()
            .find(|line| line.contains("sonnet"))
            .unwrap_or_else(|| panic!("{screen}"));
        assert!(row.contains("‹ — ›"), "{screen}");
    }

    /// What the model would use takes the placeholder's place, so a declared
    /// default reads as the effort it is.
    #[test]
    fn a_declared_default_effort_stands_in_for_the_placeholder() {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![
                item("plain", &[], None, None),
                item("sonnet", &["low", "high"], None, Some("high")),
            ],
            current: None,
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        let screen = drawn(&panel);
        let row = screen
            .lines()
            .find(|line| line.contains("sonnet"))
            .unwrap_or_else(|| panic!("{screen}"));
        assert!(row.contains("high"), "{screen}");
        assert!(!row.contains('—'), "{screen}");
    }

    /// The filter matches the row's own text, which carries the id beside the
    /// name: two models a gateway names alike stay reachable by the part that
    /// tells them apart.
    #[test]
    fn the_filter_matches_a_models_id() {
        let mut panel = ModelsPanel::new();
        let mut models = item("siliconflow/glm-5.3", &[], None, None);
        models.display_name = "GLM-5.3 · SiliconFlow".into();
        let mut other = item("volcengine/glm-5.3", &[], None, None);
        other.display_name = "GLM-5.3 · Volcengine Ark".into();
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![models, other],
            current: None,
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        assert_eq!(
            panel.paste("volcengine"),
            Effect::QueryModels {
                query: "volcengine".into(),
                after: None
            }
        );
        let models = panel
            .models()
            .iter()
            .filter(|model| model.model_id.starts_with("volcengine"))
            .cloned()
            .collect();
        panel.set_models(Ok(ModelList {
            query: "volcengine".into(),
            models,
            ..Default::default()
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("volcengine/glm-5.3"), "{screen}");
        assert!(!screen.contains("siliconflow"), "{screen}");
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel { ref model_id, .. } if model_id == "volcengine/glm-5.3"
        ));
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
        panel.set_models(Err("account.providers: discovery failed".into()));
        assert!(drawn(&panel).contains("flash"));
        assert!(drawn(&panel).contains("‹ high ›"));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                ref provider_id,
                ref model_id,
                ref reasoning_effort,
                ..
            } if provider_id == "dim" && model_id == "flash" && reasoning_effort.as_deref() == Some("high")
        ));
    }

    #[test]
    fn a_model_without_effort_ignores_arrows_and_enter_keeps_saved_preferences() {
        let mut panel = panel();
        assert_eq!(panel.handle_key(key_of(KeyCode::Down)), Effect::None);
        assert!(!drawn(&panel).contains('‹'), "{}", drawn(&panel));
        assert!(!drawn(&panel).contains('—'), "{}", drawn(&panel));
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
    fn filtering_keeps_arrows_for_effort_and_control_keys_for_the_cursor() {
        let mut panel = panel();
        let mut models = panel.models().to_vec();
        models[0].efforts = vec!["low".into(), "medium".into(), "high".into()];
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models,
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        panel.paste("flsh");
        panel.handle_key(KeyEvent::new(KeyCode::Char('b'), Modifiers::CTRL));
        panel.handle_key(KeyEvent::new(KeyCode::Char('b'), Modifiers::CTRL));
        assert_eq!(
            panel.handle_key(key_of(KeyCode::Char('a'))),
            Effect::QueryModels {
                query: "flash".into(),
                after: None
            }
        );
        let models = vec![panel.models()[0].clone()];
        panel.set_models(Ok(ModelList {
            query: "flash".into(),
            models,
            current: Some(("dim".into(), "flash".into())),
            ..Default::default()
        }));
        panel.handle_key(KeyEvent::new(KeyCode::Char('f'), Modifiers::CTRL));
        let screen = drawn(&panel);
        assert!(screen.contains("> flash"), "{screen}");
        assert!(!screen.contains("plain"), "{screen}");
        assert!(screen.contains("←/→ effort"), "{screen}");
        let cursor = panel.cursor(80).expect("the filter has a cursor");
        for (arrow, effort) in [
            (KeyCode::Right, "medium"),
            (KeyCode::Left, "low"),
            (KeyCode::Left, "high"),
        ] {
            assert_eq!(panel.handle_key(key_of(arrow)), Effect::None);
            let screen = drawn(&panel);
            assert!(screen.contains(&format!("‹ {effort} ›")), "{screen}");
            assert!(screen.contains("> flash"), "{screen}");
            assert_eq!(panel.cursor(80), Some(cursor));
        }
        assert_eq!(
            panel.handle_key(key_of(KeyCode::Delete)),
            Effect::QueryModels {
                query: "flas".into(),
                after: None
            }
        );
        let models = panel.models().to_vec();
        panel.set_models(Ok(ModelList {
            query: "flas".into(),
            models,
            ..Default::default()
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("> flas"), "{screen}");
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                ref provider_id,
                ref model_id,
                ref reasoning_effort,
                ..
            } if provider_id == "dim" && model_id == "flash"
                && reasoning_effort.as_deref() == Some("high")
        ));
    }

    /// Enter carries the row's drawn text to the confirmation, so the notice
    /// names the model the way the row does rather than by its ids.
    #[test]
    fn enter_carries_the_rows_label() {
        let mut panel = panel();
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel { ref label, .. } if label == "Dim OAuth / flash"
        ));
    }

    /// A re-read replaces the list while the picker is open; the effort the
    /// user cycled on a row that is still listed survives it.
    #[test]
    fn a_replaced_list_keeps_a_choice_its_row_still_offers() {
        let mut panel = panel();
        assert_eq!(panel.handle_key(key_of(KeyCode::Right)), Effect::None);
        assert!(drawn(&panel).contains("‹ high ›"));
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
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
            limits: Vec::new(),
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
            query: String::new(),
            next: None,
            manage: false,
            models: vec![item("flash", &["low"], Some("low"), Some("low"))],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
            limits: Vec::new(),
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
            query: String::new(),
            next: None,
            manage: false,
            models: vec![
                item("flash", &["low", "high"], None, Some("low")),
                item("plain", &[], None, None),
            ],
            current: Some(("dim".into(), "flash".into())),
            empty: Vec::new(),
            limits: Vec::new(),
        }));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::SelectModel {
                reasoning_effort: None,
                ..
            }
        ));
    }

    /// Candidates, policy and management are separate sections; the
    /// candidate list keeps its own rows and the diagnostics no longer
    /// become candidate entries.
    #[test]
    fn candidates_policy_and_management_are_separate_sections() {
        let mut panel = managed();
        policy(&mut panel);
        let screen = drawn(&panel);
        assert!(screen.contains("Conversation model"), "{screen}");
        assert!(screen.contains("Subagent models"), "{screen}");
        assert!(screen.contains("Agent choice"), "{screen}");
        assert!(screen.contains("Model management"), "{screen}");
        assert!(screen.contains("Manage models"), "{screen}");
        assert!(screen.contains("3 providers"), "{screen}");
        assert!(
            screen.contains("2 with missing limits / 1 without models"),
            "{screen}"
        );
        assert!(!screen.contains("Edit models"), "{screen}");
        assert!(!screen.contains("need a window · dimcode"), "{screen}");
    }

    /// Tab and Shift+Tab move between the sections the panel draws, and the
    /// hint follows the focus.
    #[test]
    fn tab_moves_between_sections_and_follows_the_focus() {
        let mut panel = managed();
        policy(&mut panel);
        assert!(drawn(&panel).contains("←/→ effort"), "{}", drawn(&panel));
        assert_eq!(panel.handle_key(key_of(KeyCode::Tab)), Effect::None);
        let screen = drawn(&panel);
        assert!(screen.contains("Enter change"), "{screen}");
        assert!(!screen.contains("←/→ effort"), "{screen}");
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::None
        ));
        assert!(drawn(&panel).contains("Agent choice (default)"));
        assert_eq!(panel.handle_key(key_of(KeyCode::Esc)), Effect::None);
        let screen = drawn(&panel);
        assert!(
            screen.contains("Enter change"),
            "the policy stays focused: {screen}"
        );
        assert_eq!(panel.handle_key(key_of(KeyCode::Tab)), Effect::None);
        assert!(drawn(&panel).contains("Enter manage"), "{}", drawn(&panel));
        assert_eq!(panel.handle_key(key_of(KeyCode::Tab)), Effect::None);
        assert!(drawn(&panel).contains("←/→ effort"), "{}", drawn(&panel));
        assert_eq!(panel.handle_key(key_of(KeyCode::BackTab)), Effect::None);
        assert!(drawn(&panel).contains("Enter manage"), "{}", drawn(&panel));
    }

    /// The search filters candidates only: with no match, the policy and
    /// management sections stay where they were.
    #[test]
    fn a_filtered_candidate_list_keeps_the_other_sections() {
        let mut panel = managed();
        policy(&mut panel);
        assert_eq!(
            panel.paste("no such model"),
            Effect::QueryModels {
                query: "no such model".into(),
                after: None
            }
        );
        panel.set_models(Ok(ModelList {
            query: "no such model".into(),
            manage: true,
            ..Default::default()
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("No matching models."), "{screen}");
        assert!(
            !screen.contains("Selected:"),
            "no selected model, so no summary: {screen}"
        );
        assert!(screen.contains("Subagent models"), "{screen}");
        assert!(screen.contains("Agent choice"), "{screen}");
        assert!(screen.contains("Manage models"), "{screen}");
    }

    /// The management entry opens the Provider list; the Providers with no
    /// selectable model are reachable, and their state is the row's detail.
    #[test]
    fn manage_models_lists_every_provider_with_its_state() {
        let mut panel = managed();
        policy(&mut panel);
        panel.handle_key(key_of(KeyCode::Tab));
        panel.handle_key(key_of(KeyCode::Tab));
        assert_eq!(panel.handle_key(key_of(KeyCode::Enter)), Effect::None);
        let screen = drawn(&panel);
        assert!(screen.contains("Dim OAuth"), "{screen}");
        assert!(screen.contains("Other"), "{screen}");
        assert!(screen.contains("Third"), "{screen}");
        assert!(screen.contains("ready"), "{screen}");
        assert!(screen.contains("2 with missing limits"), "{screen}");
        assert!(screen.contains("no models"), "{screen}");
        assert!(!screen.contains("dimcode models other"), "{screen}");
        panel.handle_key(key_of(KeyCode::Down));
        let screen = drawn(&panel);
        assert!(screen.contains("dimcode models other"), "{screen}");
        panel.handle_key(key_of(KeyCode::Up));
        assert!(matches!(
            panel.handle_key(key_of(KeyCode::Enter)),
            Effect::OpenArea {
                area: Area::Models { ref provider_id },
                note: None,
            } if provider_id == "dim"
        ));
    }

    /// A daemon without `provider.management` offers no management action,
    /// while the read-only diagnostics and their recovery guidance stay.
    #[test]
    fn an_unmanaged_daemon_keeps_the_read_only_diagnostics() {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            query: String::new(),
            next: None,
            manage: false,
            models: vec![item_for("dim", "Dim OAuth", "flash", &[], None, None)],
            current: None,
            empty: vec![EmptyProvider {
                provider_id: "third".into(),
                provider_name: "Third".into(),
                reason: EmptyReason::Unreadable,
            }],
            limits: Vec::new(),
        }));
        let screen = drawn(&panel);
        assert!(screen.contains("Model management"), "{screen}");
        assert!(screen.contains("Third: catalog unreadable"), "{screen}");
        assert!(!screen.contains("Manage models"), "{screen}");
        assert_eq!(panel.handle_key(key_of(KeyCode::Tab)), Effect::None);
        assert!(drawn(&panel).contains("←/→ effort"), "{}", drawn(&panel));
    }

    /// The fixed-model picker is candidate rows only: no management action,
    /// no diagnostics.
    #[test]
    fn the_fixed_model_picker_holds_candidates_only() {
        let mut panel = managed();
        policy(&mut panel);
        panel.handle_key(key_of(KeyCode::Tab));
        panel.handle_key(key_of(KeyCode::Enter));
        panel.handle_key(key_of(KeyCode::Down));
        panel.handle_key(key_of(KeyCode::Down));
        assert_eq!(panel.handle_key(key_of(KeyCode::Enter)), Effect::None);
        let screen = drawn(&panel);
        assert!(screen.contains("Fixed subagent model"), "{screen}");
        assert!(screen.contains("Dim OAuth / flash"), "{screen}");
        assert!(!screen.contains("need a window"), "{screen}");
        assert!(!screen.contains("unreadable"), "{screen}");
        assert!(!screen.contains("Manage models"), "{screen}");
    }

    /// Escape from a child page returns one level and restores the query,
    /// selection and list position it had.
    #[test]
    fn returning_from_a_child_page_restores_the_candidate_list() {
        let mut panel = managed();
        policy(&mut panel);
        panel.paste("plain");
        panel.handle_key(key_of(KeyCode::Down));
        let before = drawn(&panel);
        assert!(before.contains("> plain"), "{before}");
        panel.handle_key(key_of(KeyCode::Tab));
        panel.handle_key(key_of(KeyCode::Enter));
        assert!(drawn(&panel).contains("Agent choice (default)"));
        assert_eq!(panel.handle_key(key_of(KeyCode::Esc)), Effect::None);
        let after = drawn(&panel);
        assert!(after.contains("> plain"), "{after}");
        assert!(after.contains("Enter change"), "{after}");
        assert!(after.contains("Other / plain"), "{after}");
    }

    /// The policy section is absent until the daemon reports the policy, and
    /// when a saved policy comes back the panel focuses it.
    #[test]
    fn the_policy_section_follows_the_daemon_and_a_save_focuses_it() {
        let mut panel = managed();
        let screen = drawn(&panel);
        assert!(!screen.contains("Subagent models"), "{screen}");
        panel.handle_key(key_of(KeyCode::Tab));
        assert!(drawn(&panel).contains("Enter manage"));
        policy(&mut panel);
        panel.set_policy(ModelPolicyUpdate::Read {
            revision: "2".into(),
            policy: SubagentModelPolicy::Inherit,
            saved: true,
        });
        let screen = drawn(&panel);
        assert!(screen.contains("Inherit requesting branch"), "{screen}");
        assert!(screen.contains("Enter change"), "{screen}");
    }

    /// At the 15 rows an 80x24 scrollback session gives the dialog, the
    /// compact footer keeps all three sections, the selected summary and a
    /// viewport of three candidates.
    #[test]
    fn a_short_dialog_keeps_every_section_and_the_summary() {
        let mut panel = ModelsPanel::new();
        panel.set_models(Ok(ModelList {
            manage: true,
            models: (1..=10)
                .map(|index| {
                    item(
                        &format!("model-{index:02}"),
                        &["low", "high"],
                        None,
                        Some("low"),
                    )
                })
                .collect(),
            current: Some(("dim".into(), "model-01".into())),
            empty: Vec::new(),
            limits: Vec::new(),
            ..Default::default()
        }));
        policy(&mut panel);
        let rows = panel.render(
            80,
            15,
            &terminal_palette(ColorMode::TrueColor, Appearance::Dark),
        );
        let screen = crate::ui::render::panel::strip_escapes(&rows.join("\n"));
        assert!(rows.len() <= 15, "{screen}");
        assert!(screen.contains("Conversation model"), "{screen}");
        assert!(screen.contains("model-01"), "{screen}");
        assert!(screen.contains("model-02"), "{screen}");
        assert!(screen.contains("model-03"), "{screen}");
        assert!(screen.contains("Selected:"), "{screen}");
        assert!(screen.contains("Subagent models"), "{screen}");
        assert!(screen.contains("Agent choice"), "{screen}");
        assert!(screen.contains("Model management"), "{screen}");
        assert!(screen.contains("Manage models"), "{screen}");
    }

    /// A terminal too short for the titles keeps the focused section and the
    /// diagnostics; Tab still changes what is shown.
    #[test]
    fn the_shortest_dialog_shows_the_focused_section() {
        let mut panel = managed();
        policy(&mut panel);
        let screen = |panel: &ModelsPanel| {
            let rows = panel.render(
                80,
                7,
                &terminal_palette(ColorMode::TrueColor, Appearance::Dark),
            );
            assert!(rows.len() <= 7, "{}", rows.join("\n"));
            crate::ui::render::panel::strip_escapes(&rows.join("\n"))
        };
        let models = screen(&panel);
        assert!(models.contains("Dim OAuth / flash"), "{models}");
        assert!(
            models.contains("2 with missing limits"),
            "the diagnostics stay: {models}"
        );
        assert!(models.contains("Enter select + default"), "{models}");
        assert!(!models.contains("Manage models"), "{models}");

        panel.handle_key(key_of(KeyCode::Tab));
        let policy_view = screen(&panel);
        assert!(policy_view.contains("Agent choice"), "{policy_view}");
        assert!(!policy_view.contains("Manage models"), "{policy_view}");
        assert!(policy_view.contains("Enter change"), "{policy_view}");

        panel.handle_key(key_of(KeyCode::Tab));
        let management = screen(&panel);
        assert!(management.contains("Manage models"), "{management}");
        assert!(!management.contains("Agent choice"), "{management}");
        assert!(management.contains("Enter manage"), "{management}");
    }
}
