# dim-client

The public RPC transport shared by native Dim clients; a connection negotiates v4 by default, and the ACP gateway stays on v3. It owns a protected
socket or authenticated Windows pipe bridge, JSON framing, request identities,
interleaved responses and notices, and hello/profile validation.

`ConnectionConfig` names the endpoint, Windows bridge executable, caller identity
and optional expected `DaemonProfile`. `Client::connect` opens and negotiates an
existing daemon; it never starts or replaces one. A host that starts a daemon
uses `open` and `hello` separately so only that owner's endpoint-permission
transition can be retried. `connection_config` supplies the same configuration
for a later independently owned connection. `send`, `response`, `next`, `rpc`
and `typed` preserve the RPC identities and errors. `wait_closed` consumes the
connection and waits for transport EOF and the Windows bridge's exit.

`callback::Capture` owns only bounded loopback HTTP callback I/O. Account/MCP
owners validate callback state, exchange credentials and persist account state.
The caller owns cancellation and the returned HTTP stream.

`wire` is the shared public product-event and history-page parsing boundary.
Clients consume its typed facts directly; it does not project UI state or run
Agent code.

Daemon discovery, startup, installation, home selection and command behavior
belong to the host. This crate has no App Server, CLI, TUI or OAuth-provider
implementation dependency. Protocol boundary tests exercise actual local
connections; the existing CLI flows protect extraction compatibility.
