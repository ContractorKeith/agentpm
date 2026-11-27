# No code comments

Comments drift from the code, repeat it, or excuse a shortcut that then spreads. Reasons belong in names, types, tests, or an ADR here; help text belongs in clap `about` attributes.

`tests/comment_ratchet.rs` scans every `.rs` file and fails when a file has more comment lines than `tests/comment-baseline.txt` allows (absent means zero), or fewer, so the baseline only moves down. `AGENTPM_BLESS_COMMENTS=1 cargo test --test comment_ratchet` lowers the baseline; `AGENTPM_LIST_COMMENTS=1 cargo test --test comment_ratchet -- --nocapture` lists every remaining comment. `/delete-comment` (`.agents/skills/delete-comment`) repeats the cleanup.

The 2026-11-27 pass removed all 138 comment lines: decision records moved into ADRs 0002–0008, excuses became issues, the rest were deleted.
