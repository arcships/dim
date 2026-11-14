//! How a model id names a bundled model, and what numbers it takes from it.
//!
//! Identification runs first: a vendor's own entries name the model, by its
//! declared id, by its own tokens, by the upstream canonical id it publishes
//! or by the name it displays. Only when no step names one model do the
//! aggregator rows that name it merge, in the safe direction.

use super::*;

/// Which step of the pipeline named the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rule {
    /// The projection's own list carries this exact id.
    ProviderExact,
    /// A bundled row carries the input's whole qualified id, and the id's
    /// namespace names that row's provider.
    Qualified,
    /// A vendor's row whose own tokens are the input's head.
    Token,
    /// A vendor's row named through its upstream canonical id, its display
    /// name, or both together. `narrowed` is the case the display name settled
    /// several canonical hits.
    Canonical { narrowed: bool },
    /// A row the `custom-provider` list declares.
    CustomProvider,
    /// No step named one row, so the aggregator rows that join supply the
    /// numbers: the smallest window, the smallest output cap and the
    /// intersection of the efforts.
    Merge,
}

impl Rule {
    /// The word `metadata.matchStrategy` carries for this rule.
    fn strategy(self, provider_id: &str, model_id: &str, input: &str) -> String {
        match self {
            Rule::ProviderExact => "provider-exact".into(),
            Rule::Qualified => format!("{provider_id}-exact"),
            Rule::Token if model_id == input => format!("{provider_id}-exact"),
            Rule::Token => format!("{provider_id}-token"),
            Rule::Canonical { .. } => format!("{provider_id}-canonical"),
            Rule::CustomProvider => "custom-provider".into(),
            Rule::Merge => format!("{provider_id}-merged"),
        }
    }

    fn score(self) -> f64 {
        match self {
            Rule::ProviderExact | Rule::Qualified | Rule::Token | Rule::CustomProvider => 1.,
            Rule::Canonical { .. } => 0.95,
            Rule::Merge => 0.9,
        }
    }

    /// The step's own word, as `metadata.identification.rule` carries it.
    fn word(self) -> &'static str {
        match self {
            Rule::ProviderExact => "provider-exact",
            Rule::Qualified => "qualified",
            Rule::Token => "token",
            Rule::Canonical { .. } => "canonical",
            Rule::CustomProvider => "custom-provider",
            Rule::Merge => "merge",
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

/// What identification found.
enum Outcome {
    One(Box<Lookup>),
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

/// The `(window, output)` pair a provider prints on many of its rows as a
/// default rather than a declaration. The merge and the fill read the pairs
/// here; the catalog keeps no record of its own for them.
const PLACEHOLDER_LIMITS: &[(&str, u64, u64)] = &[
    ("tokenflux", 4096, 4096),
    ("aihubmix", 8192, 8192),
    ("siliconflow", 131072, 8192),
];

/// Whether this row's limits are its provider's default rather than a
/// declaration.
pub(super) fn is_placeholder(provider_id: &str, entry: &ModelEntry) -> bool {
    let Some((_, window, output)) = PLACEHOLDER_LIMITS
        .iter()
        .find(|(id, _, _)| *id == provider_id)
    else {
        return false;
    };
    entry.capabilities.context_window == Some(*window)
        && entry.capabilities.max_output_tokens == Some(*output)
}

/// The bundled row an id names, plus the step's strategy word and score.
pub(super) struct Lookup {
    pub(super) provider_id: String,
    pub(super) entry: ModelEntry,
    /// The word `metadata.matchStrategy` carries.
    pub(super) strategy: String,
    pub(super) score: f64,
    /// The step that named the row, as `metadata.identification.rule` carries
    /// it: `provider-exact`, `qualified`, `token`, `canonical`,
    /// `custom-provider` or `merge`.
    pub(super) rule: &'static str,
    /// Step 3's display name settled several canonical hits.
    pub(super) narrowed: bool,
    /// For a merge, the row each adopted limit came from. Empty for every step
    /// that named one row, whose own declaration supplies both limits. A
    /// projection keeps only those its entry did not declare itself.
    pub(super) suppliers: Vec<FilledLimit>,
}

impl Lookup {
    fn new(
        provider_id: String,
        entry: ModelEntry,
        strategy: String,
        score: f64,
        rule: &'static str,
        narrowed: bool,
    ) -> Self {
        Self {
            provider_id,
            entry,
            strategy,
            score,
            rule,
            narrowed,
            suppliers: Vec::new(),
        }
    }
}

/// The identification index, built once at load: every row the steps read,
/// its precomputed token tuple, canonical tokens and display tokens, and a
/// map from each token tuple to the rows that carry it, per list.
pub(super) struct IdIndex {
    vendor_tokens: HashMap<Vec<String>, Vec<usize>>,
    custom_tokens: HashMap<Vec<String>, Vec<usize>>,
    /// Every row by its whole id, lowercased, for the qualified step that runs
    /// before any step drops the namespace segments.
    qualified: HashMap<String, Vec<usize>>,
    rows: Vec<IndexedRow>,
}

/// One row of the index. `provider` and `model` address
/// `providers[provider].models[model]`. `index` is the list whose token and
/// name steps read the row; a row in neither list is only read by the merge.
struct IndexedRow {
    provider: usize,
    model: usize,
    index: Option<Index>,
    tokens: Vec<String>,
    /// The `:tag` the row's own id carries, lowercased.
    tag: Option<String>,
    canonical: Option<Vec<String>>,
    display: Vec<String>,
}

impl IdIndex {
    pub(super) fn new(providers: &[CatalogProvider]) -> Self {
        let mut vendor_tokens: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
        let mut custom_tokens: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
        let mut qualified: HashMap<String, Vec<usize>> = HashMap::new();
        let mut rows = Vec::new();
        for (provider, catalog) in providers.iter().enumerate() {
            let role = catalog.metadata.as_ref().and_then(|metadata| metadata.role);
            let role_is_vendor = role == Some(ProviderRole::Vendor);
            let custom = catalog.provider_id == "custom-provider";
            // Every row is indexed: the qualified step reads every row's whole
            // id, steps 2 and 3 read vendor rows, step 4 the `custom-provider`
            // rows, and step 7 every other aggregator row.
            for (model, entry) in catalog.models.iter().enumerate() {
                let model_tokens = split_id(&entry.model_id);
                let position = rows.len();
                if role_is_vendor {
                    vendor_tokens
                        .entry(model_tokens.clone())
                        .or_default()
                        .push(position);
                } else if custom {
                    custom_tokens
                        .entry(model_tokens.clone())
                        .or_default()
                        .push(position);
                }
                qualified
                    .entry(entry.model_id.trim().to_lowercase())
                    .or_default()
                    .push(position);
                rows.push(IndexedRow {
                    provider,
                    model,
                    index: if role_is_vendor {
                        Some(Index::Vendor)
                    } else if custom {
                        Some(Index::CustomProvider)
                    } else {
                        None
                    },
                    tag: entry
                        .model_id
                        .rsplit_once(':')
                        .map(|(_, tag)| tag.to_lowercase()),
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
            qualified,
            rows,
        }
    }

    /// The rows whose own id is the input's, whole, as positions.
    fn qualified_rows(&self, id: &str) -> &[usize] {
        self.qualified
            .get(&id.trim().to_lowercase())
            .map(Vec::as_slice)
            .unwrap_or_default()
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
            if row.index != Some(index) {
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
            Outcome::One(found) => Ok(Some(*found)),
            Outcome::Ambiguous(candidates) => Err(candidates),
            Outcome::None => Ok(None),
        }
    }

    fn identify_steps(&self, provider: &ProviderProjection, id: &str) -> Outcome {
        if let Some((provider_id, entry)) = self.own_row(provider, id) {
            let strategy = Rule::ProviderExact.strategy(&provider_id, &entry.model_id, id);
            return self.one(provider_id, entry, strategy, Rule::ProviderExact);
        }
        let Some(query) = Query::parse(id) else {
            return Outcome::None;
        };
        let namespace = namespace_of(id);
        match self.identify_qualified(id, namespace) {
            Outcome::None => {}
            found => return found,
        }
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
        match self.identify_by_names_in(Index::CustomProvider, &query, provider, namespace, id) {
            Outcome::None => {}
            found => return found,
        }
        self.merge_rows(id, &query)
    }

    /// Step 2: the whole qualified id, before any step drops its namespace
    /// segments. An aggregator's own house model is named this way, so a router
    /// is never answered by the unrelated row its final token happens to name.
    ///
    /// The namespace is what makes the id an identity here: the step answers
    /// only when the namespace names the provider whose row carries the whole
    /// id. Without that, the id is a plain name for a model several providers
    /// sell, and the steps after this one read it.
    fn identify_qualified(&self, id: &str, namespace: Option<&str>) -> Outcome {
        // The identity this step reads is the id's own namespace, so an id
        // without one has nothing here.
        let Some(namespace) = namespace else {
            return Outcome::None;
        };
        let rows = self.rows_in(self.ids.qualified_rows(id));
        let answering: Vec<Row<'_>> = rows
            .into_iter()
            .filter(|row| row.provider_id == namespace)
            .collect();
        let [row] = answering.as_slice() else {
            // Nothing under the namespace carries it, or several of its rows
            // still disagree: either way this step named no one model.
            return match answering.len() {
                0 => Outcome::None,
                _ => Outcome::Ambiguous(
                    answering
                        .iter()
                        .map(|row| row.candidate(Rule::Qualified))
                        .collect(),
                ),
            };
        };
        let strategy = Rule::Qualified.strategy(row.provider_id, &row.entry.model_id, id);
        self.one(
            row.provider_id.to_owned(),
            row.entry.clone(),
            strategy,
            Rule::Qualified,
        )
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

    /// One row answered, with the strategy word, score and step that describe
    /// how.
    fn one(&self, provider_id: String, entry: ModelEntry, strategy: String, rule: Rule) -> Outcome {
        let narrowed = matches!(rule, Rule::Canonical { narrowed: true });
        Outcome::One(Box::new(Lookup::new(
            provider_id,
            entry,
            strategy,
            rule.score(),
            rule.word(),
            narrowed,
        )))
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
    /// declares no limit is still a candidate: the fill supplies the pair, and
    /// refusing it here would send an id whose only row declares nothing to the
    /// merge, where nothing else names it.
    ///
    /// Every token, canonical-id and display-name answer passes through here,
    /// so the namespace binding is enforced once for all of them: when the
    /// input's namespace names a bundled provider and none of the rows this
    /// step found belongs to it, the id is not the other seller's model and
    /// the refusal names the rows that would have answered.
    fn settle(
        &self,
        rows: Vec<Row<'_>>,
        provider: &ProviderProjection,
        namespace: Option<&str>,
        rule: Rule,
        query: &Query,
        input: &str,
    ) -> Outcome {
        if rows.is_empty() {
            return Outcome::None;
        }
        let known_namespace = namespace.filter(|namespace| self.provider(namespace).is_some());
        if let Some(namespace) = known_namespace
            && !rows.iter().any(|row| row.provider_id == namespace)
        {
            return Outcome::Ambiguous(rows.iter().map(|row| row.candidate(rule)).collect());
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
            rule,
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
    /// The bundled row an id names. Identification runs first; the merge
    /// serves an id that no step named.
    pub(super) fn lookup(&self, provider: &ProviderProjection, id: &str) -> Option<Lookup> {
        match self.identify(provider, id) {
            Ok(Some(found)) => Some(found),
            // Several rows claim the id and disagree, so none may supply
            // numbers; the merge is not allowed to answer something weaker
            // than the conflict it would be hiding.
            Err(_) | Ok(None) => None,
        }
    }

    /// Step 7: every aggregator row whose own id names the same model with the
    /// same tag joins, and only the safe direction is taken -- the smallest
    /// window, the smallest output cap and the intersection of the efforts.
    /// A row whose pair is its provider's default contributes to neither
    /// minimum, but it still answers when it is the only row.
    fn merge_rows(&self, id: &str, query: &Query) -> Outcome {
        let input_tag = id.rsplit_once(':').map(|(_, tag)| tag.to_lowercase());
        let mut joining: Vec<Row<'_>> = Vec::new();
        let mut declaring: Vec<Row<'_>> = Vec::new();
        for row in &self.ids.rows {
            let provider_id = self.providers[row.provider].provider_id.as_str();
            let entry = &self.providers[row.provider].models[row.model];
            if !same_model_with_tag(
                &row.tokens,
                row.tag.as_deref(),
                &query.tokens,
                input_tag.as_deref(),
            ) {
                continue;
            }
            let row = Row {
                provider_id,
                entry,
                tokens: &row.tokens,
            };
            joining.push(row);
            // A provider's placeholder pair is its default, not a declaration,
            // so the row contributes nothing to the minimum; neither does a row
            // that declares no limit at all.
            if !is_placeholder(provider_id, entry) && entry.capabilities.missing_limits().is_empty()
            {
                declaring.push(row);
            }
        }
        if joining.is_empty() {
            return Outcome::None;
        }
        let first = joining[0];
        let mut entry = first.entry.clone();
        // The minimum over the rows that declare a limit. When none does, the
        // joining row's own pair is the only number there is and answers as it
        // does today, so the id is not left unselectable for want of a
        // declaration that no seller ever made.
        let fallback = declaring.is_empty().then_some(&first.entry.capabilities);
        let window = minimum(&declaring, |capabilities| capabilities.context_window)
            .or_else(|| Some((first, fallback?.context_window?)));
        let output = minimum(&declaring, |capabilities| capabilities.max_output_tokens)
            .or_else(|| Some((first, fallback?.max_output_tokens?)));
        let efforts = intersection(declaring.iter());
        entry.capabilities.context_window = window.map(|(_, value)| value);
        entry.capabilities.max_output_tokens = output.map(|(_, value)| value);
        // The intersection is the merged entry's own statement, so it is
        // written even when the row the entry is copied from declares none.
        if let Some(efforts) = efforts {
            let metadata = entry.metadata.get_or_insert_default();
            metadata.reasoning.get_or_insert_default().effort_options = Some(efforts);
        }
        let suppliers = [("contextWindow", window), ("maxOutputTokens", output)]
            .into_iter()
            .filter_map(|(field, minimum)| {
                let (row, value) = minimum?;
                Some(FilledLimit {
                    field: field.into(),
                    value,
                    provider_id: row.provider_id.to_owned(),
                    model_id: row.entry.model_id.clone(),
                    rule: Rule::Merge.word().into(),
                })
            })
            .collect();
        Outcome::One(Box::new(Lookup {
            provider_id: first.provider_id.to_owned(),
            entry,
            strategy: Rule::Merge.strategy(first.provider_id, &first.entry.model_id, id),
            score: Rule::Merge.score(),
            rule: Rule::Merge.word(),
            narrowed: false,
            suppliers,
        }))
    }
}

/// The smallest value of one limit over the rows that declare it, with the row
/// that declared that value: the first joining row carrying it, so the record
/// names one seller rather than a set.
fn minimum<'a>(
    rows: &[Row<'a>],
    limit: impl Fn(&CatalogCapabilities) -> Option<u64>,
) -> Option<(Row<'a>, u64)> {
    rows.iter()
        .filter_map(|row| Some((*row, limit(&row.entry.capabilities)?)))
        .min_by_key(|(_, value)| *value)
}

/// Whether an aggregator row's tokens name the same model as the input, with
/// the same tag. A `:batch` row joins only a `:batch` input, and a `-fast` or
/// `-highspeed` row joins nothing because the tail check rejects both sides.
fn same_model_with_tag(
    row_tokens: &[String],
    row_tag: Option<&str>,
    input: &[String],
    input_tag: Option<&str>,
) -> bool {
    if row_tag != input_tag {
        return false;
    }
    if row_tokens == input {
        return true;
    }
    row_tokens.len() < input.len()
        && input[..row_tokens.len()] == *row_tokens
        && tail_allowed(&input[row_tokens.len()..])
}

/// The efforts every row that declares any agrees on, in the first row's
/// order. Empty when at least one row declares any and none of them agree.
fn intersection<'a>(rows: impl Iterator<Item = &'a Row<'a>>) -> Option<Vec<String>> {
    let declared: Vec<Vec<String>> = rows
        .filter_map(|row| {
            row.entry
                .metadata
                .as_ref()
                .and_then(|m| m.reasoning.as_ref())
                .and_then(|r| r.effort_options.clone())
        })
        .collect();
    let first = declared.first()?;
    Some(
        first
            .iter()
            .filter(|effort| declared.iter().all(|list| list.contains(effort)))
            .cloned()
            .collect(),
    )
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
