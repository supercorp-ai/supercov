# Process-supervision contract v1

Supercov launches the already-working test command without runner-specific
semantic changes. On POSIX, the child starts a new process group and signals
target the group. Windows uses the strongest available equivalent and must
move to Job Objects before Windows binary GA.

Long-running commands are never silent. By default, every 60,000 ms stderr
receives a sanitized process-tree snapshot containing elapsed time, PID, PPID,
executable basename, state, and CPU time where available. It never prints
arguments, environment values, or full executable paths. A preloaded Node
descendant may additionally report counts from `process.getActiveResourcesInfo`.

`SUPERCOV_DIAGNOSTIC_INTERVAL_MS` changes that interval and must be a positive
integer. There is no default kill deadline. `SUPERCOV_COMMAND_TIMEOUT_MS` is an
explicit positive-integer deadline; on expiry Supercov logs the reason, sends
SIGTERM to the full child tree, escalates to SIGKILL after 5,000 ms, and exits
124. Invalid values fail before a child is spawned.

User SIGHUP/SIGINT/SIGTERM is forwarded cooperatively, records interrupted
state, and escalates if the child refuses to exit. Spawn errors and signals are
not reported as successful tests. Diagnostic/probe machinery must never keep a
completed child or the Supercov CLI alive.
