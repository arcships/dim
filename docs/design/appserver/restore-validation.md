# Native Session restore read boundary

Scope: native storage directory reads and errors during Session restoration. Related: [#1320](https://github.com/arcships/dim/issues/1320), [storage format](native-storage-format.md), [Timeline export](timeline-export.md).

## 1. Restore reads the directory

`TimelineStore.branches` reads and parses the Session configuration and its branch directory. It checks directory identities, the main root, parent existence, creation-position bounds and acyclic ancestry, then returns `StoredBranch` values.

Neither cold nor warm restoration scans `timeline_events` or `timeline_content` to admit a Session. There is no whole-Session validation pass, validation-result cache, cache refresh at release, or persistent validation marker. Restoring does not repair history. It does not add tail-event or creation-anchor preflight reads.

Normal producers commit event rows, content, branch heads and derived indexes in the same storage transaction. A process crash does not require an application pass to replay or verify every previously committed batch.

## 2. Read the objects that are needed

An owner requests its actual state and unfinished work through the existing Timeline queries. Selected events and referenced content are decoded by the existing storage reader; reads that use request bodies rebuild and hash-check those bodies. Reads that do not use request bodies do not load them. Failed required reads report an error and preserve the stored bytes.

The restore contract does not promise to discover an unread historical gap, duplicate identity, missing historical reference, damaged trace or damaged derived index before publishing the Session. Directory parsing still fails when its own data cannot describe a usable branch tree.

No background audit, sampled audit, alternate validation service or replacement cache is introduced.

## 3. Export

Export traverses the requested Session once, reading and decoding the events and required request bodies it actually exports. It performs no separate preflight over every event or derived index. An unread damaged derived index does not prevent exporting readable source facts.

A read failure returns an error and omits the terminal `end` record. The existing CLI removes an output file it created when export fails; stdout may contain an incomplete stream. Successful completion is established by the terminal record and successful process exit, not by the arrival of the header. Database format, path and Session existence errors are still reported before event output.

## 4. Verification and scope

Reuse storage, property-query, request-content and export flows. Prove that a directory can load without decoding unrelated malformed history, that selecting required damaged content fails, that a failed export lacks `end`, and that failed output files are removed. Keep directory-cycle and unsupported-format failures, atomic transaction rollback, fork and crash recovery tests.

There is no change to public Timeline APIs, native format v41, event encodings or assembly. Remove tests whose only requirement was a whole-history startup audit or validation-cache invalidation. Resource acceptance compares cold-process restore, owner reads and first usage separately on the same disposable long-session fixture.
