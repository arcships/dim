//! How a model id names a bundled model, and what numbers it takes from it.
//!
//! Identification runs first: a vendor's own entries name the model, by its
//! declared id, by its own tokens, by the upstream canonical id it publishes
//! or by the name it displays. Only when none of them names one model does the
//! projection fall back to the older walk over a fixed list of sellers.

use super::*;

/// Which step of the pipeline named the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rule {
    /// The projection's own list carries this exact id.
    ProviderExact,
    /// A vendor's row whose own tokens are the input's head.
    Token,
    /// A vendor's row named through its upstream canonical id, its display
    /// name, or both together. `narrowed` is the case the display name settled
    /// several canonical hits.
    Canonical { narrowed: bool },
    /// A row the `custom-provider` list declares.
    CustomProvider,
}

impl Rule {
    /// The word `metadata.matchStrategy` carries for this rule.
    fn strategy(self, provider_id: &str, model_id: &str, input: &str) -> String {
        match self {
            Rule::ProviderExact => "provider-exact".into(),
            Rule::Token if model_id == input => format!("{provider_id}-exact"),
            Rule::Token => format!("{provider_id}-token"),
            Rule::Canonical { .. } => format!("{provider_id}-canonical"),
            Rule::CustomProvider => "custom-provider".into(),
        }
    }

    fn score(self) -> f64 {
        match self {
            Rule::ProviderExact | Rule::Token | Rule::CustomProvider => 1.,
            Rule::Canonical { .. } => 0.95,
        }
    }
}

/// One row a step considered, named the way a client can read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) provider_id: String,
    pub(super) model_id: String,
    rule: Rule,
}

/// The one model an id names, and the row that supplied it.
struct Identification {
    provider_id: String,
    entry: ModelEntry,
    /// The step that named it, plus the strategy word and score derived
    /// from it.
    strategy: String,
    score: f64,
}

/// What identification found.
enum Outcome {
    One(Box<Identification>),
    /// Several rows still disagree, so no row may supply numbers.
    Ambiguous(Vec<Candidate>),
    None,
}

/// A row with the provider it belongs to, as the steps compare them.
#[derive(Clone, Copy)]
pub(super) struct Row<'a> {
    provider_id: &'a str,
    entry: &'a ModelEntry,
    tokens: &'a [String],
}

/// The numbers a row declares. Two rows that agree on all three are one model,
/// so they do not form a conflict.
type Numbers = (Option<u64>, Option<u64>, Vec<String>);

impl Row<'_> {
    fn candidate(&self, rule: Rule) -> Candidate {
        Candidate {
            provider_id: self.provider_id.to_owned(),
            model_id: self.entry.model_id.clone(),
            rule,
        }
    }

    /// The numbers a row declares. Two rows that agree on all three are one
    /// model, so they do not form a conflict.
    fn numbers(&self) -> Numbers {
        let mut efforts = self
            .entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.reasoning.as_ref())
            .and_then(|reasoning| reasoning.effort_options.clone())
            .unwrap_or_default();
        efforts.sort();
        (
            self.entry.capabilities.context_window,
            self.entry.capabilities.max_output_tokens,
            efforts,
        )
    }
}

/// The bundled row an id names, plus the step's strategy word and score.
pub(super) type Lookup = (String, ModelEntry, String, f64);

/// The identification index, built once at load: every row the steps read,
/// its precomputed token tuple, canonical tokens and display tokens, and a
/// map from each token tuple to the rows that carry it, per list.
pub(super) struct IdIndex {
    vendor_tokens: HashMap<Vec<String>, Vec<usize>>,
    custom_tokens: HashMap<Vec<String>, Vec<usize>>,
    rows: Vec<IndexedRow>,
}

/// One row of the index. `provider` and `model` address
/// `providers[provider].models[model]`.
struct IndexedRow {
    provider: usize,
    model: usize,
    role_is_vendor: bool,
    tokens: Vec<String>,
    canonical: Option<Vec<String>>,
    display: Vec<String>,
}

impl IdIndex {
    pub(super) fn new(providers: &[CatalogProvider]) -> Self {
        let mut vendor_tokens: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
        let mut custom_tokens: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
        let mut rows = Vec::new();
        for (provider, catalog) in providers.iter().enumerate() {
            let role_is_vendor = catalog.metadata.as_ref().and_then(|metadata| metadata.role)
                == Some(ProviderRole::Vendor);
            if !role_is_vendor && catalog.provider_id != "custom-provider" {
                continue;
            }
            let map = if role_is_vendor {
                &mut vendor_tokens
            } else {
                &mut custom_tokens
            };
            for (model, entry) in catalog.models.iter().enumerate() {
                let model_tokens = split_id(&entry.model_id);
                map.entry(model_tokens.clone())
                    .or_default()
                    .push(rows.len());
                rows.push(IndexedRow {
                    provider,
                    model,
                    role_is_vendor,
                    tokens: model_tokens,
                    canonical: entry
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.raw.get("canonical_model_id"))
                        .and_then(Value::as_str)
                        .map(|canonical| {
                            let lower = canonical.to_lowercase();
                            let last = lower.rsplit('/').next().unwrap_or(&lower);
                            split_tokens(last)
                        }),
                    display: split_display(&entry.display_name),
                });
            }
        }
        Self {
            vendor_tokens,
            custom_tokens,
            rows,
        }
    }

    /// The rows of one list whose own token tuple is a head of the input, as
    /// positions, with the length of the longest such tuple.
    fn token_rows(&self, index: Index, query: &[String]) -> Option<(usize, &[usize])> {
        let map = match index {
            Index::Vendor => &self.vendor_tokens,
            Index::CustomProvider => &self.custom_tokens,
        };
        for longest in (1..=query.len()).rev() {
            if let Some(rows) = map.get(&query[..longest]) {
                return Some((longest, rows));
            }
        }
        None
    }

    /// The rows of one list whose canonical or display tokens name the
    /// input, as positions.
    fn name_rows(&self, index: Index, query: &[String]) -> (Vec<usize>, Vec<usize>) {
        let mut canonical = Vec::new();
        let mut display = Vec::new();
        for (position, row) in self.rows.iter().enumerate() {
            let wanted = match index {
                Index::Vendor => row.role_is_vendor,
                Index::CustomProvider => !row.role_is_vendor,
            };
            if !wanted {
                continue;
            }
            if row
                .canonical
                .as_deref()
                .is_some_and(|tokens| names_tokens(tokens, query))
            {
                canonical.push(position);
            }
            if !row.display.is_empty() && names_tokens(&row.display, query) {
                display.push(position);
            }
        }
        (canonical, display)
    }
}

impl BundledCatalog {
    /// The bundled row an id names, with the step that named it.
    ///
    /// `None` means no step named one model; the caller keeps its own
    /// behaviour for that id. A step that finds several rows which still
    /// disagree refuses the id instead of letting the next step answer
    /// something weaker.
    pub(super) fn identify(
        &self,
        provider: &ProviderProjection,
        id: &str,
    ) -> Result<Option<Lookup>, Vec<Candidate>> {
        match self.identify_steps(provider, id) {
            Outcome::One(found) => Ok(Some((
                found.provider_id,
                found.entry,
                found.strategy,
                found.score,
            ))),
            Outcome::Ambiguous(candidates) => Err(candidates),
            Outcome::None => Ok(None),
        }
    }

    fn identify_steps(&self, provider: &ProviderProjection, id: &str) -> Outcome {
        if let Some((provider_id, entry)) = self.own_row(provider, id) {
            let strategy = Rule::ProviderExact.strategy(&provider_id, &entry.model_id, id);
            return self.one(provider_id, entry, strategy, Rule::ProviderExact.score());
        }
        let Some(query) = Query::parse(id) else {
            return Outcome::None;
        };
        let namespace = namespace_of(id);
        match self.identify_by_tokens(Index::Vendor, &query, provider, namespace, Rule::Token, id) {
            Outcome::None => {}
            found => return found,
        }
        match self.identify_by_names(&query, provider, namespace, id) {
            Outcome::None => {}
            found => return found,
        }
        match self.identify_by_tokens(
            Index::CustomProvider,
            &query,
            provider,
            namespace,
            Rule::CustomProvider,
            id,
        ) {
            Outcome::None => {}
            found => return found,
        }
        self.identify_by_names_in(Index::CustomProvider, &query, provider, namespace, id)
    }

    /// Step 1: the projection's own list, by exact id. The row answers with
    /// all its own fields, which describe the provider's own API.
    fn own_row(&self, provider: &ProviderProjection, id: &str) -> Option<(String, ModelEntry)> {
        if provider.source == Some(ModelSource::Custom) {
            return None;
        }
        let entry = provider.models.iter().find(|model| model.model_id == id)?;
        // The bundled entry is the declaration for a builtin provider's model,
        // so a stored copy that lacks a limit is supplied from it.
        let entry = self
            .bundled_models(&provider.provider_id)
            .and_then(|models| models.iter().find(|bundled| bundled.model_id == id))
            .cloned()
            .unwrap_or_else(|| entry.clone());
        Some((provider.provider_id.clone(), entry))
    }

    /// One row answered, with the strategy word and score that describe how.
    fn one(&self, provider_id: String, entry: ModelEntry, strategy: String, score: f64) -> Outcome {
        Outcome::One(Box::new(Identification {
            provider_id,
            entry,
            strategy,
            score,
        }))
    }

    fn rows_in(&self, positions: &[usize]) -> Vec<Row<'_>> {
        positions
            .iter()
            .map(|&position| {
                let row = &self.ids.rows[position];
                Row {
                    provider_id: self.providers[row.provider].provider_id.as_str(),
                    entry: &self.providers[row.provider].models[row.model],
                    tokens: &row.tokens,
                }
            })
            .collect()
    }

    /// Steps 2 and 4: a row whose own tokens are the input's head, with the
    /// tail allowed to be a date, `latest`, a tag or a bracket modifier.
    fn identify_by_tokens(
        &self,
        index: Index,
        query: &Query,
        provider: &ProviderProjection,
        namespace: Option<&str>,
        rule: Rule,
        input: &str,
    ) -> Outcome {
        let Some((longest, rows)) = self.ids.token_rows(index, &query.tokens) else {
            return Outcome::None;
        };
        // A tail the closed set does not allow is not this step's answer, so
        // the id moves to the next step rather than reading as its family base.
        if !tail_allowed(&query.tokens[longest..]) {
            return Outcome::None;
        }
        let rows = self.rows_in(rows);
        self.settle(rows, provider, namespace, rule, query, input)
    }

    /// Step 3: the vendor's upstream canonical id and its display name, read
    /// together. The display name narrows several canonical hits to one row,
    /// and a disagreement between the two is an ambiguity, not a guess.
    fn identify_by_names(
        &self,
        query: &Query,
        provider: &ProviderProjection,
        namespace: Option<&str>,
        input: &str,
    ) -> Outcome {
        self.identify_by_names_in(Index::Vendor, query, provider, namespace, input)
    }

    /// The canonical-id and display-name step over one index. Step 3 runs it
    /// over vendor entries, step 4 over the `custom-provider` rows.
    fn identify_by_names_in(
        &self,
        index: Index,
        query: &Query,
        provider: &ProviderProjection,
        namespace: Option<&str>,
        input: &str,
    ) -> Outcome {
        let (canonical, display) = self.ids.name_rows(index, &query.tokens);
        let canonical = self.rows_in(&canonical);
        let display = self.rows_in(&display);
        let display_groups = collapse(display.clone());
        if display_groups.len() == 1 && !display.is_empty() {
            let group = &display_groups[0];
            if canonical.is_empty() {
                return self.settle(
                    group.clone(),
                    provider,
                    namespace,
                    Rule::Canonical { narrowed: true },
                    query,
                    input,
                );
            }
            let inside = |row: &Row<'_>| {
                canonical.iter().any(|c| {
                    c.provider_id == row.provider_id && c.entry.model_id == row.entry.model_id
                })
            };
            if group.iter().all(inside) {
                return self.settle(
                    group.clone(),
                    provider,
                    namespace,
                    Rule::Canonical { narrowed: true },
                    query,
                    input,
                );
            }
            let mut candidates: Vec<Candidate> = group
                .iter()
                .map(|row| row.candidate(Rule::Canonical { narrowed: true }))
                .collect();
            candidates.extend(
                canonical
                    .iter()
                    .map(|row| row.candidate(Rule::Canonical { narrowed: false })),
            );
            return Outcome::Ambiguous(candidates);
        }
        let rows = if canonical.is_empty() {
            display
        } else {
            canonical
        };
        self.settle(
            rows,
            provider,
            namespace,
            Rule::Canonical { narrowed: false },
            query,
            input,
        )
    }

    /// Collapse the rows that declare one set of numbers, then settle which
    /// group answers. More than one group left is an ambiguity. A row that
    /// does not declare both limits cannot supply numbers on its own, so it
    /// is no candidate here and the caller keeps its own answer for the id;
    /// the fill that supplies an undeclared limit arrives with the merge.
    fn settle(
        &self,
        rows: Vec<Row<'_>>,
        provider: &ProviderProjection,
        namespace: Option<&str>,
        rule: Rule,
        query: &Query,
        input: &str,
    ) -> Outcome {
        let mut rows = rows;
        rows.retain(|row| row.entry.capabilities.missing_limits().is_empty());
        if rows.is_empty() {
            return Outcome::None;
        }
        let groups = collapse(rows);
        let Some(group) = self.choose(&groups, provider, namespace, query) else {
            return Outcome::Ambiguous(
                groups
                    .iter()
                    .flatten()
                    .map(|row| row.candidate(rule))
                    .collect(),
            );
        };
        let chosen = group
            .iter()
            .find(|row| Some(row.provider_id) == Some(provider.provider_id.as_str()))
            .or_else(|| {
                namespace
                    .and_then(|namespace| group.iter().find(|row| row.provider_id == namespace))
            })
            .unwrap_or_else(|| {
                group
                    .iter()
                    .min_by_key(|row| row.provider_id.len())
                    .unwrap()
            });
        let strategy = rule.strategy(chosen.provider_id, &chosen.entry.model_id, input);
        self.one(
            chosen.provider_id.to_owned(),
            chosen.entry.clone(),
            strategy,
            rule.score(),
        )
    }

    /// The fixed priority of the design, in order: the projection's own
    /// provider, then the API entry over a plan entry, then the vendor the
    /// input names, then a plain vendor entry, then an exact token key.
    fn choose<'g>(
        &self,
        groups: &'g [Vec<Row<'g>>],
        provider: &ProviderProjection,
        namespace: Option<&str>,
        query: &Query,
    ) -> Option<&'g Vec<Row<'g>>> {
        if groups.len() == 1 {
            return groups.first();
        }
        let own = groups
            .iter()
            .filter(|group| {
                group
                    .iter()
                    .any(|row| row.provider_id == provider.provider_id)
            })
            .collect::<Vec<_>>();
        if own.len() == 1 {
            return Some(own[0]);
        }
        let plan = |group: &&Vec<Row<'_>>| {
            group
                .iter()
                .all(|row| self.is_plan(row.provider_id) == Some(true))
        };
        let (plans, apis): (Vec<_>, Vec<_>) = groups.iter().partition(plan);
        if apis.len() == 1 && !plans.is_empty() {
            return Some(apis[0]);
        }
        let pool = if apis.is_empty() {
            groups.iter().collect()
        } else {
            apis
        };
        if let Some(namespace) = namespace {
            let named = pool
                .iter()
                .filter(|group| group.iter().any(|row| row.provider_id == namespace))
                .collect::<Vec<_>>();
            if named.len() == 1 {
                return Some(named[0]);
            }
        }
        let vendors = pool
            .iter()
            .filter(|group| group.iter().any(|row| row_is_vendor(self, row)))
            .collect::<Vec<_>>();
        if vendors.len() == 1 {
            return Some(vendors[0]);
        }
        if vendors.is_empty() {
            return None;
        }
        let exact = vendors
            .iter()
            .filter(|group| group.iter().any(|row| row.tokens == query.tokens))
            .collect::<Vec<_>>();
        if exact.len() == 1 {
            return Some(exact[0]);
        }
        None
    }
}

fn row_is_vendor(catalog: &BundledCatalog, row: &Row<'_>) -> bool {
    catalog.role(row.provider_id) == Some(ProviderRole::Vendor)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Index {
    Vendor,
    CustomProvider,
}

/// An id broken into the tokens a catalog name is compared with.
struct Query {
    tokens: Vec<String>,
}

impl Query {
    /// The id without its namespace segments, tag and bracket modifier,
    /// lowercased and split on the separators a model name uses.
    fn parse(id: &str) -> Option<Self> {
        let lower = id.trim().to_lowercase();
        let mut name = lower.rsplit('/').next()?.to_owned();
        if let Some((head, _)) = name.rsplit_once(':') {
            name = head.to_owned();
        }
        if let Some(start) = name.rfind('[')
            && name.ends_with(']')
        {
            name.truncate(start);
        }
        let tokens: Vec<_> = name
            .split(['-', '_', '.', ' '])
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect();
        (!tokens.is_empty()).then_some(Self { tokens })
    }
}

fn namespace_of(id: &str) -> Option<&str> {
    let segments: Vec<_> = id.trim().split('/').collect();
    (segments.len() > 1).then(|| segments[segments.len() - 2])
}

/// A model id without its namespace segment and tag, lowercased and split.
fn split_id(id: &str) -> Vec<String> {
    let lower = id.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let name = name.rsplit_once(':').map_or(name, |(head, _)| head);
    split_tokens(name)
}

/// A display name, lowercased and split.
fn split_display(display: &str) -> Vec<String> {
    split_tokens(&display.to_lowercase())
}

fn split_tokens(name: &str) -> Vec<String> {
    name.split(['-', '_', '.', ' '])
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether a catalog name names this input: equal, or one a head of the other
/// with a tail the closed set allows.
fn names_tokens(tokens: &[String], input: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    if tokens == input {
        return true;
    }
    if tokens.len() > input.len() && tokens[..input.len()] == *input {
        return tail_allowed(&tokens[input.len()..]);
    }
    input.len() > tokens.len()
        && input[..tokens.len()] == tokens[..]
        && tail_allowed(&input[tokens.len()..])
}

/// The only tails a longer name may carry: `latest`, a date, a tag or a
/// modifier. Nothing else means the name is a different model.
fn tail_allowed(tail: &[String]) -> bool {
    if tail.is_empty() {
        return true;
    }
    if tail.len() == 3
        && tail[0].len() == 4
        && tail
            .iter()
            .all(|token| token.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    tail.iter().all(|token| {
        token == "latest"
            || matches!(token.len(), 4 | 6 | 8) && token.chars().all(|c| c.is_ascii_digit())
    })
}

fn collapse(rows: Vec<Row<'_>>) -> Vec<Vec<Row<'_>>> {
    let mut groups: Vec<(Numbers, Vec<Row<'_>>)> = Vec::new();
    for row in rows {
        let numbers = row.numbers();
        match groups.iter_mut().find(|group| group.0 == numbers) {
            Some(group) => group.1.push(row),
            None => groups.push((numbers, vec![row])),
        }
    }
    groups.into_iter().map(|group| group.1).collect()
}

impl BundledCatalog {
    /// The bundled row an id names. Identification runs first; the older
    /// walk serves an id that no step named.
    pub(super) fn lookup(&self, provider: &ProviderProjection, id: &str) -> Option<Lookup> {
        match self.identify(provider, id) {
            Ok(Some(found)) => return Some(found),
            // Several rows claim the id and disagree, so none may supply
            // numbers; the older walk is not allowed to answer something
            // weaker than the conflict it would be hiding.
            Err(_) => return None,
            Ok(None) => {}
        }
        self.lookup_legacy(provider, id)
    }

    /// The older walk: the projection's own list by exact id, then a fixed
    /// list of family and aggregator sources, then any provider by exact id
    /// and by a normalized id.
    fn lookup_legacy(
        &self,
        provider: &ProviderProjection,
        id: &str,
    ) -> Option<(String, ModelEntry, String, f64)> {
        let mut ids = vec!["custom-provider"];
        let normalized = variants(id);
        for (prefix, sources) in [
            ("deepseek-", &["deepseek"][..]),
            ("glm-", &["zai", "zhipuai"]),
            ("qwen", &["bailian-coding-plan", "bailian"]),
            ("kimi-", &["moonshot", "moonshot-ai"]),
            ("claude-", &["anthropic"]),
            ("gemini-", &["google"]),
            ("gpt-", &["openai"]),
            ("minimax-", &["minimax", "minimax-cn"]),
            ("seed-", &["doubao-seed"]),
        ] {
            if normalized.iter().any(|variant| variant.starts_with(prefix)) {
                ids.extend_from_slice(sources);
            }
        }
        ids.extend([
            "openrouter",
            "zenmux",
            "vercel-ai-gateway",
            "jiekou",
            "ppinfra",
            "aihubmix",
        ]);
        for pid in ids {
            if let Some(models) = self.bundled_models(pid)
                && let Some((m, kind, score)) = match_models(models, id)
            {
                return Some((pid.into(), m.clone(), format!("{pid}-{kind}"), score));
            }
        }
        for exact in [true, false] {
            let candidates: Vec<_> = self
                .providers
                .iter()
                .filter(|p| !matches!(p.provider_id.as_str(), "custom-provider" | "aihubmix"))
                .flat_map(|p| p.models.iter().map(move |m| (p, m)))
                .filter(|(_, m)| {
                    if exact {
                        m.model_id == id
                    } else {
                        !variants(&m.model_id).is_disjoint(&normalized)
                    }
                })
                .filter_map(|(p, m)| media_signature(m).map(|signature| (p, m, signature)))
                .collect();
            if let Some((p, m, signature)) = candidates.first()
                && candidates.iter().all(|(_, _, other)| other == signature)
            {
                let kind = if exact { "exact" } else { "normalized" };
                return Some((
                    p.provider_id.clone(),
                    (*m).clone(),
                    format!("{}-{kind}", p.provider_id),
                    if exact { 1. } else { 0.98 },
                ));
            }
        }
        if provider.source == Some(ModelSource::Custom) {
            provider.models.iter().find(|m| m.model_id == id).map(|m| {
                (
                    provider.provider_id.clone(),
                    m.clone(),
                    "custom-config-exact".into(),
                    1.,
                )
            })
        } else {
            None
        }
    }
}

fn media_signature(model: &ModelEntry) -> Option<Value> {
    let raw = model.metadata.as_ref().map(|m| &m.raw);
    let get = |name: &str| raw.and_then(|r| r.get(name));
    let sorted = |values: Vec<String>| {
        let mut values: Vec<_> = values
            .into_iter()
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect();
        values.sort();
        values.dedup();
        values
    };
    let list = |value: Option<&Value>| {
        value
            .and_then(Value::as_array)
            .map(|v| strings(v))
            .unwrap_or_default()
    };
    let kind = model.metadata.as_ref().and_then(model_kind);
    let hints = sorted(list(get("typeHints").or_else(|| get("type_hints"))));
    let actions = sorted(list(
        get("modalityCapabilities").or_else(|| get("modality_capabilities")),
    ));
    let input = sorted(list(get("modalities").and_then(|v| v.get("input"))));
    let output = sorted(list(get("modalities").and_then(|v| v.get("output"))));
    let modalities = sorted(model.capabilities.modalities.clone());
    let is_media = model.capabilities.audio
        || model.capabilities.vision
        || kind.as_ref().is_some_and(|kind| kind != "chat")
        || !actions.is_empty()
        || hints.iter().any(|kind| kind != "chat")
        || input
            .iter()
            .chain(&output)
            .chain(&modalities)
            .any(|v| v != "text");
    is_media.then(|| {
        serde_json::json!({
            "type": kind, "typeHints": hints, "modalityCapabilities": actions,
            "input": input, "output": output, "modalities": modalities,
            "audio": model.capabilities.audio, "vision": model.capabilities.vision,
        })
    })
}

fn match_models<'a>(
    models: &'a [ModelEntry],
    id: &str,
) -> Option<(&'a ModelEntry, &'static str, f64)> {
    if let Some(m) = models.iter().find(|m| m.model_id == id) {
        return Some((m, "exact", 1.));
    }
    let target = variants(id);
    let normalized: Vec<_> = models
        .iter()
        .filter(|m| variants(&m.model_id).iter().any(|v| target.contains(v)))
        .collect();
    if normalized.len() == 1 {
        return Some((normalized[0], "normalized", 0.98));
    }
    let nums = numbers(id);
    let mut scored: Vec<_> = models
        .iter()
        .filter(|m| numbers(&m.model_id) == nums)
        .map(|m| (m, similarity(id, &m.model_id)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let best = scored.first()?;
    if best.1 >= 0.82 && best.1 - scored.get(1).map(|x| x.1).unwrap_or(0.) >= 0.08 {
        Some((best.0, "fuzzy", best.1))
    } else {
        None
    }
}

pub(super) fn variants(s: &str) -> HashSet<String> {
    let c = canonical(s);
    let mut r = HashSet::from([c.clone()]);
    if let Some((_, tail)) = c.rsplit_once('/') {
        r.insert(tail.into());
    }
    r
}

pub(super) fn canonical(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    let mut result = String::new();
    let mut separator = false;
    for ch in lower.strip_prefix("models/").unwrap_or(&lower).chars() {
        if ch == '_' || ch == ':' || ch.is_whitespace() {
            if !separator {
                result.push('-');
            }
            separator = true;
        } else {
            result.push(ch);
            separator = false;
        }
    }
    result
}

fn numbers(s: &str) -> Vec<String> {
    s.split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (canonical(a), canonical(b));
    let max = a.len().max(b.len());
    if max == 0 {
        return 1.;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.bytes().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.bytes().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(prev + usize::from(ca != cb));
            prev = old;
        }
    }
    1. - row[b.len()] as f64 / max as f64
}
