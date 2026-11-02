# agentpm

Local-first autonomous project execution in Rust. Turn an approved spec into independently reviewed GitHub PRs using Claude Code, Codex, and OpenCode.

**Status:** M0 foundation and fake-process demonstration; not yet a project manager.

See [the v1 plan](docs/PLAN.md) and [project vocabulary](CONTEXT.md). Intended surfaces: CLI, TUI, and local MCP; macOS and Linux. MIT licensed.

M0 provides typed contracts, an initial SQLite migration, and a deterministic
fake-process demonstration. It does not run real agents or manage projects yet.

Build both executables, then exercise the public command:

```sh
cargo build --workspace
cargo run --bin agentpm -- fake harness/success.json
```

Stdout is versioned JSON Lines; diagnostics use stderr. Exit codes: 0 for a
validated success or blocked report, 1 for a validated agent failure, 2 for
invalid input/protocol or execution errors. Blocking is a reported outcome,
not a waiting background process. Delay advances a logical clock; it does not
sleep. Scripts are JSON arrays with actions:
`success`, `malformed`, `block` (reason), `delay` (milliseconds), and
`failure` (reason), each tagged with `kind`.

Required gate (also run on macOS and Linux CI):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

Tests launch the real CLI and fake-agent processes without credentials or paid
APIs. See [M0 contracts](docs/features/v1/m0-contracts.md) for boundaries.

Owned worktrees (M3, independent of scheduling):

```sh
agentpm worktree --repo /path/to/repo allocate run-1 milestone-1 agentpm/run-1/milestone-1 --base main
agentpm worktree --repo /path/to/repo inspect run-1 milestone-1
agentpm worktree --repo /path/to/repo cleanup run-1 milestone-1
```

These commands emit one version-1 JSON allocation record; errors exit 2 with
stderr diagnostics. Use a pinned commit for `--base` when retrying across base
branch changes. Claims and generated checkout paths live in the canonical Git
common directory, shared by repository aliases and linked checkouts. Cleanup
only removes unchanged, clean owned allocations; committed worker output needs
later explicit revision reconciliation before it can qualify for cleanup.
Inspect returns the durable record (including interrupted-operation state), not
an assertion that the checkout still matches it. See [M3 contracts](docs/features/v1/m3-worktrees.md).
