# agentpm

Language for autonomous execution of approved software work.

## Language

**Run**: Execution of one approved specification against one repository, with persistent limits and history.
_Avoid_: Session, project (when referring to an execution).

**Milestone**: An independently verifiable delivery unit with one branch, worktree, and PR; it may contain internal tasks.
_Avoid_: Worker, attempt.

**Attempt**: One execution of a task by an agent, including a distinct repair attempt.
_Avoid_: Milestone, run.

**Coordinator**: The designated agent that proposes work and interprets results within approved requirements.
_Avoid_: Supervisor.

**Supervisor**: The Rust service that owns durable state, dispatch, processes, and workflow constraints.
_Avoid_: Coordinator, agent.

**Integration base**: A disposable branch combining pinned unmerged prerequisites for a dependent milestone; never directly merged into mainline.
_Avoid_: Release branch, integration PR.

**Ready to merge**: A PR whose required gates passed for its current validated revision. Outstanding stack dependencies display separately; this does not mean it can immediately merge into mainline.
_Avoid_: Dependencies merged, released.

**Context package**: A recorded revision of the approved spec, brief, instructions, selected skills, and relevant dependency evidence delivered to a worker.
_Avoid_: Shared mutable prompt.

**Attention item**: A durable request for a human decision with evidence and permitted actions.
_Avoid_: Notification (which merely announces an item).

**Takeover**: Exclusive human control of a milestone's worktree until explicit handback.
_Avoid_: Concurrent editing.
