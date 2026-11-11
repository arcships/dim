//! `types` writes the TypeScript declarations of the services marked
//! `#[rutis::service]` that `dim-app-server` serves (rutis usage §3.4) to
//! `npm/branch-services.d.ts`; `check` fails when that file is stale.
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn file(root: &Path) -> PathBuf {
    root.join("npm/branch-services.d.ts")
}

fn declarations(root: &Path) -> Result<String> {
    rutis_types::Exports::new()
        .declared(
            "dim_agent_types::PermissionPolicy",
            "import('./plugin.js').PermissionPolicy",
        )
        .declared(
            "dim_agent_types::AgentEvent",
            "import('./plugin.js').BranchEvent",
        )
        .declared(
            "dim_agent_types::EventDraft",
            "{ type: string; name: string; data: unknown; tags?: string[]; context_render?: boolean; context_state?: boolean }",
        )
        .declared(
            "dim_agent_runtime::TimelineIdentity",
            "import('./plugin.js').SessionRef",
        )
        .declared(
            "dim_agent_runtime::SessionIdentity",
            "{ sessionId: string; restored: boolean }",
        )
        .declared(
            "dim_agent_runtime::SessionConfig",
            "import('./plugin.js').SessionConfig",
        )
        .declared("std::num::NonZeroU64", "number | bigint")
        .declared(
            "dim_agent_types::TimelinePosition",
            "{ branch_id: string; sequence: number | bigint }",
        )
        .declarations(root.join("crates/dim-app-server"))
}

pub fn write(root: &Path) -> Result<()> {
    std::fs::write(file(root), declarations(root)?)?;
    Ok(())
}

pub fn check(root: &Path) -> Result<()> {
    let path = file(root);
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    if written != declarations(root)? {
        return Err(format!(
            "{}: service declarations are stale; run `cargo run -p xtask -- types`",
            path.display()
        )
        .into());
    }
    println!("service declarations passed");
    Ok(())
}
