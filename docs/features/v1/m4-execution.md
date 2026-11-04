# M4 execution controls and usage evidence

The real `supervisor::Command` service and `agentpm run` CLI enforce durable
execution limits. Schema 5 migrates earlier records without inventing scripts,
process identities or completion evidence. Repairs retain the run, immutable
input, consumed time and three repair credits. Three run replan credits also
survive restart. Exact mutation retries never launch or spend twice.

Runs written by the earlier schema have no controls. Migration gives each
completed (non-queued, non-running) run a row with project `default`, the
two-hour default allowance counted as fully consumed, dispatch stopped and
ownership known: usage is unknown, never zero, so `run extend` is required
before repair or resume. Queued or running old rows are classified as unknown
identities by recovery. Naming a run that does not exist returns
`unknown run '<id>'`. A detached or unresolvable worktree HEAD (branch deleted
underneath it) is reported as a diagnosis; the work stays preserved.
Downgrading to a build that predates schema 5 is unsupported.

## Dispatch, reservations and revisions

`run start SCRIPT --command-id RUN` retains the two-hour default.
`run start-controlled SCRIPT --command-id RUN --project P --allowance-ms 1000`
uses the same path with an explicit allowance. `run controls RUN` shows elapsed
time, allowance, ownership/uncertainty and counters; usage remains explicitly
unknown when the backend does not report it.

`run policy SLOTS PROJECT_CAP --command-id ID` sets global/default worker caps
(1–3); `run project-cap PROJECT CAP --command-id ID` sets an individual cap.
Worker identities reserve slots before launch. Coordinator identities have a
separate per-run limit of one; they do not consume worker slots. Waiting
identities consume neither active time nor a slot.

For owned worktrees, allocate through M3 using the same RUN, then pass
`--repository REPO --milestone M` to start-controlled. Before start, repair or
resume, ownership, registered checkout, symbolic branch, HEAD and branch tip
must match the recorded expected revision. Unexpected commits require explicit
revision reconciliation, never automatic adoption.

A canonical repository reservation and supervisor launch intent commit in one
SQLite attached-database immediate transaction. Both stores require DELETE
journals and FULL synchronization for atomic multi-file rollback. Allocation
retries and independent cooperating cleanup commands check this reservation
under the same canonical claim lock. It remains through queued, running,
shutdown and uncertain states, until authoritative completion commits alongside
its release. A failed launch-intent transaction rolls back both stores. Old
bound uncertain executions receive a conservative reservation during recovery.
Worktrees and these locks are cooperative controls, not an OS sandbox.

## Shared active union

`execution::{reserve_at, transition_at, tick_at, recover_clock}` is the shared
persistent execution-identity/clock primitive for supervisor dispatch and M8.
The caller holds the supervisor transaction. Each identity records its run,
project, role, state and unique token. Live stamps use a process-wide monotonic
clock and epoch; deterministic tests supply exact stamps through the same API.
Every set transition checkpoints the run before changing membership. A run is
charged once while any identity is active, regardless of worker/coordinator
overlap; all-waiting intervals are excluded. Waiting activation rechecks slots,
coordinator ownership and the shared remaining budget. Finished identities
cannot be reused. Task replacement must retain the run and its allowance.

The existing fake dispatch registers, activates, checkpoints and finishes through
this primitive. M8 supplies the scheduling graph and consumes the same ledger;
it does not introduce another clock or reset counters. Restart preserves the
maximum recorded time, changes clock epoch explicitly, and conservatively
charges unknown active intervals. Known waiting intervals stay excluded.

Each identity's activation is stamped on that clock. When a guardian receipt
proves how long the execution ran, `execution::finish_known` charges the union
only up to that known end (start plus receipt elapsed time), so a stopped or
delayed supervisor does not bill post-completion idle time; overlapping
identities are still counted once. The union then advances to now. An end that
cannot be placed on the current clock (restart, pre-upgrade row) is charged
conservatively and never lowers recorded time.

## Per-run reconciliation isolation

Receipt, reservation and handback recovery run per run. An unavailable
repository (renamed, removed, permission denied) skips only that run: its slot,
canonical reservation and pending handback persist, a deduplicated
`reconciliation_attention` event records the cause, and the work retries on every
later command and startup. Status, show, events, unrelated starts and supervisor
restart are unaffected. A run with a complete receipt awaiting its repository is
not classified as unknown at startup, so it is not charged its whole allowance.

## Process ownership and recovery

`run cancel RUN --command-id ID` requests shutdown. Nonblocking stdin/stdout/
stderr progress under bounded work per tick and deadlines; each retained output
stream is limited to 64 KiB. Escaped pipe holders cannot hold completion open.
Only live owners signal reserved, unreaped identities. Setup and wait errors
retain the Child until safe cleanup/reap or return explicit ownership uncertainty.
Stop/continue notifications are not mistaken for process exit.

The `execution-guard` sibling owns the actual worker. It verifies snapshot
SHA-256 before launch and retains the worker deadline across service death. It
installs cancellation handling before advertising readiness, gives the worker
50 ms grace, then kills its proven group and reaps before publishing completion.
The service waits up to 200 ms for guardian shutdown; if unconfirmed it retains
uncertainty rather than killing the guardian holding a separate worker group.
Install agentpm, fake-agent and execution-guard together.

Completion receipts are private, bounded, nonce/hash-bound records published
with file and directory synchronization after cleanup. Their recorded identity
and snapshot must match the durable attempt. Reconciliation never infers
ownership or completion from a reusable PID or an elapsed timer. It consumes
confirmed receipts, records the outcome, preserves maximum elapsed time, clears
uncertainty and atomically releases the worker slot/repository reservation.
Reconciliation runs before commands/admission and at startup; it is also exposed
as `run reconcile RUN --command-id ID`. If a guardian is still finishing, a later
reconcile consumes its receipt. Unknowable historical executions stay escalated
and reserved. Missing evidence is never fabricated or treated as completion.

`run repair RUN --command-id ID` dispatches again within remaining credits/time.
`run extend RUN MILLISECONDS --command-id ID` records an explicit human allowance
extension for a stopped/terminal run. `run resume RUN --command-id ID` explicitly
replays a known terminal attempt; it does not reset consumed time or autonomous
counters. Agent/MCP interfaces must not grant these human-only authorities.

## Takeover and handback

`run takeover RUN --command-id ID` freezes the repository claim, requests worker
shutdown and confirms human ownership only after executions end. Unconfirmed
shutdown keeps the freeze and returns a pending diagnostic. Unknown identities
cannot grant exclusive ownership.

`run handback RUN --command-id ID` persists an operation and its first inspection
of HEAD, symbolic branch and tracked/untracked/ignored changes while keeping the
freeze. The operation is namespaced by supervisor database and request identity.
The supervisor then commits human ownership transfer, evidence invalidation and
the idempotent request receipt. Only that committed receipt permits repository
unfreeze. Abort/retry/crash paths preserve the first inspection; post-commit
unfreeze failures are reconciled from the durable receipt on retry or restart.
Human commits are not adopted as expected revisions, and dirty work is preserved.

## Verification

All 39 pre-correction tests remain. Eleven correction regressions add real
supervisor/Git/process fault coverage and deterministic durable-clock coverage:
unexpected start/repair/resume revisions; independent maintenance during queued
or uncertain shutdown; launch cross-store rollback; injected human=0 handback
abort and post-commit unfreeze failure with restart; guardian-backed slot release
while the stored PID names an unrelated process; unknown historical preservation;
setup/missing-pipe/wait failures; overlap, coordinator slots, waits, exhaustion
and restart; and the recorded 750 ms / 500 ms allowance recovery probe.

Required local gates are fmt, locked clippy with warnings denied, all locked
workspace tests (50 total), and locked release build. Local results establish
macOS behavior. Linux CI and fresh independent review remain primary integration
gates. No paid backend, product merge, release or deployment is exercised.
