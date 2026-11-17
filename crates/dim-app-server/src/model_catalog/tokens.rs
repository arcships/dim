/// A model id without its namespace segment, tag and bracket modifier, lowercased and split.
pub(crate) fn split_id(id: &str) -> Vec<String> {
    let lower = id.trim().to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let name = name.rsplit_once(':').map_or(name, |(head, _)| head);
    let name = if name.ends_with(']') {
        name.rfind('[').map_or(name, |start| &name[..start])
    } else {
        name
    };
    split_tokens(name)
}

/// A display name, lowercased and split.
pub(crate) fn split_display(display: &str) -> Vec<String> {
    split_tokens(&display.to_lowercase())
}

pub(crate) fn split_tokens(name: &str) -> Vec<String> {
    name.split(['-', '_', '.', ' '])
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}
