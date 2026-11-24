//! The exact version the shipped `plugin-authoring` Skill gives the SDK.
//!
//! The Skill teaches an author to install the SDK into the plugin's own package,
//! and rutis refuses a peer that speaks another session protocol at the
//! handshake (ADR 025), so the spec the Skill's `package.json` example gives for
//! `@arcships/rutis` is the one `runtime/package.json` declares.
//!
//! The Skill names that one version and no other: the install example carries
//! it, and a note about another release states the history without a version
//! number. The prose scan below therefore refuses every other version instead
//! of guessing which ones were meant as history.

use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const SKILL: &str = "runtime/plugins/plugin-authoring/skills/plugin-authoring/SKILL.md";
const SDK: &str = "@arcships/rutis";

/// The objects in `text`'s fenced `json` blocks, in order.
fn json_examples(text: &str) -> Result<Vec<serde_json::Value>> {
    let mut examples = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "```json" {
            continue;
        }
        let mut block = String::new();
        for line in lines.by_ref() {
            if line.trim() == "```" {
                break;
            }
            block.push_str(line);
            block.push('\n');
        }
        examples.push(
            serde_json::from_str(&block).map_err(|error| {
                format!("{SKILL}: a fenced json example does not parse: {error}")
            })?,
        );
    }
    Ok(examples)
}

/// The whole spec every JSON example gives the SDK, such as `0.23.0`.
fn example_specs(examples: &[serde_json::Value]) -> Result<Vec<&str>> {
    let mut specs = Vec::new();
    for example in examples {
        let Some(dependencies) = example["dependencies"].as_object() else {
            continue;
        };
        let Some(spec) = dependencies.get(SDK) else {
            continue;
        };
        let spec = spec.as_str().ok_or_else(|| {
            format!(
                "{SKILL}: a fenced json example gives {SDK} a spec that is not a string: {spec}"
            )
        })?;
        specs.push(spec);
    }
    Ok(specs)
}

/// Every version `text` gives the SDK, such as `0.23.0`.
fn versions(text: &str) -> Vec<&str> {
    let mut named = Vec::new();
    for (at, _) in text.match_indices(SDK) {
        let after = &text[at + SDK.len()..];
        if after.starts_with(|c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '/')) {
            continue;
        }
        let rest = after.trim_start_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '`' | ':' | '=' | '@' | '|' | '^' | '~' | ' ' | '\t'
            )
        });
        let rest = rest.strip_prefix('v').unwrap_or(rest);
        if let Some(version) = version_at(rest)
            && !named.contains(&version)
        {
            named.push(version);
        }
    }
    named
}

/// The three-part version at the start of `text`, such as `0.23.0`.
fn version_at(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut end = 0;
    for part in 0..3 {
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            return None;
        }
        if part < 2 {
            if bytes.get(end) != Some(&b'.') {
                return None;
            }
            end += 1;
        }
    }
    Some(&text[..end])
}

/// Whether `version` pins all three components, such as `0.23.0`.
fn is_exact(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

pub fn check(root: &Path) -> Result<()> {
    let runtime: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("runtime/package.json"))?)?;
    let expected = runtime["devDependencies"][SDK]
        .as_str()
        .ok_or("runtime/package.json declares no @arcships/rutis version")?;
    if !is_exact(expected) {
        return Err(format!(
            "runtime/package.json must pin {SDK} to an exact X.Y.Z version, not {expected}"
        )
        .into());
    }
    let skill = std::fs::read_to_string(root.join(SKILL))?;

    let examples = json_examples(&skill)?;
    let specs = example_specs(&examples)?;
    if specs.is_empty() {
        return Err(format!(
            "{SKILL}: no fenced json example declares {SDK}; runtime/package.json declares {expected}"
        )
        .into());
    }
    let mut unpinned: Vec<&str> = Vec::new();
    for spec in specs {
        if spec != expected && !unpinned.contains(&spec) {
            unpinned.push(spec);
        }
    }
    if !unpinned.is_empty() {
        return Err(format!(
            "{SKILL}: the json example gives {SDK} {}; runtime/package.json declares {expected}",
            unpinned.join(", ")
        )
        .into());
    }

    let mut stale: Vec<&str> = Vec::new();
    for version in versions(&skill) {
        if version != expected && !stale.contains(&version) {
            stale.push(version);
        }
    }
    if !stale.is_empty() {
        return Err(format!(
            "{SKILL}: names {}; runtime/package.json declares {expected}. \
             This Skill names only that version, the install example carries it, \
             and a note about another release leaves the version out",
            stale.join(", ")
        )
        .into());
    }
    println!("plugin skill versions passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree whose runtime manifest declares 0.23.0 and whose Skill is `skill`.
    fn tree(name: &str, skill: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dim-xtask-plugin-skill-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(SKILL).parent().unwrap()).unwrap();
        std::fs::write(
            root.join("runtime/package.json"),
            r#"{"devDependencies":{"@arcships/rutis":"0.23.0"}}"#,
        )
        .unwrap();
        std::fs::write(root.join(SKILL), skill).unwrap();
        root
    }

    /// A Skill whose `package.json` example gives `spec`, after `prose`.
    fn skill(prose: &str, spec: &str) -> String {
        format!(
            "{prose}\n```json\n{{ \"dependencies\": {{ \"@arcships/rutis\": \"{spec}\" }} }}\n```\n"
        )
    }

    /// The error `check` gives for a tree whose Skill is `skill`.
    fn error_for(name: &str, skill: &str) -> String {
        let root = tree(name, skill);
        let error = check(&root).unwrap_err().to_string();
        std::fs::remove_dir_all(root).unwrap();
        error
    }

    /// Asserts that `check` accepts a tree whose Skill is `skill`.
    fn passes(name: &str, skill: &str) {
        let root = tree(name, skill);
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_runtime_version_passes() {
        passes(
            "current",
            &skill(
                concat!(
                    "The SDK version must be exactly the one Dim runs: ",
                    "@arcships/rutis 0.23.0.\n\n",
                    "```ts\nimport { definePlugin } from '@arcships/rutis'\n```\n",
                ),
                "0.23.0",
            ),
        );
    }

    #[test]
    fn a_spec_the_example_does_not_pin_is_named() {
        for spec in ["0.23.0-rc.1", "0.23.0 || 0.24.0", "^0.23.0"] {
            let error = error_for(
                "unpinned",
                &skill("The SDK version must be exactly the one Dim runs.\n", spec),
            );
            assert!(
                error.contains(&format!(
                    "the json example gives @arcships/rutis {spec}; runtime/package.json declares 0.23.0"
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn an_example_that_pins_another_exact_version_is_named() {
        let error = error_for(
            "stale-example",
            &skill("Install @arcships/rutis 0.23.0 today.\n", "0.17.1"),
        );
        assert!(
            error.contains(
                "the json example gives @arcships/rutis 0.17.1; runtime/package.json declares 0.23.0"
            ),
            "{error}"
        );
    }

    #[test]
    fn an_example_that_does_not_give_the_dependency_is_refused() {
        for (text, expected) in [
            (
                "Install @arcships/rutis 0.23.0.\n",
                "no fenced json example declares",
            ),
            (
                "```json\n{ \"name\": \"echo-plugin\" }\n```\n",
                "no fenced json example declares",
            ),
            (
                "```json\n{ \"dependencies\": { \"zod\": \"4.6.7\" } }\n```\n",
                "no fenced json example declares",
            ),
            (
                "```json\n{ \"dependencies\": { \"@arcships/rutis\": \"0.23.0\" }\n```\n",
                "does not parse",
            ),
            (
                "```json\n{ \"dependencies\": { \"@arcships/rutis\": { \"version\": \"0.23.0\" } } }\n```\n",
                "not a string",
            ),
        ] {
            let error = error_for("no-example", text);
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn a_version_the_prose_gives_is_named() {
        let error = error_for(
            "prose-stale",
            &skill("Install @arcships/rutis 0.17.1 today.\n", "0.23.0"),
        );
        assert!(
            error.contains("names 0.17.1; runtime/package.json declares 0.23.0"),
            "{error}"
        );
    }

    #[test]
    fn a_prose_version_says_the_skill_names_only_the_current_one() {
        let error = error_for(
            "prose-rule",
            &skill("Install @arcships/rutis 0.17.1 today.\n", "0.23.0"),
        );
        assert!(
            error.contains(concat!(
                "This Skill names only that version, the install example carries it, ",
                "and a note about another release leaves the version out"
            )),
            "{error}"
        );
    }

    #[test]
    fn a_version_that_ends_a_sentence_is_named() {
        let error = error_for(
            "sentence",
            &skill(
                "The SDK version must be exactly the one Dim runs: @arcships/rutis 0.17.1.\n",
                "0.23.0",
            ),
        );
        assert!(error.contains("names 0.17.1;"), "{error}");
    }

    #[test]
    fn a_prose_version_next_to_the_name_is_named() {
        for prose in [
            "| @arcships/rutis | 0.17.1 |\n",
            "Install @arcships/rutis@^0.17.1.\n",
            "Install @arcships/rutis v0.17.1.\n",
        ] {
            let error = error_for("prose-forms", &skill(prose, "0.23.0"));
            assert!(error.contains("names 0.17.1;"), "{prose}: {error}");
        }
    }

    #[test]
    fn versions_given_for_other_packages_are_ignored() {
        passes(
            "other-packages",
            concat!(
                "```json\n",
                "{ \"name\": \"echo-plugin\", \"version\": \"1.0.0\",\n",
                "  \"engines\": { \"node\": \">=20.19.0\" },\n",
                "  \"dependencies\": {\n",
                "    \"@arcships/rutis\": \"0.23.0\",\n",
                "    \"@arcships/rutis-host\": \"0.22.0\",\n",
                "    \"@modelcontextprotocol/sdk\": \"1.31.0\",\n",
                "    \"zod\": \"4.6.7\"\n",
                "  } }\n",
                "```\n",
            ),
        );
    }

    #[test]
    fn a_runtime_manifest_that_does_not_pin_an_exact_version_is_refused() {
        for declared in ["^0.23.0", "~0.23.0", ">=0.23.0"] {
            let root = tree(
                "range",
                "```json\n{ \"dependencies\": { \"@arcships/rutis\": \"0.23.0\" } }\n```\n",
            );
            std::fs::write(
                root.join("runtime/package.json"),
                format!(r#"{{"devDependencies":{{"@arcships/rutis":"{declared}"}}}}"#),
            )
            .unwrap();
            let error = check(&root).unwrap_err().to_string();
            std::fs::remove_dir_all(root).unwrap();
            assert!(
                error.contains(&format!(
                    "must pin @arcships/rutis to an exact X.Y.Z version, not {declared}"
                )),
                "{error}"
            );
        }
    }
}
