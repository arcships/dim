//! Names the commit a source build was made from, so two checkouts of the same
//! committed version are distinguishable, and stamps the artifact a build
//! produces, so two builds of the same version are distinguishable too. This is
//! the final binary crate on purpose: a new commit or stamp changes this
//! environment only, so it recompiles `dimcode` alone and never the crates that
//! print the label.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The commit a caller already resolved, for a build cache that would
/// otherwise replay another checkout's answer.
const FROM_CALLER: &str = "DIM_BUILD_COMMIT";

/// The artifact stamp: a packaging caller resolves it, and it reaches the crate
/// as a compile-time value of the same name, so a build cache that replays
/// another build's result is told to re-run this script when it changes.
const BUILD_ID: &str = "DIM_BUILD_ID";

/// The commit `dimcode` reads as a compile-time value; absent when nothing
/// named a commit or when this is a release build, whose output must not change.
const COMMIT: &str = "DIM_GIT_COMMIT";

/// Whether the release flow produced this build, forwarded into the final
/// binary crate so a build cache replays it alongside the stamp.
const RELEASE: &str = "DIM_RELEASE_BUILD";

fn main() {
    println!("cargo:rerun-if-env-changed={RELEASE}");
    println!("cargo:rerun-if-env-changed={FROM_CALLER}");
    println!("cargo:rerun-if-env-changed={BUILD_ID}");
    let stamp = std::env::var(BUILD_ID)
        .ok()
        .map(|stamp| stamp.trim().to_owned())
        .filter(|stamp| !stamp.is_empty());
    if std::env::var_os(RELEASE).is_some_and(|value| !value.is_empty()) {
        // A released build carries no source identity, but the artifact stamp a
        // packaging caller resolved still distinguishes two builds of one
        // version.
        println!("cargo:rustc-env={RELEASE}=1");
        if let Some(stamp) = stamp {
            println!("cargo:rustc-env={BUILD_ID}={stamp}");
        }
        return;
    }
    watch_refs();
    // A build cache that shares recorded build-script results between
    // checkouts replays whichever commit the first build reported, because a
    // script's declared inputs are package-local and HEAD's own files sit
    // outside the package. So a caller that already resolved the commit hands
    // it over, and the declaration above re-runs this script when it changes.
    let commit = std::env::var(FROM_CALLER)
        .ok()
        .map(|commit| commit.trim().to_owned())
        .filter(|commit| !commit.is_empty())
        .or_else(|| git(&["rev-parse", "--short=8", "HEAD"]));
    match &commit {
        Some(commit) => println!("cargo:rustc-env={COMMIT}={commit}"),
        None => println!(
            "cargo:warning=git could not resolve HEAD, so this build reports the bare version"
        ),
    }
    // Only a packaging caller stamps the artifact it produces. A source build
    // stays unstamped on purpose: it is not a distributable artifact, and a
    // client that cannot name its own build has nothing to compare.
    if let Some(stamp) = stamp {
        println!("cargo:rustc-env={BUILD_ID}={stamp}");
    }
}

/// What `git` prints for `args`, without the surrounding whitespace; `None`
/// when git is unavailable or the command fails, which is not a build error.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    output.status.success().then(|| text.trim().to_owned())
}

/// The absolute path `git rev-parse --git-path` reports for `path`.
fn resolve(path: &str) -> Option<PathBuf> {
    let path = git(&["rev-parse", "--git-path", path])?;
    let path = Path::new(&path);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
    })
}

/// Declares the files that move when HEAD does. In a linked worktree `.git` is
/// a file and HEAD lives under the worktree's own git directory, so a
/// `.git/HEAD` hint never fires; what `git rev-parse --git-path` reports,
/// resolved against the package directory, is the path that changes.
fn watch_refs() {
    let Some(head) = resolve("HEAD") else {
        return;
    };
    println!("cargo:rerun-if-changed={}", head.display());
    // A reftable repository keeps every ref in its `reftable` directory and
    // leaves HEAD and `refs/heads` as stubs that never change, so that
    // directory is what moves. Watching it also covers a new commit, which
    // rewrites a table and `tables.list` there.
    if let Some(reftable) = resolve("reftable").filter(|path| path.is_dir()) {
        println!("cargo:rerun-if-changed={}", reftable.display());
        return;
    }
    // Otherwise the refs are files: `packed-refs`, the branch's loose ref, or
    // both. A ref that becomes packed always loses its loose file, which is
    // watched, so watching both sees either transition. A watched path that
    // does not exist makes Cargo re-run this script on every build, so a
    // missing one is replaced by its nearest existing directory inside
    // `refs/`, never by the git directory itself, whose `objects/` no commit
    // of this branch needs to rescan.
    if let Some(packed) = resolve("packed-refs").filter(|path| path.is_file()) {
        println!("cargo:rerun-if-changed={}", packed.display());
    }
    let some_ref = git(&["symbolic-ref", "-q", "HEAD"]).and_then(|reference| resolve(&reference));
    let Some(reference) = some_ref else {
        return;
    };
    if reference.exists() {
        println!("cargo:rerun-if-changed={}", reference.display());
        return;
    }
    let Some(refs) = resolve("refs") else {
        return;
    };
    if let Some(inside) = reference
        .ancestors()
        .find(|parent| parent.is_dir() && parent.starts_with(&refs))
    {
        println!("cargo:rerun-if-changed={}", inside.display());
    }
}
