# ShellSession

Owner: [shell_session.rs](../../../../crates/dim-agent-capabilities/src/shell_session.rs).
The branch's persistent shell: one PTY per branch, started on its first
command, with cwd, environment, shell variables and jobs kept across calls.
It provides the branch's `ShellSession`, so the Session's `execute` tool reaches
it per branch; `session.exec.stop` reaches the branch's `BackgroundExec`.

## ShellSession

The branch part is mounted by the profile's ShellSession feature. It owns the
PTY and the command queue and hands its background tasks to `BackgroundExec`;
`execute` resolves the calling branch's service, and `session.exec.stop` goes to
the same branch's `BackgroundExec`.

### Dependencies

| Service | Purpose |
| --- | --- |
| `B:BackgroundExec` | Hand off background tasks and recover a shell task whose facts were committed but whose owner is gone. |
| `B:BranchLifecycle` | This branch\'s phase, and its `session_id`/`branch_id` for Rust-only reads.
| `P:HostPlatform` | The shell executable and platform dialect. |
| `S:SessionConfig` | The Session's cwd and execution preferences. |
| `B:BranchLifecycle` | This branch\'s phase, and its `session_id`/`branch_id` for Rust-only reads.

### Tests

`tests/integration/tests/minimal_execution.rs` covers the shell's persistence,
occupancy, background jobs, output bounds, stop and close. The App Server's
`transport_flow::shell_input` and `transport_flow::exec_shell_settings` cover
the product paths.
