//! Exports existing Session and branch services to TypeScript parts by name
//! (branch-typescript.md), generating their dispatch and declarations.
fn main() {
    rutis_bridge::build::Exports::new()
        .service("sessionPermissions", "crate::agents::SessionPermissions")
        .service("workflowFiles", "crate::settings::WorkflowFiles")
        .service_in(
            "treeCommands",
            "dim_agent_capabilities::timeline_tree::TreeCommands",
            "session",
        )
        .service_in("dispatcher", "dim_agent_runtime::Dispatcher", "branch")
        .service_in(
            "branchLifecycle",
            "dim_agent_runtime::BranchLifecycle",
            "branch",
        )
        .service_in("timeline", "dim_agent_runtime::Timeline", "branch")
        .service_in(
            "timelineIdentity",
            "dim_agent_runtime::TimelineIdentity",
            "branch",
        )
        // Custom serde: reuse the SDK envelope with native bridge sequence values.
        .declared(
            "dim_agent_types::AgentEvent",
            "import('./plugin.js').BranchEvent",
        )
        // The same envelope before the Timeline assigns identity and order.
        .declared(
            "dim_agent_types::EventDraft",
            "{ type: string; name: string; data: unknown; tags?: string[] }",
        )
        // Reuse the identity envelope; the service keeps its own TypeScript name.
        .declared(
            "dim_agent_runtime::TimelineIdentity",
            "import('./plugin.js').SessionRef",
        )
        .typescript("../../npm/branch-services.d.ts")
        .generate()
        .expect("generate the exported services");
}
