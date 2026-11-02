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

Context snapshots are available through the same binary:

```sh
agentpm context --data-dir /absolute/private/data create /absolute/manifest.json
agentpm context --data-dir /absolute/private/data show REVISION
agentpm context --data-dir /absolute/private/data update REVISION --acknowledge REVISION /absolute/manifest.json
```

The manifest is versioned JSON. All paths must be absolute; roots explicitly
allow collection. For example (replace paths with existing files):

```json
{
  "version": 1,
  "roots": ["/repo", "/user/skills"],
  "approved": {"path": "/repo/spec.md", "source_revision": "approval-42"},
  "brief": {"path": "/repo/brief.md", "source_revision": "brief-1"},
  "instructions": [{"path": "/repo/AGENTS.md", "source_revision": "commit-sha"}],
  "dependencies": [],
  "roles": {
    "implementer": {
      "backend": "codex", "model": "gpt-6.1-sol",
      "skills": [{"path": "/user/skills/example/SKILL.md", "source_revision": "skill-1"}]
    }
  },
  "overrides": {}
}
```

Overrides replace entire existing roles. Skill references are not followed;
list each required file explicitly. Missing inputs, credential paths, scope
violations and collection limits fail with exit 2 and no stdout. Create/update
return versioned JSON with a revision; show returns its embedded contents.
See [M2 contracts](docs/features/v1/m2-context.md) for provenance and limits.
M1 adds a detached local service for fake runs (no real adapters or project
scheduler). Build both binaries, then use an absolute script path:

```sh
agentpm --data-dir "$HOME/.agentpm" supervisor start
agentpm --data-dir "$HOME/.agentpm" run start "$PWD/harness/success.json" --command-id example-1
agentpm --data-dir "$HOME/.agentpm" run show example-1
agentpm --data-dir "$HOME/.agentpm" events --cursor 0
```

The data directory must have mode 700. `supervisor status` reports service PID;
`supervisor serve` runs in the foreground for service managers. Reuse the same
command ID and exact payload to retry a start safely. Events return one bounded
event per page; use its cursor for the next page, stopping at an empty page.
Service diagnostics are in `supervisor.log`. Fake delay actions use real time
inside the service; the original `fake` demo retains its virtual clock.
See [M1 contracts](docs/features/v1/m1-supervisor.md) for limits and recovery
boundaries. `libc` supplies Unix advisory locking, private file creation and
termination of newly owned child process groups on macOS/Linux.
