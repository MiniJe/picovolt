# PicoVolt 1.9.0 release-candidate soak

This ledger records the bounded workflow shards and downstream checks used for
the 1.9.0 release-candidate soak required by [RELEASING.md](RELEASING.md) and
[ROADMAP.md](../ROADMAP.md). All timestamps are UTC. GitHub-hosted runs are
bounded observations, not uninterrupted multi-day execution.

## Status

- State: **in progress; stable tag blocked**
- Soak anchor: `2026-09-07T16:24:09Z`
- Anchor commit: `5f4165c13f0c92cedd018a696b43d17091f69ec4`
- Anchor event: merge of [PR #16](https://github.com/MiniJe/picovolt/pull/16)
  into `main`
- Earliest gate-open time: `2026-10-07T16:24:09Z`
- Date-only release target: **2026-10-08 or later**
- First nominal post-anchor scheduled soak: `2026-09-13T03:17Z`
- First nominal post-anchor scheduled benchmark: `2026-09-14T04:17Z`

The clock restarts when a soak-discovered defect requires a runtime,
file-format, migration, recovery, parser/decoder, binding, or security fix.
Documentation, evidence-log, and ancestry/version-only changes do not restart
it. For any other post-anchor change, record an explicit reset or no-reset
decision here. This is a conservative operating rule for this ledger; the
existing release policy requires 30 days but does not itself define reset
semantics.

## Gate status

| Gate | Evidence so far | Remaining action |
| --- | --- | --- |
| 30 elapsed days | Anchor above | Do not tag before the gate-open time; other gates may delay release further. |
| Golden migration corpus | Current-main CI run `34144669409` passed, including `cargo test --locked --all-targets` | Keep green on every candidate change and record a final `main` run after the 30-day boundary. |
| Six fuzz targets plus model/recovery | Corrective run `34142397153` passed on the exact fix head | Record every post-anchor scheduled run on `main`, then run one final shard after the 30-day boundary. |
| Seven performance budgets | Manual current-main run `34144688065` passed all seven budgets | Record post-anchor scheduled benchmark runs and a final current-candidate run after the 30-day boundary. |
| Critical/high security findings | At `2026-09-07T16:29Z`, there were zero open Dependabot security alerts, secret-scanning alerts, or repository advisories; `cargo-audit` also passed | Recheck every source and record the final zero-open-critical/high decision. |
| Downstream compatibility | No result recorded yet | Record candidate artifact/commit, consumer, platform/runtime, result, and issue link for each trial. |
| Stable release checklist | Not yet due | Before tagging, complete version parity and the full Rust, WASM, Go, Python, npm, CLI, and benchmark matrix. After tagging, verify registries and artifacts, run clean-install starters, and record 24-hour totals. |

## Evidence and reset history

| Started UTC | Commit | Workflow/event | Run | Result | Notes |
| --- | --- | --- | --- | --- | --- |
| 2026-09-04T22:29:40Z | `523c2e7878ac` | Stabilization, manual | [33925737340](https://github.com/MiniJe/picovolt/actions/runs/33925737340) | Failed | All six fuzz jobs hit the incompatible default fuzz target; model/recovery passed. Fixed by [PR #15](https://github.com/MiniJe/picovolt/pull/15). |
| 2026-09-04T22:36:33Z | `689d47e30e2e` | Stabilization, manual | [33926232939](https://github.com/MiniJe/picovolt/actions/runs/33926232939) | Failed | Infrastructure fix worked; only `decode_wasm` failed with an arithmetic-overflow panic. |
| 2026-09-06T03:28:20Z | `689d47e30e2e` | Stabilization, scheduled | [34009073411](https://github.com/MiniJe/picovolt/actions/runs/34009073411) | Failed | Reproduced the `decode_wasm` failure; the other five fuzz targets and model/recovery passed. |
| 2026-09-07T04:31:17Z | `689d47e30e2e` | Benchmarks, scheduled | [34083408182](https://github.com/MiniJe/picovolt/actions/runs/34083408182) | Passed | All seven budget checks passed; pre-anchor supporting evidence only. |
| 2026-09-07T16:15:07Z | `6c39c9c20191` | CI, PR | [34142371564](https://github.com/MiniJe/picovolt/actions/runs/34142371564) | Passed | Corrective PR CI passed all jobs, including migration tests, 32-bit WASM bounds coverage, bindings, starter policy, and audit. |
| 2026-09-07T16:15:25Z | `6c39c9c20191` | Stabilization, manual | [34142397153](https://github.com/MiniJe/picovolt/actions/runs/34142397153) | Passed | All six fuzz targets and model/recovery passed. Exact fix-head evidence, but completed before the anchor merge. |
| 2026-09-07T16:24:09Z | `5f4165c13f0c` | PR #16 merged to `main` | [PR #16](https://github.com/MiniJe/picovolt/pull/16) | Soak reset | Decoder width and address arithmetic fix became the `main` candidate; 30-day clock starts here. |
| 2026-09-07T16:24:11Z | `5f4165c13f0c` | CI, push | [34143090723](https://github.com/MiniJe/picovolt/actions/runs/34143090723) | Passed | Post-merge `main` CI passed all applicable jobs. |
| 2026-09-07T16:44:51Z | `e08a9a2c6b1c` | PR #17 merged to `main` | [PR #17](https://github.com/MiniJe/picovolt/pull/17) | No reset | Forward-ported published 1.8.1 metadata and starter locks; runtime and format behavior are unchanged. |
| 2026-09-07T16:44:53Z | `e08a9a2c6b1c` | CI, push | [34144669409](https://github.com/MiniJe/picovolt/actions/runs/34144669409) | Passed | Current-main CI passed all applicable jobs, including golden migrations, 32-bit WASM bounds, bindings, starter policy, and audit. |
| 2026-09-07T16:45:07Z | `e08a9a2c6b1c` | Benchmarks, manual | [34144688065](https://github.com/MiniJe/picovolt/actions/runs/34144688065) | Passed | All seven budgets passed on current `main`; [artifact 10027209060](https://github.com/MiniJe/picovolt/actions/runs/34144688065/artifacts/10027209060) retained through 2026-12-06. |

## Weekly observations

Append one row after every Sunday stabilization run and Monday benchmark run.
Do not delete or replace failed rows; append the failure, its issue or PR, the
verifying rerun, and any resulting clock decision.

| Started UTC | Candidate SHA | Check | Run/evidence | Result | Notes or linked issue |
| --- | --- | --- | --- | --- | --- |
| 2026-09-07T16:45:07Z | `e08a9a2c6b1c` | Benchmarks, manual | [34144688065](https://github.com/MiniJe/picovolt/actions/runs/34144688065) | Passed | Supporting post-anchor current-main evidence; does not replace the September 14 scheduled observation. |
| — | — | Scheduled stabilization and benchmarks | — | Pending | First post-anchor scheduled observations have not run yet. |

A counting workflow row must identify a `main` commit descended from the anchor.
Label branch runs, pre-anchor runs, cancelled runs, and infrastructure failures
accurately rather than counting them as clean soak observations. For matrix
runs, name any failed shard. For dedicated longer runs, also record retained
log and artifact identifiers.

## Downstream compatibility observations

| Date UTC | Candidate artifact or SHA | Consumer and environment | Result | Evidence or issue |
| --- | --- | --- | --- | --- |
| — | — | — | Pending | No downstream result has been recorded. |

## Final release decision

Complete this section only after the gate-open timestamp.

- Final candidate SHA:
- Final stabilization run:
- Final benchmark run:
- Golden migration evidence:
- Downstream compatibility summary:
- Critical/high finding triage:
- Version-parity and full-matrix evidence:
- Decision and approver:
- Stable tag:
