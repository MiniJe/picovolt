# PicoVolt 2.3 qualification ledger — publication pending

**Mandate:** PV-2.3-M-001. **Target:** 2.3.0. **PR:** [#33](https://github.com/MiniJe/picovolt/pull/33).
**Base:** `main` at `b13ab962aad21f003e1ae37a70b963876c9b72f4`.
**Branch:** `codex/2.3-persistent-retrieval-m001`.

This ledger distinguishes implemented code from verification and publication.
It is not authorization to merge, tag, upload a package or create a release.

## Implemented scope

Named full-text BM25 and exact cosine/squared-Euclidean vector catalog objects;
SQL create/drop and Rust catalog APIs; bounded binary CAS envelopes; conditional
format 8 and a golden fixture; transactional row/index maintenance; explicit
rebuild and introspection; optional named full-text/vector/hybrid retrieval;
filtered corpus statistics; historical/transformed-query fallback; pinned native
read-session retrieval. No ANN, external models, Hub change or new dependency.
See [the guide](PERSISTENT_RETRIEVAL_2_3.md) and [format specification](FORMAT.md).

## Evidence recorded before final candidate preparation

At source `30f834e64102076300d5b6f326da6facad4fb7be`, local offline Rust 1.86.0
(x86_64 Linux) successfully ran `cargo check --locked --all-features` and
`cargo test --locked --all-targets --all-features`. This includes seeded mutation
model, rollback, historical/filtered/hybrid differential tests, corruption,
format/migration, and abrupt-process publication boundaries. The later new C ABI
and encrypted-snapshot interface tests also passed locally. Python's maintained
ctypes suite including the new persistent lifecycle test passed (12 tests);
`go test ./...` including the new lifecycle test passed locally. Final-head
reruns and source identification belong in the closure record below.

Hosted [qualification run 35629304208](https://github.com/MiniJe/picovolt/actions/runs/35629304208)
on that PR revision passed formatting, strict all-feature Clippy, all-feature
all-target tests, doctests, WASM compilation, all six Rust 1.86 feature-isolation
jobs, seeded binary/DDL fuzz smoke and the benchmark acceptance job. The run's
aggregate status was FAILURE because actionlint found the pre-existing
`sha256sum *` option-parsing hazard in `release.yml`. The candidate corrects that
one command to `sha256sum -- *`; lint remains mandatory. The run is not recorded
as an overall green gate.

## Required final verification

Run the unchanged CI plus `persistent-retrieval.yml` on the final candidate and
record its exact head and workflow URLs. Required command coverage includes:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc
cargo +1.86.0 check --locked --lib
cargo check --locked --target wasm32-unknown-unknown --features wasm --lib
python scripts/check_registry_starters.py policy
python -m unittest discover -s scripts/tests -v
cargo audit
```

Also retain no-default/full-text/vector/combined/encryption/C-ABI feature checks,
Linux/Windows tests, ThreadSanitizer, Python tests, Go tests/vet, JS/WASM integration,
fuzz targets/smoke, format/migration tests, and same-run performance evidence.
The existing production/release workflows are not dispatched for this mandate.

## Performance and resource qualification

The hosted `persistent-retrieval-envelope-35629304208` artifact records source
revision, Rust/host provenance and machine-readable measurements for 1,000 and
10,000 documents, 128 dimensions. Its medium-corpus speed comparisons passed for
full text, vector and hybrid, including tenant-filtered subsets. Subsequent final
candidate artifacts supersede timing observations, not their provenance. The
benchmark asserts exact same-run differential results before measuring. No
unmeasured cross-machine speedup, production SLA or 50,000-document capacity is
claimed. Open performs source verification; affected index generations are
rewritten on commit; unreachable CAS bytes remain under existing retention.

## Compatibility and publication boundary

Legacy retrieval JSON, ordinary secondary DDL, v1–v7 fixtures and maintained ABI
ownership remain in scope. Native encryption still does not apply to WASM.
Current LICENSE and dependency terms are preserved; the 2.3 scope record does
not claim external legal review. No independent security audit or hardware
power-cut certification has occurred.

Final version surfaces, starter integrity evidence, final green CI and founder
review must be resolved before declaring the candidate ready. Registry clean
installs are post-publication gates and are not claimed for an unpublished
candidate. This mandate must stop before merge, tags and publication.

## Closure

Pending: final source SHA, complete CI disposition, measured comparison table,
remaining limitations, and READY_FOR_FOUNDER_REVIEW or precise NOT_READY reasons.
