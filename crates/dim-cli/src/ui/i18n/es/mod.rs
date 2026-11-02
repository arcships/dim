//! Spanish, keyed by the English text, one table per area of the interface.

mod chrome;
mod management;
mod plugins;
mod prompts;
mod providers;

pub fn lookup(english: &str) -> Option<&'static str> {
    chrome::lookup(english)
        .or_else(|| prompts::lookup(english))
        .or_else(|| providers::lookup(english))
        .or_else(|| plugins::lookup(english))
        .or_else(|| management::lookup(english))
}
