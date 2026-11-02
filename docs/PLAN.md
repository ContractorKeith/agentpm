# agentpm v1

## Problem statement

Developers running several coding agents must manually preserve context, allocate worktrees, watch for questions, reconcile dependency branches, and determine whether a PR is actually reviewed. Terminal multiplexing does not own that execution lifecycle. Keith needs a daily-use autonomous project manager that executes an approved specification without silently changing the intended product.

## Solution

Submit an approved spec or milestone issue for one Git repository. A designated coding-agent coordinator proposes milestone tasks and reacts to worker reports. A Rust supervisor owns durable state, process lifetimes, dispatch limits, Git operations, and workflow authority. Claude Code, Codex, and OpenCode workers implement and independently review changes. Each milestone produces one PR, branch, and worktree. The user merges PRs; agentpm maintains dependent stacks and revalidates changed revisions.

CLI, TUI, and local MCP share one per-user supervisor. Closing clients never stops runs. A durable attention queue collects unresolved product decisions, uncertain recovery, and exhausted budgets across projects.

## User stories

1. Submit an approved file or GitHub issue, preserve its revision, and see a proposed execution graph without repeating the planning interview.
2. Configure coordinator, implementer, and reviewer backend/model assignments per project, override them per run, and never encounter silent provider switching.
3. Run independent milestones concurrently in owned worktrees while preserving the approved requirements and one PR per milestone.
4. Review a dependent PR against an unmerged parent or disposable integration base without reviewing all prerequisite changes again.
5. Keep a worker running when an upstream base changes, then rebuild its base and rerun checks/review before accepting its output.
6. See current-head checks and independent review evidence, including stack dependencies even when the product status says Ready to merge.
7. Reject an implementation and let the coordinator propose an alternative within approved requirements; clarify unexplained PR closures and scope changes rather than silently canceling behavior.
8. Close the TUI and later reconnect; after a crash recover known state without duplicated workers, commits, or PRs.
9. Take exclusive control of a worktree, hand it back explicitly, and have agentpm inspect changes and invalidate stale evidence.
10. Enforce time, repair, replan, and concurrency limits across restarts; see missing cost data as unknown rather than zero.
11. Receive context and skill snapshots with provenance and explicit updates, retain run history, and approve knowledge promotion separately.
12. Operate runs through local MCP without allowing that interface to approve human-only decisions.
13. Diagnose unsupported adapter features honestly and exercise all three backends on macOS and Linux before a stable release.

## Implementation decisions

### Ownership and architecture

Rust core and binary with thin CLI/TUI/MCP clients. Use stable Rust and commit Cargo.lock. Tokio is appropriate for concurrent process/I/O lifecycles; SQLite provides transactional durable state without an external service; clap/serde provide typed command and wire contracts; ratatui/crossterm provide the terminal interface. Add dependencies only when their role is necessary, and lock versions during implementation.

The supervisor is the sole workflow writer. Keep modules deep: domain/state transitions; durable store; supervisor command service; process/backend adapters; context packaging; Git/worktrees; GitHub; client transports. Tests target the command service and real child processes, with fake agents and a fake host service at external boundaries. Do not build a generic workflow language or plugin framework.

Local IPC uses a user-private Unix socket on both supported systems. Request IDs make retried mutations idempotent. A protocol version is negotiated; unknown fields/versions fail clearly. Clients subscribe from event cursors and can recover gaps through snapshots. Bounded messages and log retention/streaming must prevent an agent flooding supervisor memory. SQLite transactions couple accepted state transitions and events. External effects have persisted intent, execution identity, and reconciliation rather than fictional atomicity with GitHub/processes.

Use an explicit data-directory override for hermetic tests and multiple installations. Private runtime data contains SQLite state, context/artifact blobs, process logs, socket/lock files, and backend session references. Repository configuration is versionable and contains role assignments, checks, skill paths, and limits, never credentials. Use existing CLI/provider authentication and GitHub authentication; never copy credential files into context packages. Retain history until explicit pruning; never automatically delete dirty worktrees or unreconciled evidence.

### Data and transitions

Stable IDs for Project, Run, Milestone, TaskAttempt, ContextPackage, Artifact, BackendSession, Worktree, PRBinding, Evidence, AttentionItem, HumanDecision, and ExternalOperation. Record schema/protocol versions, timestamps, provenance, explicit unknown values, and monotonic event sequence. Store revision identities, not just branch names. No opaque model transcript is the authoritative scheduler state.

Runs move through queued/running/waiting/paused/completed/failed/canceled states. Milestones additionally distinguish implementing, checking, reviewing, repairing, stale-base, ready, and human-owned work. Exact Rust enums are M0's contract, with transition tests. Terminal completion requires evidence, never an agent's assertion alone. Completion means delivery of reviewed PRs; merge, release, deployment are human-owned product operations.

Coordinator output is structured and schema-validated. Proposals may add/revise task graphs within approved requirements; Rust validates cycles, ownership, capabilities, budgets, and dependencies before effects. Invalid output is reported for bounded correction. Worker reports flow through the coordinator; workers cannot independently reschedule peers. A reviewer is a fresh session not responsible for implementation. Implementers cannot mark their own review passed. Required checks, CI, and blocking findings are independently reconciled.

### Dispatch and limits

Default global worker limit is three active implementation/review workers, with configurable lower project limits. Each run has at most one active coordinator separately; wait states use no worker slot. Fair scheduling must prevent a busy project starving another. Persist counts using actual execution identities and reconcile them before dispatch on restart.

Three repair attempts per task and three autonomous replans per run are defaults. Replacement tasks cannot reset run replan or time counters. Preserve logical lineage when replacing failed work. Scope changes require a human decision. Explicit PR rejection instructions control replanning; unexplained closure raises one targeted attention item while unrelated work continues.

Default execution budget: two hours of wall time while at least one worker/coordinator is executing, counted once despite parallelism. Human/CI-only waits are excluded. A monotonic clock measures live intervals; persisted checkpoints and conservative reconciliation handle restart without gifting a fresh allowance. On exhaustion stop dispatch, cancel active execution, allow bounded shutdown grace, then terminate only proven owned processes. Preserve work and ask resume/cancel. Resume does not silently replenish exhausted limits; an explicit human extension records the new allowance. Usage/cost are reported or estimated with provenance; absent is unknown. Spend stops are best effort, never guaranteed dollar ceilings.

### Processes, adapters, and recovery

Each adapter advertises tested capabilities: launch, stream structured events/output, identify session, cancellation, usage, approvals, resume, and supported takeover mechanism. Use documented native APIs/protocols where available, verified against installed versions and official documentation. Do not infer semantic completion from terminal text or call arbitrary command launching full support. Unsupported required capabilities produce actionable diagnostics before dispatch.

The process boundary must track process identity beyond a PID, process group ownership, cwd, launch/session tokens, and durable operation IDs. Supervisor restart first reconciles live processes, backend sessions, worktrees, and remote PRs; reconnect/resume only when established, otherwise escalate. Never blindly replay a prompt or GitHub mutation. Explicitly test child survival versus termination semantics on both OSes. Machine reboot does not preserve running processes; session resume is adapter-dependent.

Human takeover obtains exclusive milestone ownership, stops/pauses its worker safely, and freezes branch maintenance. Explicit handback examines the worktree and branch before generating new evidence. Unexpected human branch commits outside takeover also stop maintenance and request a decision. Trusted same-user tools are not security-isolated: these are workflow controls, not an OS sandbox.

### Git and GitHub

One run operates one repository. Canonicalize repository identity and enforce branch/worktree ownership across runs. Worktree allocation and cleanup use system Git with argument arrays, inspect existing state, and preserve unrelated/dirty files. Branch updates use expected old SHAs and force-with-lease where rewriting is necessary; never blind force push. Never modify the user's current checkout as a worker checkout.

Single-parent stacks target the prerequisite branch. Multiple unmerged prerequisites produce a deterministic disposable integration base from pinned prerequisite heads; the dependent PR targets that base so its diff isolates the milestone. Integration branches are never merged directly into mainline. Once prerequisites merge, rebuild/rebase onto mainline, retarget dependent PRs, and rerun evidence. Handle merge commits, squash, and rebase merges using recorded commit ancestry/content and explicit ambiguity handling rather than assuming one merge method.

An upstream change while C runs does not interrupt C. Mark its desired base stale immediately and allow its current attempt to finish; it cannot qualify as ready until rebased/rechecked/reviewed against the updated base. Repeat changes are coalesced and revalidated. Conflicts in known agent changes get bounded repair; unexpected human edits escalate. Preserve recoverable work when rebuilding integration bases.

GitHub is the only v1 host. Isolate operations behind a host interface; production uses existing authentication (gh is an acceptable bridge). Read full issue bodies/comments and snapshot their provenance. Persist PR identity and external-operation keys, reconcile create/update results after timeouts, and avoid duplicate PRs. Required CI absent, queued, failed, canceled, or for another SHA never counts as passing. Review evidence is bound to head plus base/relevant prerequisite revisions, check configuration, context revision, and independent session identity.

Ready to merge means required gates passed for the current validated revision; outstanding stack dependencies display separately. It does not assert immediate mergeability into mainline. The product never merges autonomously. In this repository's build workflow, the orchestrator may merge implementation PRs after the prescribed reviews under Keith's build authorization; that is separate from product behavior.

### Context, skills, and authority

Context packages snapshot approved spec revision, milestone brief, applicable repository instructions, selected skill contents/versions, and dependency reports. Updates are explicit, versioned, and acknowledged. Read existing project/user skills without rewriting their canonical sources. References are bounded and checked for intended scope; do not package credentials. Discoveries remain artifacts until human-approved project-knowledge promotion. Approval references exact content; a changed artifact needs a new approval.

Attention items have stable IDs, reason, evidence, allowed actions, target revision, resolution actor, and deduplication key. Stale decisions cannot approve newer content. Optional desktop notifications point back to the durable queue. MCP can submit approved work and operate runs within configured policy, but cannot approve scope changes, promote knowledge, or expand its own authority. Separate human decision commands from agent-callable methods. Same-user arbitrary CLI access is outside the security guarantee.

### CLI and client contract

One binary, agentpm. Command families: doctor; supervisor start/status/stop; project configuration; run start/list/show/pause/resume/cancel; events/logs; attention list/show/resolve; milestone takeover/handback; knowledge propose/approve; tui; mcp. Worker milestones finalize concise subcommand spelling with help and compatibility tests. JSON output is versioned and stdout contains protocol data only; diagnostics go to stderr. Noninteractive commands never prompt unexpectedly. Reject conflicting flags and unknown identifiers without side effects. Human decisions cannot be forged by invoking an MCP tool with a human=true field.

TUI defaults to runs and cross-project attention, with drill-down into milestones, dependency/base state, worker logs, usage, and actionable evidence. Display unknown usage explicitly. Destructive stop/prune and handback decisions show their concrete targets. This is a developer control plane; no IDE/editor clone or generic chat interface.

## Testing decisions

Highest seam: submit commands to the real supervisor service and assert externally visible durable state/events and effects. Use deterministic fake backend executables and a fake GitHub host; temporary real Git repositories prove branch semantics. Add only narrow clock/process/host seams required for determinism. Process integration tests start the real binary and disconnect clients/restart the daemon. Never require paid live agents in ordinary CI.

Test duplicate request IDs, dropped responses, restart between intent/effect/receipt, malformed coordinator output, cyclic graphs, concurrent runs claiming one branch, process identity mismatch, cancellation descendants, budget accounting during overlapping workers/waits, stale approvals, unknown usage, exhausted repair lineage, API pagination/rate errors, late CI for old SHAs, human edits, squash-merged prerequisites, and diamond dependency rebuilds. Verify evidence invalidation on both head and base changes.

Required automated gate after M0: cargo fmt --all -- --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace; cargo build --workspace --release. CI runs on Linux and macOS. Worker changes must add meaningful behavior tests through the highest seam and keep neighboring docs accurate.

Release requires live smoke tests for each of three adapters on both OSes, recording versions, exact tested capabilities, commands, results, and limitations. Daily-use acceptance demonstrates two parallel prerequisites plus an integration-base dependent PR, independent review, human merges followed by retarget/revalidation, restart recovery, takeover/handback, and budget exhaustion. Packaging alone is not release acceptance. Record unavailable infrastructure as incomplete evidence, never a pass.

## Milestone ledger

| Milestone | Outcome | Blocked by | End-to-end assertion |
|---|---|---|---|
| M0 | Contracts, fake harness, schema, CI | none | Valid/invalid state transitions and fake-agent protocol proven on both CI OSes |
| M1 | Durable supervisor and fake run | M0 | Start a run, detach client, reconnect and observe persisted results |
| M2 | Context and skills | M0 | Snapshot configured inputs, retrieve exact revisions, apply explicit update |
| M3 | Owned Git worktrees | M0 | Allocate isolated milestone checkout; refuse collisions and dirty cleanup |
| M4 | Budgets, recovery, takeover | M1, M3 | Exhaust/restore/reconcile a run and transfer exclusive ownership |
| M5 | Claude Code adapter | M1, M2 | Versioned adapter starts/cancels/reconciles a session with honest capabilities |
| M6 | Codex adapter | M1, M2 | Same contract for Codex |
| M7 | OpenCode adapter | M1, M2 | Same contract for OpenCode |
| M8 | Coordinator and scheduling | M2, M3, M4, M5, M6, M7 | Approved spec becomes validated tasks, dispatches within limits, replans boundedly |
| M9 | Reviewed GitHub PR delivery | M8 | Implement/check/review/repair produces one current-evidence PR without merging |
| M10 | Stacked and integration PRs | M9 | Diamond graph rebuilds and retargets after prerequisite changes/merges |
| M11 | Attention, human controls, MCP | M4, M8, M9 | Durable decisions work through human CLI; MCP cannot approve reserved actions |
| M12 | Daily-use TUI | M10, M11 | Reconnect, inspect dependency readiness, act on attention, take over/hand back |
| M13 | Live acceptance and packaging | M12 | All six live adapter/OS cases and required operational scenarios recorded |

M0 lands alone. After M0, M1/M2/M3 may run concurrently; merge order M1 then M2 then M3. After prerequisites, M4/M5/M6/M7 may run in disjoint modules; merge M4 then M5 then M6 then M7. After M9, M10/M11 may run concurrently, merge M10 then M11. Rebase and rerun gates after integration. Each milestone is one issue, branch, worktree, draft PR, and fresh Sol 6.1 T3 worker. The orchestrator validates complete diffs and checks; independent reviewer and live adversarial review precede ready/merge. Release has a separate skeptic/owner acceptance gate.

## Out of scope

GUI/Tauri/web companion, remote execution, non-GitHub hosts, direct Ollama/MLX model serving, automatic provider routing, peer delegation, skill installation/catalog, autonomous merges/releases/deploys, OS sandboxing, and autonomous knowledge promotion.

## Further notes

The eleven-round interview is preserved in docs/features/v1/interview.md. The user confirmed the scope and authorized spec, tickets, and build on 2026-11-02. No runtime implementation exists at specification creation. Adapter-specific protocol details and live environment availability must be verified during their milestones; missing support cannot silently narrow the agreed acceptance bar. Existing public projects are inspiration, not source dependencies or evidence of a vacant market.
