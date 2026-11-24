# UiExtensions and UiResources

The [client UI design](../design/client-ui-extensions.md) owns the public
contract. These are App Server presentation/resource owners, not Capabilities.
They emit no Timeline events and do not alter agent execution. The generated
[App Server assembly](rust-agent/generated/app-server/services.md) records their
current bindings.

UiExtensions is an independently enabled process row that registers its Session
part. It lives in the App Server crate because its owners serve every connected
client; it does not depend on the CLI or a renderer. WebEntry adapts authenticated
HTTP to these owners, while the Web client owns mounting and selection.

| Part | Scope | Dependencies | Provides |
| --- | --- | --- | --- |
| UiExtensions | process | managementMethods, partRegistry, fsHost | uiExtensions |
| UiResources | Session | sessionIdentity, sessionLifecycle, fsHost | uiResources |

PluginManager registers UI content independently of code rows. UiExtensions
copies regular files through FileStorage into a private generation directory,
then atomically publishes its directory entry and revision. Its process-row
configuration retains source declarations and the revision counter across row
reloads. Snapshot bytes and file handles belong to each generation. Failed
preparation withdraws the old entry and exposes an unavailable declaration.
Withdrawal cancels admitted reads, waits for their producers, then removes the
snapshot. The registration sources contain no runtime handles.

FileStorage registers the stable `ui-cache` root once, reusing its root handle
across UiExtensions generations. Each generation owns a child directory and
removes it on close. Before activation, UiExtensions removes abandoned UUID
directories left by a crash. Source roots are deduplicated by FileStorage.
Registrations without UI content only withdraw an existing entry; they do not
change the revision when the public directory is unchanged. Source maps are
excluded from snapshots.

UiExtensions registers the UiResources Session part. `publishFile` uses the
calling rutis context as the publication owner, registers cleanup there, and
returns an opaque reference. It opens only finalized regular files within an
already authorized FileStorage root. Calls and reads end on Session closing,
without waiting for the part's unload; withdrawing another caller's reference
fails. The generated TypeScript binding exports this service by its Session name.

WebEntry authenticates resource requests and holds the Session lease during a
media read. A bounded channel carries 64 KiB chunks; dropping the HTTP body or
withdrawing the owner cancels its producer. This transport carries bytes directly
and does not route file contents through the TypeScript bridge.
