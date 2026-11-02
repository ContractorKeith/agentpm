# M4 execution controls and usage evidence

The actual `supervisor::Command` service and `agentpm run` CLI enforce limits on
M1 fake runs. Schema 4 preserves v2/v3 rows and immutable scripts. Unknown
historical inputs are never invented. Each fake run is one logical task;
repairs use the same run and input, consume one of three repair attempts, and
retain elapsed time. Replan commands consume one of three durable run credits;
no coordinator/task-graph scheduler exists yet. Mutations use durable request
IDs; exact retries do not launch or spend twice. A new run is a separately
submitted specification, never the implementation of a replacement task.

`run start SCRIPT --command-id RUN` retains the two-hour default. For tests or
explicit lower limits, `run start-controlled SCRIPT --command-id RUN --project P
--allowance-ms 1000` uses the same launch path. `run controls RUN` shows consumed
milliseconds, allowance, stop/identity/human ownership flags, repair/replan
counts and explicitly unknown usage. `run policy SLOTS PROJECT_CAP --command-id
ID` lowers global/default project limits (1–3); `run project-cap PROJECT CAP
--command-id ID` records an individual project cap. Admission reserves queued
intents as well as live processes; unresolved restart identities retain slots.
No fair scheduler or coordinator dispatch is claimed in this fake-run slice.

`run cancel RUN --command-id ID` requests cancellation. The owner sends TERM,
waits a bounded grace, then KILLs its still-reserved worker group before reaping. Input,
stdout and stderr progress together under nonblocking bounded work per tick;
retained output is bounded at 64 KiB per stream. Escaped descendants cannot
hold completion open through inherited pipes. Only the live Child owner signals
its unreaped group, never a database PID.

A separate `execution-guard` sibling binary owns the actual fake worker and
keeps its deadline after service death. It accepts at most 64 KiB under a two
second input deadline (also bounded by the execution deadline), verifies the
snapshotted SHA-256 before launching, and gives the worker 50 ms shutdown grace.
The service requests guardian shutdown and waits up to 200 ms. If shutdown is
unconfirmed it retains an uncertain slot instead of killing the guardian while
it may hold a worker. The guardian owns the worker group and its 50 ms grace. The
recorded PID identifies the guardian. Install all three binaries together.
Killing the guardian independently, escaping process groups, or same-user
metadata tampering is outside this cooperative ownership guarantee.

A monotonic interval is checkpointed during execution and on completion.
Sequential repairs add to the retained run total. Terminal/blocked and human
waits consume no time. The current fake supervisor has one execution per run,
so its active union is exactly that execution interval; parallel runs have
independent budgets. Multiple workers/coordinator within one run are not yet
dispatchable and must share this run allowance when M8 supplies scheduling.
Restart cannot reconnect the fake adapter: it transactionally exhausts uncertain
allowances, retains slots, and emits a deduplicated recovery attention event.
It never signals stored PIDs, releases uncertain claims, or replays prompts.
This conservative policy may overcharge downtime. There is no automated slot
release or adoption for uncertain runs; manual reconciliation is still required.

`run repair RUN --command-id ID` dispatches the immutable input again, within
remaining time and repair credits. `run extend RUN MILLISECONDS --command-id ID`
records a human extension for a stopped/terminal run without resetting consumed
time or counters. `run resume RUN --command-id ID` is an explicit human replay
of a known terminal attempt, not an autonomous repair-credit reset. Unknown
identity or exhausted allowance blocks replay. Future agent/MCP adapters must
not expose extension or human resume as autonomous authority.

For takeover, first allocate an M3 worktree with the same RUN ID, then use
`start-controlled ... --repository REPO --milestone M`. The worker cwd and Git
binding are recorded. `run takeover RUN --command-id ID` freezes the repository
claim under M3's immediate transaction, requests shutdown, and confirms exclusive
human ownership only after the execution owner finishes. Failure to confirm
leaves maintenance frozen and returns a pending diagnostic. Unknown recovery
identities cannot be granted exclusive ownership.

Allocation retries and cleanup obey the same repository-local freeze, including
independent cooperating CLI processes. `run handback RUN --command-id ID`
inspects HEAD, symbolic branch and tracked/untracked/ignored status, persists its
inspection in the repository claim database, and records an evidence-invalidated
event before returning. It never adopts human commits as the expected revision.
Dirty/changed branches remain protected by M3 checks; replay rechecks ownership.
No product review evidence exists in fake runs; M9 must consume revalidation
requirements when its evidence pipeline exists. These controls do not sandbox
other tools running as the same user.

## Local verification

`cargo test --workspace --locked` preserves the 34 pre-M4 tests and adds five
supervisor/process integration scenarios (39 total). They cover real temporary
Git takeover/handback with preserved edits; short-budget exhaustion; global and
project admission; cancellation; three actual repair dispatches and three replan
credits with restart persistence; service crash without duplicate dispatch;
guardian deadline/reaping; blocked stdin; and an escaped pipe holder. The schema
upgrade test now expects version 4 while retaining unknown v2 script bytes.
The guardian test uses a one-second deadline to include macOS process startup;
subsecond tests target already-owned process I/O rather than require a worker to
finish startup inside 120 ms.

Required local gates: fmt, locked clippy with warnings denied, locked workspace
tests, and locked release build. This is local macOS evidence only. Linux CI,
independent attack/review, production backend capability tests and release
acceptance remain separate. No paid API, product merge or deployment occurs.
