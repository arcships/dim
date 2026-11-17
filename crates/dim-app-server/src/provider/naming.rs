//! The model names one provider's catalog publishes. They are unique within
//! that provider and computed on every read, so nothing stores them.
use crate::model_catalog::ModelEntry;
use std::collections::{HashMap, HashSet};

pub(crate) use crate::model_catalog::name_inputs::{Name, keys};
use crate::model_catalog::name_inputs::{id_fallback, label_candidate};

impl From<&ModelEntry> for Name {
    fn from(entry: &ModelEntry) -> Self {
        Self {
            model_id: entry.model_id.clone(),
            display_name: entry.display_name.clone(),
            label: entry
                .metadata
                .as_ref()
                .and_then(|m| m.raw.get("magpie_label"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        }
    }
}

/// The name each of one provider's complete rows publishes, in their order,
/// each unique within that provider. Every row takes part — a disabled or an
/// unavailable one too — so switching a row on or off renames no other, and
/// every client reads the same names.
///
/// A name compares after trimming. A row whose name no other row states keeps
/// it as stated. A row that states a shared name publishes its label candidate
/// — `<its own name> · <the origin its gateway states for it>` — but only while
/// no other row reaches the same candidate and no row states that name itself.
/// Every other row of the group appends its id instead, so a label two rows
/// carry apart names neither of them; every computed name is built from trimmed
/// parts, the appended id included. A fallback no other row reaches and no row
/// states is kept as it stands, and every name kept — label or fallback — is
/// claimed before any counter is assigned, so a counter cannot take another
/// row's own name. The rows still unnamed then take counters in one canonical
/// order — by fallback, then by id, then by the name stated — so listing
/// position decides a name only between rows with the same id and the same
/// stated name. Everything else is read off the whole set before any name is
/// assigned, so listing the same rows in another order names each row the same.
pub(crate) fn unique<'a>(entries: impl IntoIterator<Item = &'a ModelEntry>) -> Vec<String> {
    let entries: Vec<_> = entries.into_iter().map(Name::from).collect();
    unique_names(&entries)
}

pub(crate) fn unique_names(entries: &[Name]) -> Vec<String> {
    let entries: Vec<&Name> = entries.iter().collect();
    let mut stated: HashMap<&str, usize> = HashMap::new();
    for entry in &entries {
        *stated.entry(entry.display_name.trim()).or_default() += 1;
    }
    let shared: Vec<bool> = entries
        .iter()
        .map(|entry| stated[entry.display_name.trim()] > 1)
        .collect();

    // A shared row's candidate, and how many rows reach it. Both are read from
    // the whole set first, so what one row publishes cannot depend on the order
    // the rows arrive in.
    let candidates: Vec<Option<String>> = entries
        .iter()
        .zip(&shared)
        .map(|(entry, shared)| shared.then(|| label_candidate(entry)).flatten())
        .collect();
    let reached = reached_by(&candidates);

    // Every name a row states is claimed: a computed name may not equal one
    // after trimming either, or two rows would publish one name.
    let mut claimed: HashSet<String> = stated.keys().map(|name| (*name).to_owned()).collect();
    let keeps_label: Vec<bool> = candidates
        .iter()
        .map(|candidate| kept_once(candidate.as_deref(), &reached, &claimed))
        .collect();
    // A name a row keeps is claimed before any counter is assigned, so a name
    // another row takes later cannot take it first.
    claim_kept(&candidates, &keeps_label, &mut claimed);

    // The id fallback of every row still unnamed, counted the same way and
    // claimed under the same rule. The id is trimmed only where it is
    // published; the stored id is untouched.
    let fallbacks: Vec<Option<String>> = entries
        .iter()
        .zip(&shared)
        .enumerate()
        .map(|(index, (entry, shared))| {
            (*shared && !keeps_label[index]).then(|| id_fallback(entry))
        })
        .collect();
    let reached = reached_by(&fallbacks);
    let keeps_fallback: Vec<bool> = fallbacks
        .iter()
        .map(|fallback| kept_once(fallback.as_deref(), &reached, &claimed))
        .collect();
    claim_kept(&fallbacks, &keeps_fallback, &mut claimed);

    let mut names: Vec<Option<String>> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            if !shared[index] {
                Some(entry.display_name.clone())
            } else if keeps_label[index] {
                candidates[index].clone()
            } else if keeps_fallback[index] {
                fallbacks[index].clone()
            } else {
                None
            }
        })
        .collect();
    // The rows left take counters in one canonical order, so only rows with the
    // same fallback, id and stated name are told apart by their position.
    let mut counting: Vec<usize> = (0..entries.len())
        .filter(|index| names[*index].is_none())
        .collect();
    counting.sort_by_key(|index| {
        let entry = entries[*index];
        (
            fallbacks[*index].clone().unwrap_or_default(),
            entry.model_id.clone(),
            entry.display_name.clone(),
            *index,
        )
    });
    for index in counting {
        let base = fallbacks[index]
            .as_deref()
            .expect("a row left for a counter has a fallback");
        let name = free(base, &claimed);
        claimed.insert(name.clone());
        names[index] = Some(name);
    }
    names
        .into_iter()
        .map(|name| name.expect("every row is named"))
        .collect()
}

/// How many rows reach each of the names they computed, none counted for a row
/// that computed none.
fn reached_by(names: &[Option<String>]) -> HashMap<&str, usize> {
    let mut reached: HashMap<&str, usize> = HashMap::new();
    for name in names.iter().flatten() {
        *reached.entry(name.as_str()).or_default() += 1;
    }
    reached
}

/// Whether a row keeps the name it computed: exactly one row reaches it and no
/// row states it as its own.
fn kept_once(
    name: Option<&str>,
    reached: &HashMap<&str, usize>,
    claimed: &HashSet<String>,
) -> bool {
    name.is_some_and(|name| reached[name] == 1 && !claimed.contains(name))
}

/// Claims every name the rows keep, before any row is left to a counter.
fn claim_kept(names: &[Option<String>], kept: &[bool], claimed: &mut HashSet<String>) {
    for (index, name) in names.iter().enumerate() {
        if kept[index] {
            claimed.insert(name.clone().expect("a kept name is present"));
        }
    }
}

/// `base`, or the first `<base> <n>` no row publishes yet.
fn free(base: &str, claimed: &HashSet<String>) -> String {
    if !claimed.contains(base) {
        return base.to_owned();
    }
    let mut count = 2;
    loop {
        let candidate = format!("{base} {count}");
        if !claimed.contains(&candidate) {
            return candidate;
        }
        count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// One listed row: its id, the name the listing states, and the label the
    /// gateway states for it when it states one.
    fn entry(id: &str, name: &str, label: Option<&str>) -> ModelEntry {
        let metadata = label.map(|label| json!({"raw": {"magpie_label": label}}));
        serde_json::from_value(json!({
            "modelId": id,
            "displayName": name,
            "status": "active",
            "capabilities": {"modalities": ["text"], "toolCalling": true,
                "streaming": true, "structuredOutput": true, "vision": false,
                "audio": false, "reasoning": false},
            "metadata": metadata,
        }))
        .unwrap()
    }

    /// The four rows a gateway returns for one model it serves over three
    /// channels: the group it builds over them, then one row per channel, all
    /// named alike and each labelled with where it comes from.
    fn gateway() -> Vec<ModelEntry> {
        vec![
            entry("auto/glm-5.3", "GLM-5.3", Some("GLM-5.3 · routing group")),
            entry(
                "siliconflow/glm-5.3",
                "GLM-5.3",
                Some("GLM-5.3 · SiliconFlow"),
            ),
            entry(
                "volcengine/glm-5.3",
                "GLM-5.3",
                Some("GLM-5.3 · Volcengine Ark"),
            ),
            entry("zhipu/glm-5.3", "GLM-5.3", Some("GLM-5.3 · Zhipu")),
        ]
    }

    /// Every listing order of one row set names each row the same, keyed by the
    /// id and the name it states, and no two rows publish one name after
    /// trimming. A row set with a repeated id and name cannot be checked this
    /// way: its rows are interchangeable, so their order carries no name.
    fn assert_every_order_names_rows_alike(entries: &[ModelEntry]) {
        fn permutations(count: usize) -> Vec<Vec<usize>> {
            let rest = match count.checked_sub(1) {
                Some(rest) => permutations(rest),
                None => return vec![Vec::new()],
            };
            let first = count - 1;
            let mut orders = Vec::new();
            for order in rest {
                for at in 0..=order.len() {
                    let mut order = order.clone();
                    order.insert(at, first);
                    orders.push(order);
                }
            }
            orders
        }

        fn agreed_names(
            entries: &[ModelEntry],
            order: &[usize],
        ) -> BTreeMap<(String, String), String> {
            let rows: Vec<&ModelEntry> = order.iter().map(|index| &entries[*index]).collect();
            unique(rows.iter().copied())
                .into_iter()
                .zip(&rows)
                .map(|(name, entry)| ((entry.model_id.clone(), entry.display_name.clone()), name))
                .collect()
        }

        let listed: Vec<usize> = (0..entries.len()).collect();
        let expected = agreed_names(entries, &listed);
        assert_eq!(
            expected.len(),
            entries.len(),
            "the rows must state ids and names of their own for this check"
        );
        for order in permutations(entries.len()) {
            let names = agreed_names(entries, &order);
            assert_eq!(
                names, expected,
                "the same rows in another order name each row the same"
            );
            let distinct: HashSet<&str> = names.values().map(|name| name.trim()).collect();
            assert_eq!(
                distinct.len(),
                names.len(),
                "no two rows publish one name: {names:?}"
            );
        }
    }

    #[test]
    fn a_gateways_rows_take_their_labels_apart() {
        let names = unique(&gateway());
        assert_eq!(
            names,
            [
                "GLM-5.3 · routing group",
                "GLM-5.3 · SiliconFlow",
                "GLM-5.3 · Volcengine Ark",
                "GLM-5.3 · Zhipu",
            ],
            "the group row is told apart from the channels it spreads over"
        );
        assert_eq!(
            names.iter().collect::<HashSet<_>>().len(),
            4,
            "four rows, four names"
        );
        assert_every_order_names_rows_alike(&gateway());
    }

    #[test]
    fn a_provider_without_duplicates_keeps_every_name() {
        let entries = vec![
            entry("flash", "Flash", Some("Flash · Dim")),
            entry("plain", "Plain", None),
            entry("spaced", " Spaced ", Some("Spaced · Dim")),
        ];
        assert_eq!(
            unique(&entries),
            ["Flash", "Plain", " Spaced "],
            "no name is rewritten, the one with space around it included"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn names_that_agree_only_after_trimming_are_duplicates() {
        let entries = vec![
            entry("a", "GLM-5.3", Some("GLM-5.3 · SiliconFlow")),
            entry("b", " GLM-5.3 ", Some("GLM-5.3 · Zhipu")),
        ];
        assert_eq!(
            unique(&entries),
            ["GLM-5.3 · SiliconFlow", "GLM-5.3 · Zhipu"],
            "the two state one name, so both are qualified"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_row_nothing_can_choose_still_counts_as_a_duplicate() {
        let mut entries = gateway();
        let stated = unique(&entries);
        // Rows no picker offers: one its status excludes, one a stored disable
        // set does. The rule reads neither.
        entries[2].status = crate::model_catalog::ModelStatus::Unavailable;
        assert_eq!(
            unique(&entries),
            stated,
            "a row's status changes no name, its own included"
        );
    }

    #[test]
    fn rows_whose_labels_collide_all_take_their_ids() {
        let entries = vec![
            entry(
                "siliconflow/glm-5.3",
                "GLM-5.3",
                Some("GLM-5.3 · SiliconFlow"),
            ),
            entry("proxy/glm-5.3", "GLM-5.3", Some("GLM-5.3 · SiliconFlow")),
        ];
        assert_eq!(
            unique(&entries),
            ["GLM-5.3 · siliconflow/glm-5.3", "GLM-5.3 · proxy/glm-5.3",],
            "both rows carry the label, so it names neither and both take ids"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn ids_that_agree_after_trimming_take_one_name() {
        let entries = vec![entry("a", "GLM-5.3", None), entry("a ", "GLM-5.3", None)];
        assert_eq!(
            unique(&entries),
            ["GLM-5.3 · a", "GLM-5.3 · a 2"],
            "the ids differ only in space, so the second appends the first one's name"
        );
        assert_eq!(
            unique(&entries).len(),
            2,
            "two rows, two names, whichever id a row states"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_counter_cannot_take_another_rows_id_name() {
        let entries = vec![
            entry("a", "GLM-5.3", None),
            entry("a 2", "GLM-5.3", None),
            entry("c", "GLM-5.3 · a", None),
        ];
        assert_eq!(
            unique(&entries),
            ["GLM-5.3 · a 3", "GLM-5.3 · a 2", "GLM-5.3 · a"],
            "the counter skips the id names the other rows publish"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_stated_name_that_differs_only_by_trimming_still_collides() {
        let entries = vec![
            entry("a", "GLM-5.3", None),
            entry("b", "GLM-5.3", None),
            entry("c", "GLM-5.3 · b ", None),
        ];
        assert_eq!(
            unique(&entries),
            ["GLM-5.3 · a", "GLM-5.3 · b 2", "GLM-5.3 · b "],
            "the row stating the name keeps it, space and all, so the id the \
             other row would take is already read as taken"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_row_without_a_label_takes_its_id() {
        let entries = vec![
            entry("flash", "Flash", Some("Flash · Dim")),
            entry("other", "Flash", None),
        ];
        assert_eq!(
            unique(&entries),
            ["Flash · Dim", "Flash · other"],
            "a provider that states no origin still names each row apart"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_renamed_row_keeps_its_own_name() {
        let entries = vec![
            entry(
                "siliconflow/x",
                "V4.1 Flash (SiliconFlow)",
                Some("V4.1 Flash (SiliconFlow) · SiliconFlow"),
            ),
            entry(
                "volcengine/x",
                "V4.1 Flash (SiliconFlow)",
                Some("V4.1 Flash (Volcengine) · Volcengine"),
            ),
        ];
        assert_eq!(
            unique(&entries),
            [
                "V4.1 Flash (SiliconFlow) · SiliconFlow",
                "V4.1 Flash (SiliconFlow) · Volcengine",
            ],
            "the name the user chose stays in front; only the origin is added"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_name_another_row_states_survives_as_it_stands() {
        let entries = vec![
            entry("a", "GLM-5.3", None),
            entry("b", "GLM-5.3", None),
            entry("c", "GLM-5.3 · b", None),
        ];
        let names = unique(&entries);
        assert_eq!(
            names,
            ["GLM-5.3 · a", "GLM-5.3 · b 2", "GLM-5.3 · b"],
            "the row that states one of the qualified names keeps it"
        );
        assert_eq!(
            names.iter().collect::<HashSet<_>>().len(),
            3,
            "the qualified names stay apart"
        );
        assert_every_order_names_rows_alike(&entries);
    }

    #[test]
    fn a_row_whose_id_is_taken_takes_a_counter() {
        let entries = vec![
            entry("b", "GLM-5.3", None),
            entry("b", "GLM-5.3", None),
            entry("b", "GLM-5.3", None),
        ];
        assert_eq!(
            unique(&entries),
            ["GLM-5.3 · b", "GLM-5.3 · b 2", "GLM-5.3 · b 3"],
            "ids name one row each, so only a repeated id needs the counter"
        );
    }
}
