# Guardian receipts are the only proof an execution ended

Each worker runs under a separate guardian process (`execution-guard`) that survives supervisor death, holds the worker `Child`, and keeps the execution deadline. Completion is proven only by the guardian's durable receipt, which is bound to a nonce and the snapshot hash and published after worker ownership ends. PID values, elapsed timers and process absence never prove completion, and an uncertain launch is never replayed.

- Recovery reconciles authoritative receipts first and classifies only unresolved intervals as unknown.
- A complete receipt whose repository is unavailable is "deferred": it keeps its slot and is retried, but is not classified unknown.
- Any path without an authoritative receipt stays reserved and uncertain.
- Error paths keep the `Child` until cleanup and reap succeed. When the guardian may merely be descheduled, the supervisor does not kill it, because that could orphan the worker's separate group; it records uncertainty instead.
- Ownership certainty is a typed value, independent of untrusted protocol diagnostics.

We accept runs that sit in an unknown state until a human resolves them (ADR 0005) in exchange for never double-running a worker.
