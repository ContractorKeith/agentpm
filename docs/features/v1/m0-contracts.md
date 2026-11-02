# M0 foundation contracts

The highest seam is `harness::execute(Request, fake_agent_path)`: a typed
command returns validated event envelopes or an error. The CLI is a thin
caller. It launches a separate deterministic executable and validates its
entire bounded output before emitting stdout. No durable supervisor or
production adapter is implied.

Wire protocol version is 1, independent of SQLite schema version 1.
Requests validate versions before launching effects. Fake-agent messages
also carry version 1; unknown fields and unsupported versions fail.
Events include request identity, contiguous sequence and logical milliseconds.
Sequences are per demonstration; SQLite event cursors are global monotonic
integers. IDs are distinct string newtypes with validated ASCII identifiers;
their generation belongs to the future supervisor. Unknown usage is an
explicit tagged value, never numeric zero. Evidence binds exact head, base,
ordered prerequisite heads, context package and check configuration, and
requires distinct implementer/reviewer sessions. Revision strings are opaque
identities in M0, not Git-validated commits.

Lifecycle transitions reject invalid edges without mutation. Milestone
review-to-ready and ready-to-completed require current evidence. Completed
means delivery, never product merge. Run completion currently accepts a
trusted service-supplied delivery-verification flag; M1/M9 must derive it
from durable milestone evidence rather than agent input. Human handback
always enters stale-base for revalidation. These are pure domain contracts,
not an authorization or takeover implementation.

The migration transaction creates projects, runs, milestones, requests,
events and evidence; enables foreign keys on each opened connection passed
to it; refuses newer schema versions; and is repeatable. State vocabulary,
JSON syntax, nonnegative counters, repository uniqueness, references and
independent evidence sessions are constrained by SQLite. SQL does not enforce
lifecycle edges or semantic JSON contracts: the future sole-writer service
must validate these and commit transitions plus events in one transaction.
Later milestones add context/attempt/artifact/operation storage and
reconciliation without treating this initial schema as a finished store.

The script is a bounded JSON array, while the child emits versioned JSON
Lines. Success, block, and failure terminate a response; additional output,
missing termination, malformed records, clock overflow, or exit/report
disagreement reject the entire response. Both script and output have a
64 KiB limit. Delay advances a virtual clock, enabling exact assertions
without timing flakiness. Failure exits 1; malformed/execution errors exit 2.
The sibling `fake-agent` binary must be built alongside `agentpm`.

No host calls occur in M0, so no speculative host interface is introduced.
M1 must supply durable request idempotency, event replay, real clocks,
timeouts, process ownership/cancellation and restart behavior. The synchronous
demo assumes the shipped finite fake executable; it is not safe for arbitrary
or hung production agents. M3/M9 must verify real Git/host revisions and
independent evidence. CI uses no real agents. Cross-platform CI success and
live adapter acceptance must be observed separately.
