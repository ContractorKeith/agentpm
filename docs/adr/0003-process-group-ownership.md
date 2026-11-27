# Signal only process groups we provably still own

A process group is signaled only through a live `Child` whose unreaped leader reserves the PID and PGID. `waitid(WNOWAIT)` observes the leader's exit without reaping it, so the numeric identity cannot be reused until all group signals are done. A failed ownership probe never authorizes a signal.

- Darwin can hide a zombie's PGID even while WNOWAIT holds it, so a mismatched `getpgid` falls back to the WNOWAIT check.
- Child pipes are nonblocking and reads are bounded per tick, so an escaped pipe holder or a producer that never stops writing cannot stall the owner.
- The fake agent's escaped-pipe fixture forks only after `exec` has closed `spawn`'s error pipe, so the test exercises an escaped stdout holder and not a spawn failure.
- `finish` owns the only reap, even when protocol validation fails.
