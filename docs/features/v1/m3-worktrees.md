# M3 owned milestone worktrees

`git::GitService::{open, allocate, inspect, cleanup}` is the narrow synchronous
service seam; `agentpm worktree --repo PATH` exposes it through the single binary.
M8 supplies scheduling. No supervisor, context, domain state or M0 schema changes
are included. No dependency was added.

Repository identity is the canonical Git common directory. Every Git subprocess
clears inherited GIT_* selectors/configuration before using the explicit repository. A repository-local
`agentpm-claims.sqlite3` keeps unique run/milestone, branch and generated path
claims across processes and restarts. SQLite immediate transactions serialize
cooperating callers. Allocation persists intent before Git effects. A Git ref
transaction atomically creates an absent branch plus its private ownership ref
under `refs/agentpm/owners/RUN/MILESTONE`, both pointing to the pinned base commit.
A retry checks recorded request parameters and reconciles missing checkout
creation without adopting a preexisting unowned branch. Changed base arguments
are conflicts. Allocation success requires the branch tip and checkout HEAD to
match the recorded expected revision; moved revisions require reconciliation. Completed claims remain tombstones; identifiers cannot be reused.
The private refs and database must be retained together and are not host refs.

Cleanup requires the ownership ref, registered linked-worktree identity and
backlink, expected symbolic branch and expected HEAD, unchanged branch ref,
and no tracked, untracked or ignored changes. Index entries marked assume-unchanged
or skip-worktree block cleanup because Git status can conceal their edits. The primary checkout is excluded.
It persists cleanup intent, invokes Git worktree removal without force, then
atomically deletes branch plus ownership ref using expected old SHAs. A retry
can finish after checkout removal or ref deletion. Unexpected states preserve
remaining data and return an error for manual reconciliation. No recursive
filesystem removal, branch force deletion, reset or checkout of the user's
active tree is used. A failed allocation retains its claim for retry/inspection.

Inspect returns recorded intent/receipt, not a fresh cleanliness attestation.
The initial expected head is deliberately immutable in M3: commits made by
workers require a later explicit revision/ownership handoff before cleanup.
Takeover, branch maintenance and evidence invalidation belong to later milestones.
Git and SQLite are not one atomic transaction; incomplete states are exposed,
not guessed successful. A partial Git worktree-add failure may require manual
repair. Same-user tools can alter Git metadata/files outside these locks; these
are cooperative workflow controls, not a sandbox. Expected-SHA ref transactions
preserve moved commits even if a noncooperating tool races checkout removal.

`tests/git_worktrees.rs` uses real temporary repositories, library commands and
independent binary processes: parallel distinct milestones, duplicate requests,
symlink/linked-checkout identity collisions, six competing branch claims, dirty
tracked/untracked/ignored data, unexpected commits, lost ownership refs, detached
checkouts, moved branches, recovery between Git effects and receipts, literal
shell metacharacters and safe idempotent cleanup. M0 behavior tests are preserved.
Local results are macOS evidence only; Linux CI and independent review remain
separate integration requirements.
