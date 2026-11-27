# Context snapshots are explicit, immutable and stored outside the supervisor database

Context is collected only from explicitly listed absolute roots and inputs; there is no source discovery and no dispatch authority. Role overrides replace a whole role; there is no implicit provider fallback.

- Snapshots live in a filesystem content store, separate from the supervisor database.
- Publication stages to a temp file and hard-links it into place: readers never see partial data and an existing snapshot is never overwritten.
- An update acknowledgment pins exactly the prior content; changed source is read only during update.
- Every entry point enforces the same metadata bound, including library callers that skip the manifest file.
- Inputs are checked to be regular files before opening, because opening a FIFO can block before descriptor metadata is available.
