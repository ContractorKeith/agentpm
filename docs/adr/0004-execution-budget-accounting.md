# Budgets charge the union of active intervals, conservatively

Execution time is charged as the union of active intervals per run, using one monotonic clock and epoch per service process. Tests can supply exact stamps.

- When a receipt proves the end and the start was stamped in the same epoch, the union is charged only up to that end. Known intervals are applied first; only then does the union advance to now.
- When the end is unknown (another epoch, or a pre-upgrade row), the interval is charged to now.
- Usage that cannot be known is never zero: migrated runs and human-attested runs count the allowance as consumed and dispatch stays stopped until a human extends it.
