//! The `/skill:<name>` token, in one place: where one may start, which
//! characters a name holds, and the spans a text carries. The Skills submit
//! check, the CLI and web composers and the CLI, web and ACP transcript
//! projections all read this definition, so every surface agrees on what loads
//! and on what is drawn as one unit ([Skills §9](../../../docs/design/plugins/Skills.md#9-references-load-with-the-operators-input)).
//!
//! A token is `/skill:` and then one or more name characters, with no space
//! after the colon. It starts a text or follows whitespace, so it may stand
//! mid-sentence and never starts inside a word: `a/skill:x` and
//! `https://host/skill:y` carry none. A name never ends with a full stop, so a
//! sentence's final stop stays the operator's text: `/skill:review.` names
//! `review` and keeps its full stop.

/// What every token opens with.
pub const PREFIX: &str = "/skill:";

/// Whether `c` may stand in a Skill name a token carries: `A-Z`, `a-z`, `0-9`,
/// `_`, `-` and `.`, which never ends the name. A Skill whose name holds any
/// other character, or ends with a full stop, cannot be referenced and is not
/// offered by a palette.
pub fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Whether `name` is a Skill name a token can carry. A name ending in a full
/// stop cannot be one, because the token ends there.
pub fn names_a_token(name: &str) -> bool {
    !name.is_empty() && !name.ends_with('.') && name.chars().all(is_name_char)
}

/// One token a text carries: the byte range it occupies and its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub start: usize,
    pub end: usize,
    pub name: &'a str,
}

/// The tokens `text` carries, in the order they appear.
pub fn tokens(text: &str) -> std::vec::IntoIter<Token<'_>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(PREFIX) {
        let start = from + at;
        let after = start + PREFIX.len();
        let rest = &text[after..];
        let end = after + rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
        // A full stop between other characters stays in the name; one that would
        // end it is the sentence's own and stays text.
        let end = after + text[after..end].trim_end_matches('.').len();
        let leads = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if leads && end > after {
            found.push(Token {
                start,
                end,
                name: &text[after..end],
            });
            from = end;
        } else {
            from = after;
        }
    }
    found.into_iter()
}

#[cfg(test)]
mod tests {
    use super::{Token, names_a_token, tokens};

    fn found(text: &str) -> Vec<Token<'_>> {
        tokens(text).collect()
    }

    fn names(text: &str) -> Vec<&str> {
        found(text).into_iter().map(|token| token.name).collect()
    }

    #[test]
    fn a_token_opens_a_text_or_follows_whitespace() {
        assert_eq!(names("/skill:commit"), ["commit"]);
        assert_eq!(names("/skill:commit please review"), ["commit"]);
        assert_eq!(names("please /skill:commit it"), ["commit"]);
        assert_eq!(names("one\n/skill:commit"), ["commit"]);
        assert_eq!(
            names("/skill:alpha then /skill:beta"),
            ["alpha", "beta"],
            "a text carries every token, in order"
        );
        let found = found("hi /skill:commit");
        let [token] = found.as_slice() else {
            panic!("one token");
        };
        assert_eq!(&"hi /skill:commit"[token.start..token.end], "/skill:commit");
    }

    #[test]
    fn a_name_is_the_run_of_name_characters() {
        assert_eq!(names("/skill:a-b_c.d1"), ["a-b_c.d1"]);
        assert_eq!(
            names("/skill:two words"),
            ["two"],
            "a name ends at whitespace, whatever follows stays text"
        );
        assert_eq!(
            names("/skill:commit."),
            ["commit"],
            "a sentence's full stop stays text"
        );
        assert_eq!(
            names("/skill:a.b"),
            ["a.b"],
            "a full stop inside a name stays in it"
        );
        assert_eq!(
            names("/skill:a.."),
            ["a"],
            "trailing full stops end the token"
        );
        assert_eq!(
            names("/skill:."),
            [] as [&str; 0],
            "a lone full stop is no name"
        );
        assert_eq!(names("/skill:naïve"), ["na"]);
        assert_eq!(
            names("/skills:x"),
            [] as [&str; 0],
            "another slash command is not a Skill reference"
        );
        assert_eq!(names("/skill"), [] as [&str; 0]);
        assert_eq!(names("/skill:<dim-skill>"), [] as [&str; 0]);
        assert!(names_a_token("a-b_c.d1"));
        assert!(!names_a_token("two words"));
        assert!(!names_a_token(""));
        assert!(!names_a_token("naïve"));
    }
}
