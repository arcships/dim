# Timeline SQLite acceptance

This opt-in program uses public TimelineStore/Agent/Session interfaces and a test SQLite VFS. Each run creates an independent database. Set `TIMELINE_ACCEPTANCE_ROOT` to a dedicated test directory; the default is `/mnt/data/timeline-acceptance`.

Build from the repository root, then run one workload at a time:

```sh
cargo build --locked --release -p dim-app-server --example timeline_acceptance
TIMELINE_ACCEPTANCE_ROOT=/absolute/test-directory \
  ./target/release/examples/timeline_acceptance rounds 11
```

| Command | Workload |
| --- | --- |
| `memory 2000` | Long Agent history with context preparation/compaction and allocation measurement |
| `reads` | Cold history/context reads and interrupt delivery during reads |
| `paged-reads` | Paged recovery while appending events |
| `rounds [BRANCHES [PER_SECOND [CHAINS]]]` | BRANCHES streaming Sessions (default 64) writing PER_SECOND deltas each (a positive rate, default 200) and CHAINS Sessions (default 0) whose writes each wait for the previous one's publication: write rounds, WAL bytes/syncs, delta publication latency, and each chain step's latency and throughput. Arguments are positional, so CHAINS needs the two before it: `rounds 11 30`, `rounds 64 200 1`, `rounds 0 200 64` |
| `crashes` | Process termination at transaction/publication barriers and continuity after recovery |
| `powerloss` | Restore only successfully synced main/WAL images and verify recovery continuity |

`rounds` reports `walBytesPerEvent` over all output deltas and chain writes, and
`checkpointSyncs` counts successful main-database syncs during the measured WAL
traffic. On Unix, `writerCpuMs` is the syncing writer thread’s CPU time between
its first and last WAL sync; it excludes setup and work outside that span. It is
`null` on unsupported platforms or if multiple threads sync the WAL, rather than
reporting process-wide CPU as writer CPU.

In `crashes` and `powerloss` the interrupted batch is an input and the request prepared from it. Recovery validates the Session, including every request list's nodes, reads the recovered events back, and writes a request extending it; the Session's rows must then equal those of the same events written without interruption.

On macOS, `TIMELINE_ACCEPTANCE_PLAIN_FSYNC=1` turns SQLite's F_FULLFSYNC into a plain fsync, a model of a disk whose sync is fast; elsewhere SQLite does not use F_FULLFSYNC and the variable changes nothing.

The power-loss model assumes fsync reports durable writes honestly; it does not model torn sectors or macOS fullfsync. Rust allocation measurements exclude SQLite allocations and are not RSS. Routine storage behavior remains protected by the normal flow tests; these workloads are optional resource measurements.
