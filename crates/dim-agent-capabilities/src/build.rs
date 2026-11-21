//! The label this build reports as its identity: the version the release flow
//! set for it, or the committed version marked `-source` and naming the commit
//! it was built from. Display only: it must never be ordered or compared as a
//! version, which is what [`VERSION`] is for.

use std::sync::OnceLock;

/// The workspace version; the release flow sets it for the builds it produces.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The marker of a build the release flow did not produce.
const SOURCE: &str = "-source";

/// The commit the final binary crate read at build time; `None` for a build
/// with no git to answer, which reports the bare `SOURCE` mark.
static COMMIT: OnceLock<Option<&'static str>> = OnceLock::new();

/// The stamp the final binary crate embedded, which names the artifact rather
/// than the source; `None` when the build left none.
static BUILD_ID: OnceLock<Option<String>> = OnceLock::new();

/// Whether the release flow produced this build, installed by the final binary
/// crate.
static RELEASE: OnceLock<bool> = OnceLock::new();

/// Hands this build the commit, artifact stamp and release mark the final
/// binary crate read at build time. The binary calls this once before it serves
/// either role, so every label in the process — the client's, the daemon's
/// reports and its MCP identity — names one build. Nothing in this crate reads
/// git or a build environment, so a new commit or release flag never recompiles
/// it. The first call wins.
pub fn install(commit: Option<&'static str>, build_id: Option<&'static str>, release: bool) {
    let _ = COMMIT.set(commit);
    let _ = BUILD_ID.set(stamp(build_id));
    let _ = RELEASE.set(release_flag(release));
}

/// The stamp this build carries. A debug build takes `DIM_TEST_BUILD_ID`
/// instead, so a flow can stand a client and a daemon of different builds
/// beside each other; an empty value there means no stamp.
fn stamp(build_id: Option<&'static str>) -> Option<String> {
    #[cfg(debug_assertions)]
    if let Some(value) = std::env::var_os("DIM_TEST_BUILD_ID") {
        return value
            .to_str()
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
    }
    build_id.map(str::to_owned)
}

/// Whether this build behaves as a released build. A debug build takes
/// `DIM_TEST_RELEASE_BUILD` instead, so a flow can exercise release-gated
/// behavior without recompiling with `DIM_RELEASE_BUILD=1`; an empty value or
/// `"0"` means a source build. A release build ignores the variable.
fn release_flag(release: bool) -> bool {
    #[cfg(debug_assertions)]
    if let Some(value) = std::env::var_os("DIM_TEST_RELEASE_BUILD") {
        return value
            .to_str()
            .is_some_and(|value| !value.is_empty() && value != "0");
    }
    release
}

/// Whether this process behaves as a released build: the compiled release mark
/// installed by the binary crate, or in debug builds the `DIM_TEST_RELEASE_BUILD`
/// override read during [`install`]. A release build ignores the override.
pub fn release() -> bool {
    RELEASE.get().copied().unwrap_or(false)
}

/// The artifact stamp this build reported, or `None` when its build left
/// none: an unstamped build names its source but not its artifact.
pub fn build_id() -> Option<&'static str> {
    BUILD_ID.get().and_then(|build_id| build_id.as_deref())
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
    label(VERSION, release(), COMMIT.get().copied().flatten())
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
}
