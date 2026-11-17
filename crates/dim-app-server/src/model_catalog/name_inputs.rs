pub(crate) struct Name {
    pub model_id: String,
    pub display_name: String,
    pub label: Option<String>,
}

pub(crate) fn keys(name: &Name) -> std::collections::BTreeSet<String> {
    let mut keys =
        std::collections::BTreeSet::from([name.display_name.trim().to_owned(), id_fallback(name)]);
    if let Some(candidate) = label_candidate(name) {
        keys.insert(candidate);
    }
    let mut keys: std::collections::BTreeSet<_> = keys
        .into_iter()
        .map(|mut key| {
            while key.contains(SEPARATOR)
                && let Some((head, tail)) = key.rsplit_once(' ')
            {
                if tail
                    .parse::<u64>()
                    .is_ok_and(|n| n >= 2 && tail == n.to_string())
                {
                    key = head.to_owned();
                } else {
                    break;
                }
            }
            key
        })
        .collect();
    keys.insert(format!("=\0{}", name.display_name.trim()));
    keys
}

const SEPARATOR: &str = " · ";

pub(crate) fn label_candidate(entry: &Name) -> Option<String> {
    let (_, qualifier) = entry.label.as_deref()?.rsplit_once(SEPARATOR)?;
    let qualifier = qualifier.trim();
    if qualifier.is_empty() {
        return None;
    }
    Some(format!(
        "{}{SEPARATOR}{qualifier}",
        entry.display_name.trim()
    ))
}

pub(crate) fn id_fallback(entry: &Name) -> String {
    format!(
        "{}{SEPARATOR}{}",
        entry.display_name.trim(),
        entry.model_id.trim()
    )
}
