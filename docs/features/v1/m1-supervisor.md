# M1 durable fake supervisor contract

The service owns a user-private Unix socket and a lifetime advisory flock.
Stale sockets are replaced only after obtaining the lock. The default data
directory is ~/.agentpm; --data-dir supports isolated installations/tests.
The socket protocol is newline-delimited JSON with version 1, an ASCII request
id, and a tagged command. Unknown fields/versions fail before command effects.
One bounded request/response uses each connection, with read/write deadlines.
Start, show, events and status are shared by the CLI and public client seam.

SQLite schema version 2 adds narrowly scoped fake_runs, supervisor_requests,
and supervisor_events tables without changing the M0 domain tables. These are
fake execution records, not assertions of reviewed product delivery. Fake
success is recorded as finished with an explicit succeeded result; it never
sets the domain run's completed flag. Launch intent, operation/session token,
script payload, executable, cwd and timestamp are durable before spawning.
PID (also process group leader) is recorded in the launch receipt. Tokens are
logical execution identities (also passed as AGENTPM_EXECUTION_ID), not proof that a stored PID remains owned.
Accepted request response and initial event are committed together. Exact
retries return the original response; ID reuse with different payload fails.
Final result and event also commit together. No process launch is atomically
coupled to SQLite: crashes between intent, launch, receipt and completion leave
uncertain records. M1 never replays those operations or kills stored PIDs.

Clients may disconnect at any time after a start is accepted. The child and
its readers belong to the service. Real fake-agent delay actions sleep, unlike
the M0 virtual-clock demo. Three active fake runs bound process/reader count;
uncertain queued/running records continue to occupy capacity after restart.
There is no fair scheduler, budget accounting, takeover, session adoption,
automatic restart recovery or graceful supervisor shutdown in this milestone.
M4 owns reconciliation and recovery. Supervisor termination can orphan children;
restart retrieves durable results without inferring completion of uncertain work.

Stdout/stderr are drained concurrently, retaining at most 64 KiB each. Excess
stdout invalidates the result; stderr truncation is explicitly marked. Protocol
validation checks terminal records and exact exit agreement, preserving raw
bounded output and diagnostics in the result. A fixed 30-second fake-process
safety deadline kills only the newly spawned owned group. Remaining descendants
in that group are terminated after child exit to close inherited pipes. This
is a fake boundary, not an adapter or a security sandbox against hostile
same-user children escaping their group. IPC frames are capped at 1 MiB and
events are paged one at a time; durable history is retained until explicit
future pruning. This is bounded retained streaming, not live log subscription.

Tests use real service/CLI/fake child processes, temporary private directories,
SQLite persistence, duplicate IDs, changed-payload rejection, cursor replay,
service restart after completion, protocol mismatch before effects, competing
service locking, missing/invalid scripts, excessive stdout and stderr, and
background CLI startup. Local results establish macOS behavior only; Linux CI
and independent review remain integration gates.
