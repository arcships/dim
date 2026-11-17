//! The label this build reports as its identity: the version the release flow
//! set for it, or the committed version marked `-source` and naming the commit
//! it was built from. Display only: it must never be ordered or compared as a
//! version, which is what [`VERSION`] is for.

use std::sync::OnceLock;

/// The workspace version; the release flow sets it for the builds it produces.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether the release flow produced this build, which it marks with
/// `DIM_RELEASE_BUILD`. CI exports an unset variable as empty, so an empty
/// mark is no mark.
pub const RELEASE: bool = stamped(option_env!("DIM_RELEASE_BUILD"));

/// Whether `value` marks the build: an empty value is no mark.
const fn stamped(value: Option<&'static str>) -> bool {
    matches!(value, Some(value) if !value.is_empty())
}

/// The marker of a build the release flow did not produce.
const SOURCE: &str = "-source";

/// The commit the final binary crate read at build time; `None` for a build
/// with no git to answer, which reports the bare `SOURCE` mark.
static COMMIT: OnceLock<Option<&'static str>> = OnceLock::new();

/// Hands this build the commit the final binary crate read at build time. The
/// binary calls this once before it serves either role, so every label in the
/// process — the client's, the daemon's reports and its MCP identity — names
/// the commit it runs. Nothing here reads git or the environment, so a new
/// commit never recompiles this crate. The first call wins.
pub fn install(commit: Option<&'static str>) {
    let _ = COMMIT.set(commit);
}

/// `version` for a released build; otherwise the same version marked `SOURCE`,
/// followed by `+commit` when `commit` names the tree it was built from.
pub fn label(version: &str, release: bool, commit: Option<&str>) -> String {
    match (release, commit) {
        (true, _) => version.to_owned(),
        (false, Some(commit)) => format!("{version}{SOURCE}+{commit}"),
        (false, None) => format!("{version}{SOURCE}"),
    }
}

/// What this build reports.
pub fn current() -> String {
    label(VERSION, RELEASE, COMMIT.get().copied().flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_build_reports_its_version_alone() {
        assert_eq!(label("0.1.0-alpha.8", true, None), "0.1.0-alpha.8");
        assert_eq!(
            label("0.1.0-alpha.8", true, Some("02ebd4a1")),
            "0.1.0-alpha.8",
            "a release build never carries the -source mark or a commit"
        );
    }

    #[test]
    fn a_source_build_marks_itself_and_names_its_commit_when_it_has_one() {
        assert_eq!(
            label("0.1.0-alpha.8", false, Some("02ebd4a1")),
            "0.1.0-alpha.8-source+02ebd4a1"
        );
        assert_eq!(label("0.1.0-alpha.8", false, None), "0.1.0-alpha.8-source");
    }

    #[test]
    fn an_empty_release_mark_is_no_mark() {
        assert!(stamped(Some("1")));
        assert!(!stamped(Some("")));
        assert!(!stamped(None));
    }
}
