# Repository claims live with the Git common directory and serialize on SQLite

Milestone claims are stored in a SQLite database inside the Git common directory. Any checkout or symlink of a repository therefore opens the same claim namespace, independent of caller-supplied data directories. Worktree protection is cooperative: callers that use agentpm share the same serialization; other tools are not stopped.

- Git runs without a shell, and every `GIT_*` repository, index and config selector inherited from hooks or tools is removed, including for `update-ref`'s separate process, so the explicit repository owns every operation.
- Claim operations hold `BEGIN IMMEDIATE` across external effects. Intent is persisted before effects; where a lock handoff is needed, the intent is committed and serialization reacquired, and the durable unique row protects the claim in between.
- Branch creation and deletion go through one `git update-ref --stdin` transaction together with the ownership marker, so a crash cannot leave an unowned branch that looks owned.
- Cleanup records its intent before removing the worktree, so a crash after removal can finish ref cleanup without guessing ownership from a missing path.
- Clean checks read `git ls-files -v` because `git status` and non-force removal trust assume-unchanged and skip-worktree flags and can miss edits.
- The supervisor attaches the claim store so supervisor and claim writes commit atomically across both files.
- Handback inspection never adopts edits as the expected revision or as clean evidence; reports require revalidation.
- Each run's reservation is recovered on its own: an unavailable repository keeps that run's reservation (and the repository's handback freeze) for the next retry and never fails unrelated work.
- The supervisor releases its store lock while an owner performs bounded shutdown.
