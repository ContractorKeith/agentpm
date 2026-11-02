# agentpm

A Rust autonomous project manager for approved specs: local Claude Code, Codex, and OpenCode workers produce independently reviewed GitHub PRs. Users own product merges. v1 acceptance is the full workflow in docs/PLAN.md on macOS and Linux.

## Status
Planning scaffold, 2026-11-02. Implementation begins at M0; no runtime exists yet.

## Commands
M0 establishes: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --workspace --release`. These are required once Cargo exists.

## Pointers
- docs/PLAN.md — canonical spec, architecture, milestone map, acceptance.
- CONTEXT.md — glossary; docs/adr/ — decisions.
- docs/features/v1/ — interview, tracking, worker briefs and acceptance evidence.
- Central ../worklog — session state; no repository WORKLOG.md.

## Conventions & Gotchas
- Follow user-approved scope. Rust supervises; agents propose. Worktrees are not a security sandbox.
- Evidence binds head and base revisions. Missing CI or usage never means passing or zero.
- No silent backend switching or resetting budgets through task replacement.
- Worker model: Codex gpt-6.1-sol in a T3-managed worktree, one milestone per draft PR. No further delegation by workers unless explicitly assigned.
- Orchestrator owns integration, full-diff review, independent review/attack and merge. Workers never merge or release.
- Commit as ContractorKeith only, no AI attribution. Preserve unrelated changes.

## Tracking
GitHub issues are the milestone ledger; one Basecamp card summarizes this repo. See docs/features/v1/TRACKING.md when created. M0 lands alone; parallel/merge order is in PLAN.md.

## Out of Scope
GUI, remote workers, other Git hosts, OS sandbox, autonomous product merge/deployment, provider auto-routing, skill installation, direct model hosting.
