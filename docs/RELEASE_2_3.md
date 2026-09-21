# PicoVolt 2.3 qualification ledger

**Mandate:** PV-2.3-M-001. **Candidate:** 2.3.0. **PR:** [#33](https://github.com/MiniJe/picovolt/pull/33).
**Base:** `main` at `b13ab962aad21f003e1ae37a70b963876c9b72f4`.
**Branch:** `codex/2.3-persistent-retrieval-m001`.
**Runtime-qualified source:** `489c734341ee35ecd1a23e1e4505532db403e03d`.

This record qualifies the 2.3.0 release candidate. The founder explicitly
authorized merge, tag and public publication on 2026-09-21 after final-head
qualification passed. The follow-up recording this ledger and benchmark archive
changes no runtime, bindings, dependency, version, or workflow source. Release
publication must still pass the tag-triggered registry, artifact, provenance and
clean-install gates before the GitHub Release is considered complete.

## Implemented scope and semantics

Named optional BM25 full-text and exact cosine/squared-Euclidean vector indexes;
SQL create/drop and Rust catalog/verify/rebuild APIs; bounded binary CAS storage;
conditional format 8 with a golden fixture; transactional mutation maintenance;
inspection; named full-text/vector/hybrid retrieval and pinned read sessions.
Legacy JSON and ordinary secondary-index SQL remain supported. Source rows stay
authoritative. Filtered BM25 recomputes corpus statistics over the selected IDs;
vector and hybrid use the same SELECT snapshot. Historical/transformed SELECTs
use the safe legacy fallback rather than an incompatible current index.

Open validates checksums, bounds, definitions, generations and reference state
rebuilt from source rows once, then retains validated index state. Corruption
fails open deterministically; no automatic repair conceals it. Dirty CAS envelopes
and catalog publication participate in the existing rollback/physical-journal
boundary. There is no independently published sidecar. See the
[guide](PERSISTENT_RETRIEVAL_2_3.md) and [wire format](FORMAT.md#format-8-persistent-retrieval).

## Verification evidence

Runtime qualification on 2026-09-21:

- [Persistent retrieval qualification 35632837786](https://github.com/MiniJe/picovolt/actions/runs/35632837786): all nine jobs passed, including strict all-feature validation, Python lifecycle tests, workflow lint, six feature-isolation jobs on Rust 1.86, both seeded decoder/DDL fuzz targets, and measured benchmark acceptance.
- [CI 35632837703](https://github.com/MiniJe/picovolt/actions/runs/35632837703): the required unchanged general matrix for the same candidate, including Linux/Windows, MSRV, native C ABI, Go 1.26/1.27 test/vet, JS/WASM integration, ThreadSanitizer, audit, fuzz compilation, DCO, and starter policy. Final status must be read together with the PR's current-head checks.
- Local offline x86_64 Linux, Rust 1.86.0: **385 Rust all-target/all-feature tests and 2 doctests passed**, strict Clippy and formatting passed; **12 Python tests and 32 policy tests passed**; Go tests/vet passed. The local native policy suite was rerun after building the CLI with `--all-features`; the earlier capi-only executable did not expose the required data-tools subcommand.

Executed command coverage:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc
# Local installed compiler was exactly 1.86.0; hosted MSRV also checks all features.
cargo check --locked --lib
cargo +1.86.0 check --locked --all-features
cargo check --locked --target wasm32-unknown-unknown --features wasm --lib
cargo build --locked --all-features --bins
cargo build --locked --features capi
PYTHONPATH=bindings/python PICOVOLT_LIB="$PWD/target/debug/libpicovolt.so" \
  python -m pytest -q bindings/python/test_*.py
python -m unittest discover -s scripts/tests -v
python scripts/check_registry_starters.py policy
python scripts/check_public_release.py
# With the native library on the documented CGO/linker/runtime paths:
(cd bindings/go && go test ./... && go vet ./...)
go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.7
cargo audit
```

The dedicated MSRV matrix runs `cargo test --locked --no-default-features
--features "$FEATURES" --test persistent_retrieval` for empty, full-text,
vector-search, combined, encryption+combined, and capi+combined feature sets.
The ordinary default-feature suite and actual WASM bindings are separate gates.
Existing JS integration discovery includes the new named-index lifecycle test.
Rust interface coverage exercises C allocation/free ownership and encrypted
snapshot persistence. CLI inspection of the format-8 fixture reports both healthy
indexes; create/retrieve/bake/import and prior v1–v7 fixtures run in the suite.

Differential/generated-corpus tests cover full-text, both exact vector metrics,
tenant-filtered and hybrid ranking, Unicode/NULL/empty/long tokens, invalid IDs and
vectors, mutation/rollback, index budgets, source mismatch and historical fallback.
A seeded 160-transaction mutation model compares rows and retrieval after each
commit/rollback and repeated reopen. Pinned readers stay stable across a later
writer. Child-process tests exercise logged/unlogged abrupt exits, plus test-only
fault points before maintenance, during encoding, after CAS writes, before
manifest publication and after manifest publication. Corruption tests mutate
headers, bounds, lengths, checksums, IDs, dimensions, term references, trailing
bytes and descriptor/source associations. These are qualification tests, not a
formal proof or hardware power-cut certification.

Both new fuzz targets build and receive seeded 30-second libFuzzer smoke runs.
Checksummed malformed bodies reach the binary decoder, not just checksum failure.
This is bounded smoke coverage, not sustained or independent security auditing.
No dependency was added and the mandatory cargo-audit gate passed.

## Same-run benchmark evidence

The exact downloaded GitHub artifact is retained at
[`benchmarks/persistent-retrieval-2.3-489c734.zip`](../benchmarks/persistent-retrieval-2.3-489c734.zip).
It contains `persistent-retrieval.json`, `revision.txt`, `host.txt` and
`toolchain.txt`, without repository credentials or build binaries.

- Run: `35632837786`; artifact ID: `10655261355`.
- ZIP SHA-256: `cb20312d69f8dc54b3d1a5a72996d0efa311b9a5196c12280818f16b67efd96a`.
- JSON SHA-256: `7091fca4990ad40554a011cd71314ee88351c17a7b8b2de154574212fab6595e`.
- Recorded test revision: `18427e8fde5e9b9f28cf249a1ee07d570a2cc488`, GitHub's synthetic PR test merge for head `489c734` and the unchanged base; **not a merge to main**.
- Host: Linux x86_64, AMD EPYC 7763, four exposed CPUs; Rust 1.86.0, release profile.
- Command: `PV_BENCH_REVISION="$(git rev-parse HEAD)" cargo run --release --locked --example persistent_retrieval_envelope -- --bench`.
- Corpora: 1,000 and 10,000 documents; 128-dimensional vectors; 15 repetitions/query. The tenant subset selects one of eight tenants. Equal results are asserted before timing.

### Medium corpus, mean milliseconds per query

| Query | Ephemeral rebuild | Persistent | Observed ratio |
| --- | ---: | ---: | ---: |
| Full text, all documents | 130.884 | 10.009 | 13.08x |
| Full text, tenant filter | 16.325 | 4.425 | 3.69x |
| Exact vector, all documents | 40.881 | 11.729 | 3.49x |
| Exact vector, tenant filter | 7.144 | 3.960 | 1.80x |
| Hybrid, all documents | 160.125 | 13.952 | 11.48x |
| Hybrid, tenant filter | 20.034 | 5.392 | 3.72x |

These are observations from one host/run, not universal speedups or SLAs.
The path retains SELECT materialization. Snapshot admission is excluded from
repeated queries on an already admitted database/reader.

For 10,000 documents: text creation **162.907 ms**; vector creation **74.595 ms**;
verified image import **418.648 ms** versus **7.698 ms** unindexed; indexed workspace
reopen **520.411 ms**. Baked size grows from **1,802,398** to **9,512,426 bytes**
(**7,710,028 bytes** overhead). Initial workspace size grows from **1,686,743** to
**9,396,736 bytes**, then **11,905,843 bytes** after one text update.

Single observed indexed mutation costs were delete **39.025 ms**, insert
**43.371 ms**, text update **42.031 ms** and vector update **30.740 ms**, versus
unindexed **2.590/0.027/19.604/19.371 ms** respectively. These are not percentiles.
The measured process used **196,732 KiB RSS**, **218,800 KiB peak RSS**, including
multiple live database copies; these figures are not per-index memory attribution.
Full small-corpus and resource measurements remain in the retained JSON.

## Candidate versions and installation boundary

Cargo, Python, C-header notices and maintained starter pins are prepared for
2.3.0. Historical release evidence and published 2.2 README installation examples
remain intact. Starter policy verifies registry-only references, not availability
of an unpublished version. No registry clean-install result is claimed.

The isolated [candidate build 35632009358](https://github.com/MiniJe/picovolt/actions/runs/35632009358)
built npm output with Rust 1.98.1, wasm-pack 0.15.0 and Node 24.20.0 without
publication. Both npm starter locks use its measured archive integrity:

```text
sha512-Fe3Y3cGeNCPDMc5ubdjgjKDlOnNAOCMTcY6ow68PdYIMlRkvwaqTqIqCkenPpDuC84AotmWhXoYIi91nnUQ7hw==
```

The Go 2.3 module hash is source-derived (the same calculation was checked against
the existing 2.2 sum), not fetched from a nonexistent published tag. At an authorized
release, registry artifacts and clean installations must be verified again.

## Diagnosed failures and closure boundary

Earlier retrieval qualification failed actionlint only on the pre-existing
`sha256sum *` command in `release.yml`; the narrow fix is `sha256sum -- *`.
No release workflow was run and no check was removed or relaxed.

Run `35632009358` applied the exact hash-verified candidate patch, passed its build,
385-test Rust suite and policy checks, then failed its Git push because the runner
app cannot update workflow files. The verified commit objects were recovered and
advanced by the already authorized GitHub connector, without force-pushing.
Temporary transport files, source-writing scripts and the write-enabled workbench
were removed before candidate qualification. This was a delivery-permission issue,
not an architectural or data-integrity blocker.

Remaining limits are intentional and documented: exact-only vectors; no
phrase/fuzzy/stemming search; 10,000 documents/index; full source verification on
open; materialized filtering; whole affected index encoding at commit; retained
unreachable CAS generations; historical/transformed-query fallback; native-only
encryption. Use a retained read transaction to amortize shared snapshot admission.
No 50,000–100,000-document qualification, independent audit, production-at-scale
claim, or changed license terms is asserted.

The implementation passed founder review and publication was explicitly
authorized on 2026-09-21. Tag-triggered release automation remains responsible
for immutable registry publication, clean-install smoke tests, native artifacts,
SBOMs, checksums, attestations and the final GitHub Release. Hub deployment
remains out of scope.
