//! The committed `package-lock.json` files of the runtime's plugins.
//!
//! A plugin's tree is installed with `npm ci --ignore-scripts`, so the lock is
//! what ships: it carries this npm's lockfile version, its root entry repeats
//! the manifest it was resolved for as npm normalizes it, every exact pin
//! resolves from its holder to the version the lock records, and every entry
//! has an `integrity` and no install script.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const LOCKFILE_VERSION: u64 = 3;
/// The manifest members npm resolves into the lock's root entry.
const ROOT_MEMBERS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];
/// The manifest fields the lock's root entry repeats.
const ROOT_FIELDS: [&str; 2] = ["name", "version"];
/// The members whose exact pins npm resolves for an entry. A peer is left out:
/// the lock carries an optional peer only when something installed it, so an
/// unmet peer is not drift.
const RESOLVED_MEMBERS: [&str; 3] = ["dependencies", "devDependencies", "optionalDependencies"];

/// Every plugin directory that has a lock, sorted.
fn locks(root: &Path) -> Result<Vec<PathBuf>> {
    let directory = root.join("runtime/plugins");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        let lock = path.join("package-lock.json");
        if path.is_dir() && lock.is_file() {
            found.push(lock);
        }
    }
    found.sort();
    Ok(found)
}

/// The version a lock spec pins exactly, or `None` for a range npm resolves.
fn exact(spec: &str) -> Option<&str> {
    let mut parts = spec.split('.');
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(major), Some(minor), Some(patch), None)
            if digits(major) && digits(minor) && digits(patch) =>
        {
            Some(spec)
        }
        _ => None,
    }
}

/// The manifest as npm's `optionalDedupe` step leaves it: a name that
/// `optionalDependencies` also declares leaves `dependencies`, because npm
/// resolves the optional spec (`@npmcli/package-json/lib/normalize.js`;
/// `@npmcli/arborist/lib/arborist/load-virtual.js` reads a lock root the same
/// way, deleting every optional name from its `dependencies`).
fn npm_normalized(manifest: &Value) -> Value {
    let mut normalized = manifest.clone();
    let optional: BTreeSet<String> = manifest
        .get("optionalDependencies")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(dependencies) = normalized
        .get_mut("dependencies")
        .and_then(Value::as_object_mut)
    {
        dependencies.retain(|name, _| !optional.contains(name));
    }
    normalized
}

/// One member of a manifest or lock root, absent members as empty.
fn members(entry: &Value, member: &str) -> BTreeMap<String, Value> {
    entry
        .get(member)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn check_lock(lock_path: &Path, errors: &mut Vec<String>) {
    let shown = lock_path.display();
    let read = || -> std::result::Result<Value, String> {
        let bytes = std::fs::read(lock_path).map_err(|error| format!("{shown}: {error}"))?;
        serde_json::from_slice(&bytes).map_err(|error| format!("{shown}: {error}"))
    };
    let lock = match read() {
        Ok(lock) => lock,
        Err(error) => return errors.push(error),
    };
    match lock["lockfileVersion"].as_u64() {
        Some(LOCKFILE_VERSION) => {}
        Some(other) => errors.push(format!(
            "{shown}: lockfileVersion is {other}, not {LOCKFILE_VERSION}"
        )),
        None => errors.push(format!("{shown}: lockfileVersion is absent")),
    }
    let Some(entries) = lock["packages"].as_object() else {
        return errors.push(format!("{shown}: no packages member"));
    };
    for (name, entry) in entries.iter().filter(|(name, _)| !name.is_empty()) {
        if !entry["integrity"].is_string() {
            errors.push(format!("{shown}: {name} has no integrity"));
        }
        if !matches!(entry["hasInstallScript"], Value::Null | Value::Bool(false)) {
            errors.push(format!("{shown}: {name} runs an install script"));
        }
    }

    let manifest_path = lock_path.with_file_name("package.json");
    let manifest_shown = manifest_path.display();
    let manifest: Value = match std::fs::read(&manifest_path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest,
            Err(error) => return errors.push(format!("{manifest_shown}: {error}")),
        },
        Err(error) => return errors.push(format!("{manifest_shown}: {error}")),
    };
    let Some(root) = entries.get("") else {
        return errors.push(format!("{shown}: no root entry"));
    };
    for field in ROOT_FIELDS {
        if root.get(field) != manifest.get(field) {
            errors.push(format!(
                "{shown}: the root {field} is {}, the manifest's is {}",
                root.get(field).unwrap_or(&Value::Null),
                manifest.get(field).unwrap_or(&Value::Null)
            ));
        }
    }
    let normalized = npm_normalized(&manifest);
    for member in ROOT_MEMBERS {
        let (locked, declared) = (members(root, member), members(&normalized, member));
        let drifted: std::collections::BTreeSet<&String> = locked
            .keys()
            .chain(declared.keys())
            .filter(|name| locked.get(*name) != declared.get(*name))
            .collect();
        if !drifted.is_empty() {
            errors.push(format!(
                "{shown}: the root entry's {member} disagree with the manifest on {}",
                drifted
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    // An exact pin must be the version npm installs for the entry that pins it.
    for (name, entry) in entries {
        let holder = if name.is_empty() {
            "the root"
        } else {
            name.as_str()
        };
        for member in RESOLVED_MEMBERS {
            for (dependency, pin) in members(entry, member) {
                let Some(pin) = pin.as_str().and_then(exact) else {
                    continue;
                };
                let version = installed(entries, name, &dependency);
                match version {
                    Some(version) if version == pin => {}
                    Some(version) => errors.push(format!(
                        "{shown}: {holder} pins {dependency} {pin}, the lock installs {version}"
                    )),
                    None => errors.push(format!(
                        "{shown}: {holder} pins {dependency} {pin}, which is not locked"
                    )),
                }
            }
        }
    }
}

/// The version npm installs for `dependency` of the entry at `holder`: the
/// nearest `node_modules/<dependency>` from the holder's directory up to the
/// root, the walk npm makes when it resolves a dependency.
fn installed<'a>(
    entries: &'a serde_json::Map<String, Value>,
    holder: &str,
    dependency: &str,
) -> Option<&'a str> {
    let locked = |directory: &str| {
        let path = if directory.is_empty() {
            format!("node_modules/{dependency}")
        } else {
            format!("{directory}/node_modules/{dependency}")
        };
        entries
            .get(&path)
            .and_then(|entry| entry["version"].as_str())
    };
    let mut directory = holder;
    loop {
        if directory.is_empty() {
            return locked(directory);
        }
        if directory.rsplit('/').next() != Some("node_modules")
            && let Some(version) = locked(directory)
        {
            return Some(version);
        }
        directory = match directory.rfind('/') {
            Some(index) => &directory[..index],
            None => "",
        };
    }
}

pub fn check(root: &Path) -> Result<()> {
    let locks = locks(root)?;
    if locks.is_empty() {
        return Err(format!("{}: no plugin lock", root.join("runtime/plugins").display()).into());
    }
    let mut errors = Vec::new();
    for path in &locks {
        check_lock(path, &mut errors);
    }
    if !errors.is_empty() {
        return Err(errors.join("\n").into());
    }
    println!("plugin locks passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin tree whose manifest and lock agree, under a fresh directory.
    fn agreeing(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("dim-xtask-npm-{name}-{}", std::process::id()));
        let plugin = root.join("runtime/plugins/example");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("package.json"),
            r#"{"name":"example","version":"1.0.0","private":true,
                "dependencies":{"left":"1.2.3"},"devDependencies":{"test-only":"0.1.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            plugin.join("package-lock.json"),
            r#"{"name":"example","lockfileVersion":3,"packages":{
                "":{"name":"example","version":"1.0.0",
                     "dependencies":{"left":"1.2.3"},"devDependencies":{"test-only":"0.1.0"}},
                "node_modules/left":{"version":"1.2.3","integrity":"sha512-left"},
                "node_modules/test-only":{"version":"0.1.0","integrity":"sha512-test"}}}"#,
        )
        .unwrap();
        root
    }

    /// The lock file of the tree `root`, rewritten with `edit`.
    fn rewrite(root: &Path, edit: impl FnOnce(&mut Value)) {
        let path = root.join("runtime/plugins/example/package-lock.json");
        let mut lock: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut lock);
        std::fs::write(&path, serde_json::to_vec(&lock).unwrap()).unwrap();
    }

    /// A plugin tree with two copies of `b`: 2.0.0 at the top level and 1.0.0
    /// nested under `a`, which pins the nested one.
    fn nested(name: &str) -> PathBuf {
        let root = agreeing(name);
        let plugin = root.join("runtime/plugins/example");
        std::fs::write(
            plugin.join("package.json"),
            r#"{"name":"example","version":"1.0.0","private":true,
                "dependencies":{"a":"1.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            plugin.join("package-lock.json"),
            r#"{"name":"example","lockfileVersion":3,"packages":{
                "":{"name":"example","version":"1.0.0","dependencies":{"a":"1.0.0"}},
                "node_modules/a":{"version":"1.0.0","integrity":"sha512-a",
                                 "dependencies":{"b":"1.0.0"}},
                "node_modules/a/node_modules/b":{"version":"1.0.0","integrity":"sha512-b1"},
                "node_modules/b":{"version":"2.0.0","integrity":"sha512-b2"}}}"#,
        )
        .unwrap();
        root
    }

    fn refusal(root: &Path) -> String {
        let error = check(root).unwrap_err().to_string();
        std::fs::remove_dir_all(root).unwrap();
        error
    }

    /// A plugin tree whose manifest declares `manifest` and whose lock root
    /// repeats `root`, as npm writes it, with `installed` as its
    /// `node_modules/` entries.
    fn overlapping(name: &str, manifest: &str, root: &str, installed: &[(&str, &str)]) -> PathBuf {
        let tree = agreeing(name);
        std::fs::write(tree.join("runtime/plugins/example/package.json"), manifest).unwrap();
        rewrite(&tree, |lock| {
            let packages = lock["packages"].as_object_mut().unwrap();
            packages.clear();
            packages.insert(String::new(), serde_json::from_str(root).unwrap());
            for (package, version) in installed {
                packages.insert(
                    format!("node_modules/{package}"),
                    serde_json::json!({"version": version, "integrity": format!("sha512-{package}")}),
                );
            }
        });
        tree
    }

    #[test]
    fn an_agreeing_tree_passes() {
        let root = agreeing("clean");
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_lockfile_version_other_than_three_is_named() {
        let root = agreeing("lockfile-version");
        rewrite(&root, |lock| lock["lockfileVersion"] = Value::from(2));
        let error = refusal(&root);
        assert!(error.contains("lockfileVersion is 2, not 3"), "{error}");
    }

    #[test]
    fn a_root_dependency_the_manifest_changed_is_named() {
        let root = agreeing("root-dependency");
        rewrite(&root, |lock| {
            lock["packages"][""]["dependencies"]["left"] = Value::from("2.0.0")
        });
        let error = refusal(&root);
        assert!(
            error.contains("the root entry's dependencies disagree with the manifest on left"),
            "{error}"
        );
        assert!(
            error.contains("pins left 2.0.0, the lock installs 1.2.3"),
            "{error}"
        );
    }

    #[test]
    fn a_dependency_npm_takes_out_of_an_optional_overlap_passes() {
        let root = overlapping(
            "optional-overlap",
            r#"{"name":"example","version":"1.0.0","private":true,
                "dependencies":{"left":"1.2.3"},"optionalDependencies":{"left":"1.2.3"}}"#,
            r#"{"name":"example","version":"1.0.0","optionalDependencies":{"left":"1.2.3"}}"#,
            &[("left", "1.2.3")],
        );
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_optional_overlap_does_not_hide_a_dependency_the_lock_root_lacks() {
        let root = overlapping(
            "optional-overlap-drift",
            r#"{"name":"example","version":"1.0.0","private":true,
                "dependencies":{"left":"1.2.3","right":"2.0.0"},
                "optionalDependencies":{"left":"1.2.3"}}"#,
            r#"{"name":"example","version":"1.0.0","optionalDependencies":{"left":"1.2.3"}}"#,
            &[("left", "1.2.3")],
        );
        let error = refusal(&root);
        assert!(
            error.contains("the root entry's dependencies disagree with the manifest on right"),
            "{error}"
        );
    }

    #[test]
    fn a_dev_overlap_npm_keeps_is_named_when_the_lock_root_drops_it() {
        let root = overlapping(
            "dev-overlap",
            r#"{"name":"example","version":"1.0.0","private":true,
                "dependencies":{"left":"1.2.3"},"devDependencies":{"left":"1.2.3"}}"#,
            r#"{"name":"example","version":"1.0.0","devDependencies":{"left":"1.2.3"}}"#,
            &[("left", "1.2.3")],
        );
        let error = refusal(&root);
        assert!(
            error.contains("the root entry's dependencies disagree with the manifest on left"),
            "{error}"
        );
    }

    #[test]
    fn a_peer_overlap_npm_keeps_is_named_when_the_lock_root_drops_it() {
        let root = overlapping(
            "peer-overlap",
            r#"{"name":"example","version":"1.0.0","private":true,
                "dependencies":{"left":"1.2.3"},"peerDependencies":{"left":"1.2.3"}}"#,
            r#"{"name":"example","version":"1.0.0","peerDependencies":{"left":"1.2.3"}}"#,
            &[("left", "1.2.3")],
        );
        let error = refusal(&root);
        assert!(
            error.contains("the root entry's dependencies disagree with the manifest on left"),
            "{error}"
        );
    }

    #[test]
    fn a_root_version_the_lock_does_not_install_is_named() {
        let root = agreeing("root-version");
        rewrite(&root, |lock| {
            lock["packages"][""]["devDependencies"]["test-only"] = Value::from("0.2.0")
        });
        let error = refusal(&root);
        assert!(
            error.contains("pins test-only 0.2.0, the lock installs 0.1.0"),
            "{error}"
        );
    }

    #[test]
    fn a_root_dependency_with_no_entry_is_named() {
        let root = agreeing("missing-entry");
        rewrite(&root, |lock| {
            lock["packages"]
                .as_object_mut()
                .unwrap()
                .remove("node_modules/test-only");
        });
        let error = refusal(&root);
        assert!(
            error.contains("the root pins test-only 0.1.0, which is not locked"),
            "{error}"
        );
    }

    #[test]
    fn an_entry_without_integrity_is_named() {
        let root = agreeing("integrity");
        rewrite(&root, |lock| {
            lock["packages"]["node_modules/left"]
                .as_object_mut()
                .unwrap()
                .remove("integrity");
        });
        let error = refusal(&root);
        assert!(
            error.contains("node_modules/left has no integrity"),
            "{error}"
        );
    }

    #[test]
    fn an_install_script_is_named_and_its_absence_is_not() {
        let root = agreeing("install-script");
        rewrite(&root, |lock| {
            lock["packages"]["node_modules/test-only"]["hasInstallScript"] = Value::Bool(false)
        });
        check(&root).unwrap();
        rewrite(&root, |lock| {
            lock["packages"]["node_modules/test-only"]["hasInstallScript"] = Value::Bool(true)
        });
        let error = refusal(&root);
        assert!(
            error.contains("node_modules/test-only runs an install script"),
            "{error}"
        );
    }

    #[test]
    fn a_nested_pin_the_lock_does_not_install_is_named() {
        let root = agreeing("nested-pin");
        rewrite(&root, |lock| {
            lock["packages"]["node_modules/left"]["dependencies"] =
                serde_json::json!({"test-only": "0.9.0"})
        });
        let error = refusal(&root);
        assert!(
            error.contains("node_modules/left pins test-only 0.9.0, the lock installs 0.1.0"),
            "{error}"
        );
    }

    #[test]
    fn a_pin_resolves_to_the_copy_nested_under_its_holder() {
        let root = nested("nested-ok");
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_pin_the_copy_under_its_holder_contradicts_is_named() {
        let root = nested("nested-drift");
        rewrite(&root, |lock| {
            lock["packages"]["node_modules/a/node_modules/b"]["version"] = Value::from("1.0.1")
        });
        let error = refusal(&root);
        assert!(
            error.contains("node_modules/a pins b 1.0.0, the lock installs 1.0.1"),
            "{error}"
        );
        assert!(!error.contains("2.0.0"), "{error}");
    }

    #[test]
    fn a_range_is_not_an_exact_pin() {
        let root = agreeing("range");
        rewrite(&root, |lock| {
            lock["packages"]["node_modules/left"]["dependencies"] =
                serde_json::json!({"test-only": "~0.1.0"})
        });
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
