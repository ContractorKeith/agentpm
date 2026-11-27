# Humans can attest an unknown execution ended, but not while it is visibly alive

`agentpm run reconcile RUN --confirm-ended` is the supported way out of an unknown execution. It releases the slot and worktree reservation. Refusals elsewhere name this command.

- An attestation is a claim, not proof. It is refused while any live process carries one of the run's execution tokens in its environment, or (for a run bound to a worktree) has its cwd inside that worktree. A match proves the execution has not ended; no match proves nothing. The check fails closed when the process table cannot be read. There is no override flag.
- An unbound run's cwd is the supervisor's own, so only bound runs are checked by cwd.
- Pre-upgrade runs have no identity rows; the human vouches for the recorded session token.
- Linux reads `/proc/*/environ` and `/proc/*/cwd`; unreadable entries (exited or other-user processes) are skipped because they cannot be our worker. macOS uses `ps -axwwE` (which appends the environment of the caller's own processes) and `lsof -Fpn -d cwd`.
- Accounting stays conservative (ADR 0004).
