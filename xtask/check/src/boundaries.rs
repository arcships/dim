//! Crate dependency boundaries from `cargo metadata`, live syn-AST module
//! boundaries, committed `crates.*` freshness, and plugin-document checks.

use crate::{modules, plugin_docs};
use serde_json::{Value, json};
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Verifies crate/module boundaries, `crates.*` freshness and plugin documents.
pub fn check(root: &Path) -> Result<()> {
    run(root, false)
}

/// Verifies crate/module boundaries, writes `crates.*`, and checks plugin documents.
pub fn write(root: &Path) -> Result<()> {
    run(root, true)
}

fn check_dependency(name: &str, dep: &str) -> Result<()> {
    if name == "dim-agent-types" && !modules::TYPES_VALUE_DEPENDENCIES.contains(&dep) {
        return Err("types must not depend on runtime/capabilities".into());
    }
    if name == "dim-agent-runtime" && (dep == "dim-agent-capabilities" || dep == "dim-app-server") {
        return Err("runtime must not depend on capabilities/server".into());
    }
    if name == "dim-agent-capabilities" && dep == "dim-app-server" {
        return Err("capabilities must not depend on server".into());
    }
    if name == "xtask-check" && (dep.starts_with("dim-") || dep == "rutis") {
        return Err(format!("xtask-check must stay free of business/runtime crate {dep}").into());
    }
    Ok(())
}

fn run(root: &Path, write: bool) -> Result<()> {
    let output = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy_owned(output.stderr).into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let mut edges = vec![];
    let mut graph = String::from("%% Generated from cargo metadata; do not edit.\nflowchart LR\n");
    for p in metadata["packages"].as_array().unwrap() {
        let name = p["name"].as_str().unwrap();
        for d in p["dependencies"].as_array().unwrap() {
            let dep = d["name"].as_str().unwrap();
            check_dependency(name, dep)?;
            if dep.starts_with("dim-") || dep == "rutis" {
                edges.push(json!({"from":name,"to":dep}));
                graph += &format!(
                    "  {}[\"{name}\"] --> {}[\"{dep}\"]\n",
                    name.replace('-', "_"),
                    dep.replace('-', "_")
                );
            }
        }
    }
    let out = root.join("docs/architecture/rust-agent/generated");
    let files = [
        ("crates.json", serde_json::to_string_pretty(&edges)? + "\n"),
        ("crates.mmd", graph),
    ];
    modules::check(root)?;
    if write {
        std::fs::create_dir_all(&out)?;
        for (name, text) in &files {
            std::fs::write(out.join(name), text)?;
        }
    } else {
        let stale: Vec<_> = files
            .iter()
            .filter(|(name, text)| {
                std::fs::read_to_string(out.join(name)).ok().as_ref() != Some(text)
            })
            .map(|(name, _)| out.join(name).display().to_string())
            .collect();
        if !stale.is_empty() {
            return Err(format!(
                "generated architecture files are stale: {}; run `cargo run -p xtask -- graph`",
                stale.join(", ")
            )
            .into());
        }
    }
    println!("crate and module boundaries passed");
    plugin_docs::check(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_dependency;

    #[test]
    fn xtask_check_rejects_business_and_runtime_crates() {
        assert!(check_dependency("xtask-check", "dim-agent-types").is_err());
        assert!(check_dependency("xtask-check", "dim-app-server").is_err());
        assert!(check_dependency("xtask-check", "rutis").is_err());
        assert!(check_dependency("xtask-check", "rutis-types").is_ok());
        assert!(check_dependency("xtask-check", "syn").is_ok());
    }
}
