# Independent RC3 follow-up prompt

You are a separate evaluator of PicoVolt 2.0.0-rc.3. Review a fresh checkout of
`adf527af35ca7fec6c300bdf24bda9810f0bf7f4` from
https://github.com/MiniJe/picovolt. Do not alter the candidate or the frozen RC2
review evidence. Record the source revision, toolchain and binary hashes.

This follow-up covers correctness, recovery, maintained interfaces, usability
and performance. Security review remains deferred and must be reported as such.
Do not treat functional checks as completion of that separate gate.

Independently retest the three reproduced RC2 defects: acknowledged sequence reuse
after missing log history; SQLite trigger body fragments executing during import;
and the documented registry starter runner's NameError. Check the fixes beyond
the original reproducer, including missing middle/tail/whole history, pruning,
rollback, process-crash publication, legacy upgrade, incomplete compound SQL,
comments and CASE expressions. Distinguish process crashes from power loss.

Assess automatic PRIMARY KEY/UNIQUE indexes and batch uniqueness semantics,
including numeric equality, NULL, updates, deletes, history, reopen and writable
legacy imports. Check Rust, C, Python, Go, JS/WASM, CLI and HTTP surfaces.

Choose realistic workloads before reading the implementation agent's benchmark
conclusions. Compare preserved RC2 and RC3 with version-verified SQLite and DuckDB
using identical data and constraints, suitable bulk APIs, recorded durability,
fresh serial processes, multiple repetitions and full correctness checks.
Report CPU, memory, storage, maintenance, startup costs, losses, timeouts and
host noise alongside improvements. Do not extrapolate universal superiority.

Produce a verdict, prioritized reproducible findings, raw measurements and an
updated release-gate matrix. Separate local/agent-generated demonstrations from
real external owner trials. Preserve absent provenance, incomplete external
migrations, package publication and deferred security review as open gates.
Do not publish, merge, tag, modify production data or contact third parties.
