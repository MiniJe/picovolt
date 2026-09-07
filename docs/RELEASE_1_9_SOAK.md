# PicoVolt 1.9.0 release-candidate qualification ledger

This ledger records the candidate-scoped workflow shards and compatibility checks
required by [RELEASING.md](RELEASING.md) and [ROADMAP.md](../ROADMAP.md). All
timestamps are UTC. GitHub-hosted runs are bounded observations, not
uninterrupted multi-day execution.

## Status

- State: **release authorized; final candidate pipeline pending**
- Soak anchor: `2026-09-07T16:24:09Z`
- Anchor commit: `5f4165c13f0c92cedd018a696b43d17091f69ec4`
- Anchor event: merge of [PR #16](https://github.com/MiniJe/picovolt/pull/16)
  into `main`
- First nominal post-anchor scheduled soak: `2026-09-13T03:17Z`
- First nominal post-anchor scheduled benchmark: `2026-09-14T04:17Z`

On 2026-09-07, the project owner replaced the fixed 30-day wait with daily minor
and weekly major release windows and explicitly authorized the immediate 1.9.0
release. Candidate changes now invalidate only the evidence they can affect.
Documentation, evidence-log, and ancestry/version-only changes do not invalidate
runtime evidence. Runtime, format, migration, recovery, parser/decoder, binding,
or security changes require their affected gates to be rerun.

## Gate status

| Gate | Evidence so far | Remaining action |
| --- | --- | --- |
| Fixed elapsed wait | Superseded by owner decision on 2026-09-07 | No elapsed-time minimum; all technical gates still apply. |
| Golden migration corpus | Main CI run `34145609565` passed, including `cargo test --locked --all-targets` | Keep green on the final release-preparation candidate. |
| Six fuzz targets plus model/recovery | Corrective run `34142397153` passed on the exact runtime fix head | Runtime is unchanged by release metadata; rerun if the final candidate changes affected code. Scheduled observations remain supplementary. |
| Seven performance budgets | Manual main run `34144688065` passed all seven budgets on the same runtime tree | Runtime is unchanged by release metadata; retain the artifact and rerun if affected code changes. |
| Critical/high security findings | At `2026-09-07T16:29Z`, there were zero open Dependabot security alerts, secret-scanning alerts, or repository advisories; `cargo-audit` also passed | Recheck every source and record the final zero-open-critical/high decision. |
| Compatibility | First-party Rust, JS, Python, Go, C, CLI, and browser surfaces passed main CI; exact registry installs are post-publish gates | Run all clean-install starters against 1.9.0 before the GitHub Release is created and record any failure. |
| Stable release checklist | In progress | Before tagging, complete version parity and the full Rust, WASM, Go, Python, npm, CLI, and benchmark matrix. After tagging, verify registries and artifacts, run clean-install starters, and record 24-hour totals. |

## Evidence and reset history

| Started UTC | Commit | Workflow/event | Run | Result | Notes |
| --- | --- | --- | --- | --- | --- |
| 2026-09-04T22:29:40Z | `523c2e7878ac` | Stabilization, manual | [33925737340](https://github.com/MiniJe/picovolt/actions/runs/33925737340) | Failed | All six fuzz jobs hit the incompatible default fuzz target; model/recovery passed. Fixed by [PR #15](https://github.com/MiniJe/picovolt/pull/15). |
| 2026-09-04T22:36:33Z | `689d47e30e2e` | Stabilization, manual | [33926232939](https://github.com/MiniJe/picovolt/actions/runs/33926232939) | Failed | Infrastructure fix worked; only `decode_wasm` failed with an arithmetic-overflow panic. |
| 2026-09-06T03:28:20Z | `689d47e30e2e` | Stabilization, scheduled | [34009073411](https://github.com/MiniJe/picovolt/actions/runs/34009073411) | Failed | Reproduced the `decode_wasm` failure; the other five fuzz targets and model/recovery passed. |
| 2026-09-07T04:31:17Z | `689d47e30e2e` | Benchmarks, scheduled | [34083408182](https://github.com/MiniJe/picovolt/actions/runs/34083408182) | Passed | All seven budget checks passed; pre-anchor supporting evidence only. |
| 2026-09-07T16:15:07Z | `6c39c9c20191` | CI, PR | [34142371564](https://github.com/MiniJe/picovolt/actions/runs/34142371564) | Passed | Corrective PR CI passed all jobs, including migration tests, 32-bit WASM bounds coverage, bindings, starter policy, and audit. |
| 2026-09-07T16:15:25Z | `6c39c9c20191` | Stabilization, manual | [34142397153](https://github.com/MiniJe/picovolt/actions/runs/34142397153) | Passed | All six fuzz targets and model/recovery passed. Exact fix-head evidence, but completed before the anchor merge. |
| 2026-09-07T16:24:09Z | `5f4165c13f0c` | PR #16 merged to `main` | [PR #16](https://github.com/MiniJe/picovolt/pull/16) | Candidate reset | Decoder width and address arithmetic fix became the `main` candidate; affected evidence was rerun. |
| 2026-09-07T16:24:11Z | `5f4165c13f0c` | CI, push | [34143090723](https://github.com/MiniJe/picovolt/actions/runs/34143090723) | Passed | Post-merge `main` CI passed all applicable jobs. |
| 2026-09-07T16:44:51Z | `e08a9a2c6b1c` | PR #17 merged to `main` | [PR #17](https://github.com/MiniJe/picovolt/pull/17) | No reset | Forward-ported published 1.8.1 metadata and starter locks; runtime and format behavior are unchanged. |
| 2026-09-07T16:44:53Z | `e08a9a2c6b1c` | CI, push | [34144669409](https://github.com/MiniJe/picovolt/actions/runs/34144669409) | Passed | Main-at-the-time CI passed all applicable jobs, including golden migrations, 32-bit WASM bounds, bindings, starter policy, and audit. |
| 2026-09-07T16:45:07Z | `e08a9a2c6b1c` | Benchmarks, manual | [34144688065](https://github.com/MiniJe/picovolt/actions/runs/34144688065) | Passed | All seven budgets passed on the release runtime; [artifact 10027209060](https://github.com/MiniJe/picovolt/actions/runs/34144688065/artifacts/10027209060) retained through 2026-12-06. |
| 2026-09-07T17:02:27Z | `293d3f37e7bd` | CI, push | [34145609565](https://github.com/MiniJe/picovolt/actions/runs/34145609565) | Passed | Latest pre-release `main` CI passed all applicable jobs after the ledger was added. |
| 2026-09-07 | — | Release-policy decision | Owner directive | Authorized | Fixed elapsed soak was superseded by evidence-based qualification; immediate 1.9.0 publication authorized. |

## Weekly observations

Append one row after every Sunday stabilization run and Monday benchmark run.
Do not delete or replace failed rows; append the failure, its issue or PR, the
verifying rerun, and any resulting clock decision.

| Started UTC | Candidate SHA | Check | Run/evidence | Result | Notes or linked issue |
| --- | --- | --- | --- | --- | --- |
| 2026-09-07T16:45:07Z | `e08a9a2c6b1c` | Benchmarks, manual | [34144688065](https://github.com/MiniJe/picovolt/actions/runs/34144688065) | Passed | Current-runtime evidence; scheduled observations are supplementary. |
| — | — | Scheduled stabilization and benchmarks | — | Pending | First scheduled observations have not run; they do not impose an elapsed-time release delay. |

A counting workflow row must identify a `main` commit descended from the anchor.
Label branch runs, pre-anchor runs, cancelled runs, and infrastructure failures
accurately rather than counting them as clean soak observations. For matrix
runs, name any failed shard. For dedicated longer runs, also record retained
log and artifact identifiers.

## Compatibility observations

| Date UTC | Candidate artifact or SHA | Consumer and environment | Result | Evidence or issue |
| --- | --- | --- | --- | --- |
| 2026-09-07 | `293d3f37e7bd` | First-party Rust, JS, Python, Go, C, CLI, and browser matrix on Linux/Windows | Passed | [Main CI 34145609565](https://github.com/MiniJe/picovolt/actions/runs/34145609565); no external downstream trial is recorded. Exact registry starters remain mandatory post-publication. |

## Final release decision

Complete the remaining fields when the final candidate and registry gates finish.

- Final candidate SHA:
- Final stabilization run:
- Final benchmark run:
- Golden migration evidence:
- Downstream compatibility summary:
- Critical/high finding triage:
- Version-parity and full-matrix evidence:
- Decision and approver: Immediate publication authorized by the project owner on 2026-09-07, subject to the technical gates above.
- Stable tag:
