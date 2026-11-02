# agentpm interview

## Confirmed decisions

- Name: agentpm.
- Purpose: a daily-use tool.
- Direction: autonomous orchestration, including task decomposition and worker coordination.
- Proposed core language from initial brief: Rust; detailed architecture remains open.
- Interview only; no implementation until shared understanding is confirmed.

## Confirmed round 1: autonomy contract

- Input: an approved spec or milestone issue.
- Completion: a reviewed, ready-to-merge PR. The human owns merging; release and deployment are outside autonomous completion.
- Ambiguity: escalate product decisions into the attention queue while continuing independent work. Routine implementation choices belong to the agent.
- Recovery: at most three repair attempts per task within a run budget, then escalate with evidence. Never weaken acceptance criteria to finish.

## Confirmed round 2: execution model

- Coordinator: a designated coding-agent session plans tasks and reacts to results; Rust enforces dependencies, budgets, and approval boundaries.
- Required v1 adapters: Claude Code, Codex, OpenCode.
- Routing: explicit project role assignments with per-run overrides; no silent provider switching.
- Execution: workers and supervisor on the same machine; macOS and Linux in v1. Remote workers deferred.

## Confirmed round 3: work and Git boundaries

- One repository per run; dashboard may supervise multiple runs.
- One independently testable milestone per branch, worktree, and PR; milestones may contain internal tasks.
- Stacked PRs are supported: dependent milestones may build on unmerged prerequisite branches.
- Ready-to-merge requires configured local checks, required CI, and a fresh reviewer session that did not implement the change. Blocking findings must be resolved; missing required checks block completion.

## Confirmed round 4: stacked PR lifecycle

- Automatically rebase owned dependent branches, update PR bases, and push with force-with-lease after prerequisite changes or merges; rerun checks and independent review for changed heads.
- Create integration branches for milestones with multiple unmerged prerequisites.
- Use the ready-to-merge label for PRs that passed gates even while stack dependencies remain outstanding; show those dependencies separately.
- Attempt conflicts involving known agentpm changes within the repair budget. Escalate unexpected human edits on that branch, preserving work and continuing unrelated milestones.

## Resolved glossary terms (transfer to CONTEXT.md when the repository is scaffolded)

- Ready to merge: a PR whose required checks and independent review passed for its current revision. Outstanding stack dependencies are displayed separately; this state does not mean the PR can immediately merge to mainline. Avoid: using ready to merge as a synonym for all dependencies merged.

## Confirmed round 5: integration branch lifecycle

- Integration bases are disposable, contain prerequisite changes, and are never merged directly into mainline. Dependent PRs target the integration base until prerequisites merge, then are rebased/retargeted to mainline and pass gates again.
- If upstream changes during dependent execution, let the dependent finish, then rebuild its base and rerun gates. Do not interrupt the worker for this update.
- If a prerequisite PR is closed without merge, the coordinator may redesign dependencies and continue.

## Confirmed round 6: replanning authority

- Coordinator may change tasks, dependencies, and implementation while preserving approved requirements. Changes to required behavior must be escalated.
- Follow explicit rejection instructions; clarify unexplained PR closure while unrelated work continues.
- Default run-wide allowance: three autonomous replans plus the run budget. Replacement tasks never reset these limits. Three repair attempts still apply within each task.

## Confirmed round 7: session lifetime and recovery

- A per-user background supervisor owns runs. Closing the TUI or disconnecting MCP does not stop them; explicit pause/stop controls execution.
- Recover automatically after reconciling persisted state with processes, worktrees, and GitHub. Reconnect/resume when state is established; escalate uncertain cases rather than replaying actions blindly.
- Human takeover grants exclusive control of the milestone worktree, pauses its worker and branch maintenance, and requires explicit handback. Inspect changes and invalidate affected checks before resuming; unrelated work continues.
- Default global concurrency: three active implementation/review workers, configurable, with lower per-project limits. Each run has at most one active coordinator session separately; waiting tasks use no worker slot.

## Confirmed round 8: budgets and accounting

- Enforce execution-time and attempt limits. Track reported usage and estimated costs when available, explicitly mark missing data, and treat spend thresholds as best-effort stops rather than guaranteed dollar ceilings.
- At an enforced limit, stop dispatch immediately, request controlled cancellation of active coordinators/workers, allow bounded shutdown grace, and terminate owned processes if needed. Preserve work and queue a human resume-or-cancel decision.
- Default configurable budget: two execution hours per run. Count wall time once while any coordinator or worker executes regardless of parallelism; exclude periods solely waiting for human input or CI. This is not a compute-cost measure.

## Confirmed round 9: shared context and skills

- Record worker context packages containing approved spec revision, milestone brief, applicable repository instructions, selected skills, and relevant dependency reports. Deliver later changes explicitly.
- Retain run history and artifacts automatically. Human approval is required before promoting discoveries into reusable project knowledge.
- Reference existing project/user skill directories, record versions/content used, and select skills explicitly per role. Skill authoring/installation remain outside v1.
- Workers route findings and requests through the coordinator; supervisor records delivery. Only the coordinator changes assignments. Workers may read relevant shared artifacts.

## Confirmed round 10: user controls

- Ship TUI, scriptable CLI with JSON output, and local MCP over the same supervisor. Defer Tauri/web companion.
- Durable cross-project attention queue includes blocking reason, evidence, and actions; optional desktop notifications announce newly blocked work. Decisions survive disconnect/restart.
- MCP operates runs within configured authority but cannot approve product-scope changes, promote project knowledge, or grant itself more authority. Reserved decisions require explicit human input.
- Workers are trusted local tools under the user's account. v1 provides workflow enforcement, not an OS security sandbox; worktrees do not restrict filesystem or service access.

## Confirmed round 11: delivery acceptance

- GitHub only for v1 PRs, CI, and review state, using existing authentication. Isolate host-specific operations behind an interface; other hosts deferred.
- Deterministic fake-agent CI exercises orchestration failures. Live smoke tests validate Claude Code, Codex, and OpenCode on both macOS and Linux; record actual adapter capability gaps.
- Acceptance includes an approved spec with two parallel prerequisites and a dependent milestone using an integration base, reviewed PR delivery, and correct retargeting/revalidation after human merges. Controlled runs also exercise restart recovery, takeover/handback, and budget exhaustion.

## Interview status

All eleven rounds answered and shared understanding confirmed on 2026-11-02. User authorized spec, tickets, and build with new T3 threads using Sol 6.1 workers and primary-agent orchestration/review.

## Spec follow-through

- Carry all confirmed decisions into the specification; preserve these interview notes as provenance.
- Transfer resolved vocabulary to root CONTEXT.md when the project is scaffolded.
- Offer a short ADR for disposable integration bases and revision-bound validation: a consequential alternative to serial-only execution and directly merged integration PRs.
- Detail adapter capability contracts, runtime storage, retention, IPC, and supported takeover mechanisms during specification and technical research; do not claim these implementation details were decided by the interview.
- Approval controls are workflow boundaries for trusted local agents, not security isolation from arbitrary CLI execution.
- Evidence for a dependent branch that finishes on an outdated prerequisite must not qualify it as ready until base reconstruction and required gates complete.
