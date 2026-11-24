#![cfg(unix)]

#[path = "support/cli_harness.rs"]
mod harness;
#[allow(
    unused_imports,
    reason = "submodules re-import shared harness items via super::*"
)]
use harness::*;

#[path = "cli_flow/goal.rs"]
mod goal;
#[path = "cli_flow/scheduled.rs"]
mod scheduled;
#[path = "cli_flow/session_management.rs"]
mod session_management;
#[path = "cli_flow/tasks.rs"]
mod tasks;
