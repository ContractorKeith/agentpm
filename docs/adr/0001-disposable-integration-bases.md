# Disposable integration bases with revision-bound evidence

Dependent milestones may start before multiple prerequisite PRs merge. Compose pinned prerequisite heads into disposable integration bases, never merge those bases directly into mainline, and retarget/revalidate dependent PRs after prerequisites merge; this accepts stack-maintenance complexity to preserve parallel throughput and milestone-sized review diffs.
