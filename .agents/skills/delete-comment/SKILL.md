---
name: delete-comment
description: Remove code comments from agentpm and lower the comment ratchet. Classifies each comment as tautology, stale, excuse or decision record; deletes it, files an issue for excuses, and moves decisions into docs/adr/. Use for '/delete-comment', 'clean up comments', or when tests/comment_ratchet.rs fails because a comment was added.
---

# Delete comments

Comments are banned in agentpm (docs/adr/0009-no-code-comments.md). `tests/comment_ratchet.rs` enforces it against `tests/comment-baseline.txt`, which may only go down.

## 1. Inventory

```sh
AGENTPM_LIST_COMMENTS=1 cargo test -q --test comment_ratchet -- --nocapture | grep -E '^[^ ]+\.rs:[0-9]+:'
```

Scope is the listed files, or the user's argument (a path or a diff such as `git diff main...HEAD`). Read each comment with the code around it before judging it.

## 2. Classify and act

| Class | Test | Action |
|---|---|---|
| Tautology | Restates what the name, signature, test name or assertion already says | Delete |
| Stale | Names code, milestones, schema versions or behavior that no longer hold | Delete |
| Excuse | Justifies a shortcut: a precondition enforced only by prose ("caller must hold…"), a retry around a race, a "temporary" hack | Delete, then file one GitHub issue per hack with `gh issue create` naming the file, function and the fix |
| Decision record | A non-obvious why the code cannot show: an invariant, platform quirk, safety or ordering rule | Delete, and add it to the ADR in `docs/adr/` that covers that area, or a new numbered ADR in the same short style |

Rules:
- Clap `///` help text is not a comment: convert it to `#[command(about = "...")]` or `#[arg(help = "...")]`, drop the trailing period, and confirm `--help` output is byte-identical before and after.
- Before writing an ADR line, check the claim against the code. Do not copy a comment you have not verified.
- When unsure between tautology and decision record, ask whether a careful new reader would make a mistake without it. If yes, it is a decision record.
- Prefer a clearer name or a test over an ADR line when the reason is local to one function.

## 3. Ratchet and gates

```sh
AGENTPM_BLESS_COMMENTS=1 cargo test -q --test comment_ratchet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Bless only lowers the baseline; an increase still fails. Commit the lower `tests/comment-baseline.txt` with the deletions.

## 4. Report

Give the user a table of every comment (file:line, class, action), the issues filed, the ADRs touched, and lines removed (`git diff --numstat`).
