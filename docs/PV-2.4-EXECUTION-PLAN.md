# PicoVolt 2.4 executable plan

**Mandate:** PV-2.4-EXECUTION-001. **Decision:** propose one outcome for founder
review: **reduce and qualify persistent-retrieval lifecycle costs on the existing
single-writer engine**. **Exit:** `READY_FOR_FOUNDER_REVIEW`.

This is the adjudicated implementation proposal, not authorization to publish or
a claim that 2.4 is implemented. Implementation mandates below become executable
after outcome approval. No shipping date, changed license, or Hub deployment is
authorized here. Release eligibility follows evidence, not the release windows
in [RELEASING.md](RELEASING.md).

## 1. Evidence and authority

Repository and GitHub state inspected on **2026-09-29, approximately 20:57 UTC**:

| Evidence | Verified revision/status |
| --- | --- |
| Released engine | `v2.3.0` -> `089090bab62e47d5fde046d608094ebc7e8532dd`; annotated tag `529677f6feb6b4e23d7fbc2cf5728b215861fd49` |
| GitHub `main` and this worktree's starting HEAD | `d72133ac0c61d852efb964beb988111f7757ac97`; clean dedicated worktree on `codex/a16-pv-24-synthesis` |
| Publication | [2.3 release](https://github.com/MiniJe/picovolt/releases/tag/v2.3.0), published `2026-09-22T13:21:57Z`; [verification record](RELEASE_2_3_VERIFICATION.json) closes registries, native artifacts, checksums, SBOMs and provenance |
| [PR #42](https://github.com/MiniJe/picovolt/pull/42) | OPEN, unmerged; audit head `ff9e1812722e259823202f861587a2fd6e057ae2`; displayed CI/retrieval checks successful at inspection |
| [PR #43](https://github.com/MiniJe/picovolt/pull/43) | OPEN, unmerged; independent HTTP fix head `6b56227292360b7878d30e934ec161192955d4f1`; no patch release/tag/publication claimed |
| [PR #40](https://github.com/MiniJe/picovolt/pull/40) | OPEN MSRV guard for Arrow 60; proposed guard is not current-main policy |

Authority order: released bytes and executable source/tests; exact qualification
and publication ledgers; bounded audit results with their methodology; capability
guides; roadmap intentions. Earlier proposals do not establish delivered scope.
No tracked 2.4 execution proposal was found in the inspected tree or commit-title
search across fetched/local refs. Candidate ordering in ROADMAP/HUB_ROADMAP is
treated as input, not an engine release commitment.

Read alongside this plan: [ROADMAP](../ROADMAP.md), [CHANGELOG](../CHANGELOG.md),
[SECURITY](../SECURITY.md), [BENCHMARKS](../BENCHMARKS.md),
[Hub roadmap](HUB_ROADMAP.md), [1.9 qualification](RELEASE_1_9_SOAK.md),
[2.0 qualification](RELEASE_2_0.md), [2.0 verification](RELEASE_2_0_VERIFICATION.json),
[2.1 qualification](RELEASE_2_1.md), [2.2 qualification](RELEASE_2_2.md),
[2.2 verification](RELEASE_2_2_VERIFICATION.json), [2.3 qualification](RELEASE_2_3.md)
and its verification record. The unmerged
[PV-2.3-HARDENING-REPORT.md](https://github.com/MiniJe/picovolt/blob/ff9e1812722e259823202f861587a2fd6e057ae2/docs/PV-2.3-HARDENING-REPORT.md)
is pinned here so it remains readable without pretending it exists on main.

### Contradictions resolved

| Statement in older material | Adjudication against release/code evidence |
| --- | --- |
| SECURITY says 2.0 is latest and there is no at-rest encryption | 2.3.0 is publicly released; native opt-in encrypted snapshots/vaults shipped in 2.2. Plaintext workspaces, WASM and browser storage are not transparently encrypted. Update security wording during M001. |
| SECURITY calls the HTTP server bounded | Body/engine-queue limits do not bound pre-authentication request parsing. #42 confirms a HIGH availability defect in released 2.3/main; #43 separately proposes the ingress fix. |
| SECURITY/release prose says corruption "fails open" | Here that means **opening returns an error**. It does not mean permissive security fallback; malformed/stale indexes cannot answer queries. Use unambiguous wording. |
| CONCURRENCY says first shared write advances to format 6, and live encryption is absent | That paragraph predates the format-7 sequence anchor. Logged writes use at least 7; retrieval requires 8. Native standalone vaults exist, but shared page-journal storage is not transparently encrypted. FORMAT, current source and 2.3 guide control. |
| BENCHMARKS says a WAL is future work and quotes 1.x throughput | Shared/logged filesystem handles already have a physical incremental journal/change stream. Unlogged Database retains rollback images. Historical Fast-mode figures are not synced 2.3 performance. |
| Hub roadmap presents retrieval/encryption as future/private, with per-call indexes | Those dated sections describe old engine/pilot states. Public 2.3 has persistent retrieval and native encryption. The document does not prove a current deployed Hub runtime upgrade. No production access occurred. |
| ROADMAP/CHANGELOG date 2.3 as September 21 | Runtime qualification was September 21; immutable public publication was September 22. Retain the distinction. |
| Stable-format promise implies old binaries read future files | Backward read means newer qualified engines read earlier supported files. Older binaries reject newer format requirements. Conditional format 8 and logged format 7 are explicit examples. |
| An audit or green CI closes the deferred external gates | #42 is a bounded independent agent hardening review, not an independent specialist security/cryptographic audit, real owner migration acceptance or hardware power-cut certification. |

Main differs from the released tag in publication records/workflows, dependency
automation/MSRV protection and a vault-open lock-release fix with regression
coverage (`58634d9`). It does not contain #42 or #43. Qualify 2.4 against both
the immutable release and the eventual approved integration base; never move the
2.3 tag or attribute main-only fixes to already published packages.

## 2. Verified current capability baseline

| Area | Delivered capability and boundary |
| --- | --- |
| Storage/SQL | Page-backed dev workspaces, baked owned read-only mappings, MVCC history, practical SQL subset, secondary indexes, prepared statements, atomic batches, explicit migration/compaction and optional data tools. Ordinary indexes are decoded in memory; SELECT results can materialize. Not a SQLite/PostgreSQL replacement. |
| Concurrency | Native Rust `SharedDatabase` coordinates FIFO single-writer admission and immutable independent readers. Defaults: 64 waiting operations, 16 readers, 256 MiB/image. Snapshot admission copies a full image and verifies retrieval; existing readers continue through later writes. New reads wait behind an active write. Independent coordinators/raw handles are not cache-coherent. |
| Recovery | Shared filesystem transactions default to the incremental page journal; sequential Database can opt into the same log. Unlogged transactions copy/sync a rollback workspace; in-memory transactions snapshot an image. Process-crash recovery is tested. Directory sync/platform restrictions remain; no blanket power-loss certification. |
| Change stream | Checksummed schema-1 physical commits, final page images/new CAS blobs/manifest, sequence anchor, exclusive cursors, explicit prune/acknowledgement, checkpoint export and host-owned ChangeSink. Defaults: 64 MiB transaction journal, 256 MiB/4096 retained commits. Missing history fails; expired cursor needs a verified new base. This is a foundation, not a supported replication service or logical CDC API. |
| Retrieval | Optional named BM25 and exhaustive exact cosine/L2-squared indexes, hybrid reciprocal-rank fusion, SQL DDL, inspection/verify/rebuild and JSON interfaces. Authorized SELECT defines the corpus; filtered BM25 recomputes corpus statistics. Historical/transformed selections use the supported snapshot fallback. IDs remain decimal strings over JSON. |
| Retrieval persistence | Format-8 descriptors and immutable CAS envelopes commit with source rows. Open structurally validates and rebuilds a source reference per index, then compares canonical state. Corruption rejects open; rebuild requires an already healthy writable database. Incremental row maintenance already exists; dirty commit encoding still rewrites whole affected envelopes. |
| Encryption | Native opt-in `PVENC-1`, XChaCha20-Poly1305 and fixed-profile Argon2id; bounded snapshot/vault transactions, rotation and verified backup/restore. Vaults copy/bake/encrypt full RAM images, capped at 256 MiB; single writer. No browser crypto, transparent encrypted journals, automatic key recovery or cross-reopen anti-rollback guarantee. |
| Maintained surfaces | Rust; C ABI; Python ctypes/DB-API; Go direct/database/sql v2; raw JS/WASM and SQLite-style adapter; CLI; optional HTTP. Shared coordination is native Rust, not universal binding thread safety. Raw C unique-pointer/lifetime rules remain unsafe contracts. Native release targets and exact installs are in the 2.3 verification ledger. |
| Browser/edge | JS byte import/export, OPFS whole-image save/load and module-worker RPC. PersistentDb exposes retrieval; current worker RPC has no retrieval method. Async worker events can overlap around await; serialize/qualify this before claiming ordered storage operations. No multi-tab writer coherence. RangeReader/open_streamed is an existing host-supplied read path, outside SharedDatabase; not a managed object-store backend. |

Source anchors: [db.rs](../src/db.rs), [persistent integration](../src/db/persistent.rs),
[envelope/catalog](../src/persistent.rs), [concurrent.rs](../src/concurrent.rs),
[journal.rs](../src/journal.rs), [encryption.rs](../src/encryption.rs),
[browser.js](../bindings/js/browser.js), [worker.js](../bindings/js/worker.js),
[format](FORMAT.md), [concurrency](CONCURRENCY.md),
[retrieval guide](PERSISTENT_RETRIEVAL_2_3.md) and [encryption guide](ENCRYPTION_2_2.md).

Retrieval admission remains: **16 indexes**, **10,000 documents/index and selected
IDs**, **100,000 retained row versions/indexed table**, **32 MiB/envelope**,
**64 MiB active payloads**, **8 MiB text source**, **16 MiB vector source**,
**4,194,304 vector scalars**, **1,048,576 document-term entries**. Dimensions are
1–4096 subject to the scalar cap. Requests are 128 KiB; SELECT materialization
is 16 MiB; returned hits are at most 100. These overlapping limits can reject
a workload below 10,000 documents. They are not total RSS, total disk or hard
wall-clock limits. Full limits remain in the retrieval guide.

## 3. Debt and release risks

| Risk/gate | Status and disposition for 2.4 |
| --- | --- |
| Pre-auth HTTP memory/thread exhaustion | Confirmed HIGH in #42. External implementation owner is #43; this plan does not duplicate it. Gate on approved fix integration and exact-candidate unauthenticated network regressions before any candidate retaining the server can qualify. An open PR or edge-policy suggestion alone is not closure. |
| Independent review of format/recovery/FFI/network, plus crypto integration | Deferred in 2.0/2.2 and still not closed by 2.3 or #42. M005 prepares/rehearses the scope; a reviewer independent of implementation must assess final changed paths. Absent specialist availability is `BLOCKED_EXTERNAL`, not silently waived by historical authorization. |
| Real legacy migrations and three external application RC trials | 2.0 founder-authorized deferrals persist. Golden/generated cases are internal evidence. Seek owner provenance, verified backups/restores and written acceptance under M006; absent participants are `BLOCKED_EXTERNAL`. Prior release authorization does not pre-authorize another deferral. |
| Recovery edge cases | #42's ENOSPC/permission probes and process exits are bounded evidence. Secret-file metadata/open race and resumable-bake parent-directory sync are hardening risks, not demonstrated exploits/data loss. Triage under M005; do not conflate these with the confirmed HTTP defect or certify hardware durability. |
| Write amplification and startup | Whole affected envelopes, full source verification, snapshots and unlogged whole-workspace backups add distinct costs. Profile separately; optimize CPU/allocation without dropping verification. Explicit log use is already available and changes the rollback cost model. |
| Retained CAS/disk growth | Older generations and row-version blobs accumulate; log pruning/drop-index/page compaction do not provide CAS reclamation. `CasStore::pack` packs all catalogued blobs, so ordinary bake/migrate is not a GC operation. Host disk quotas and headroom remain required. Online/offline CAS remapping is excluded from 2.4. |
| MSRV drift | Cargo declares Rust 1.86; locked all-feature MSRV and feature-isolation CI enforce it. Main's wat automation guard excludes >=1.259; #40's Arrow-60/Rust-1.88 guard is still open. Broad manifest semver ranges can select newer-MSRV releases without the repository lock. M001 must test downstream resolution, not just our lockfile. |
| FFI/browser maintenance | Preserve allocation/free, error, closed-handle and single-owner contracts. Mocked OPFS/Node tests cannot establish actual browser interrupted-save durability. M004 needs real browser evidence and honest per-platform limits. No new binding family without adopter, CI and owner. |
| Release/legal evidence | One version across maintained distributions, checked license notice, checksums/SBOM/provenance and clean registry installs remain mandatory. No independent legal review is recorded. Changed terms or required human authorization are external gates; this plan proposes neither. |

## 4. Candidate adjudication and chosen outcome

| Coherent candidate | User value/evidence | Dependencies, compatibility/security cost, maintenance | Decision |
| --- | --- | --- | --- |
| Persistent retrieval lifecycle qualification and cost reduction | Improves already shipped catalog/docs/search workflows; measured open, mutation, snapshot and filesystem costs exist. Preserves exact results and current limits. | Existing journal, format 8 and bindings suffice. Main work is profiling, removing redundant verification/encoding work, qualifying the logged path and browser operations. Low format/API cost; mandatory recovery/security review. | **Choose for 2.4.** |
| One-way change-stream replica | Useful read distribution and recovery, but no named adopter/acceptance corpus here. | Needs verified-base/cursor atomic publication, duplicate/gap/restart/expiry handling, identity/version checks, transport authorization, retention policy and a maintained client. Physical changes are format-coupled. Adds security and platform surface before existing lifecycle cost is qualified. | Defer until the chosen foundation and adopter exist. |
| Larger/ANN and richer-language retrieval | Could help larger search workloads; 50k/100k exceeds current admission and ANN recall needs a real corpus. | New ranking semantics, recall budgets, memory/format structures, parameter tuning and more costly maintenance. No evidence that this beats lifecycle work for existing users. | Defer; do not raise caps or change BM25/tokenization in 2.4. |
| Browser-first remote pager or immutable object-store engine | Useful if whole-file delivery actually blocks an edge adopter. | Range consistency/cache/ETag/interruption semantics, bounded memory and platform maintenance needed. Hub already has a separate whole-file client path; hosted control-plane demand is not an engine dependency. | Defer managed pager work; qualify current OPFS/WASM path. |

**2.4 outcome:** applications can use the current bounded, persistent retrieval
with a measured lifecycle envelope, cheaper verified-open/encoding work, a
qualified opt-in logged writer recipe across maintained native interfaces and
ordered browser-worker retrieval/save operations. Exact results, source
verification and recovery semantics survive. A successful proposal requires
measured improvements below, not only renamed documentation.

Explicit non-goals: format 9; ANN/embeddings; phrase/fuzzy/stemming search;
50k/100k retrieval; logical CDC or automatic replication; bidirectional/offline
sync; consensus/multiple writers; zero-copy snapshots/shared encrypted pager;
automatic CAS GC/history deletion; SQL compatibility expansion; new language
adapters; encrypted OPFS; hosted accounts/telemetry; any Hub deployment. Do not
replace the unlogged recovery protocol or auto-enable logging in legacy handles.

## 5. Format, API and MSRV contract

- **Format:** preserve page/record, journal schema 1, format-7 sequence anchor,
  conditional format-8 descriptors and envelope v1 byte semantics. 2.4 must read
  every checked-in supported historical fixture and 2.3 images. 2.4-created
  supported images must reopen in the immutable 2.3 reader with equal full-history
  results. Open alone must not upgrade index-less files. Explicit migration may
  still stamp latest (8); index drop does not lower an existing format floor.
- **API:** preserve SQL, JSON requests/results, error categories and C symbols/
  ownership. Add the missing worker `retrieve` RPC and serialize operations without
  changing message IDs or existing methods. New diagnostics, if needed, use an
  explicitly versioned opt-in report; do not add fields to strict stored
  descriptors or break Rust public struct literals/exhaustive matches. Keep
  cancellation and outcome-unknown behavior honest. Maintain Rust shared handles
  and sequential native bindings as different concurrency contracts.
- **MSRV:** retain **Rust 1.86.0 throughout 2.4**. Keep stable code free of nightly
  requirements; fuzz/TSan use explicit nightly independently. Qualify default,
  no-default/isolated features, all-features including data-tools, C ABI and WASM.
  Guard incompatible dependencies using evidence, and qualify a clean downstream
  Rust-1.86 resolution without relying solely on the repository lock. Any
  unavoidable increase requires a separately reviewed policy revision; never
  weaken or rename the 1.86 gate to make a dependency update green.

Migration is a rehearsal even without a new format: quiesce independent writers;
retain an exact backup/image plus the **complete workspace and .pv-log** where
applicable; run `pv migrate --dry-run` and no-clobber migration; compare full MVCC
history and retrieval before/after; restore and query the old backup. A baked
image alone is not the old change history. Enabling the existing log is explicit,
requires compatible readers and disk budget, and cannot be undone by deleting
the log/anchor. Crypto restore retains required old keys; never erase sources
or recovery evidence to demonstrate a successful migration.

## 6. Benchmark and acceptance evidence

Two different existing observations motivate this choice; neither is an SLA:

| Source/method | Observation and correct interpretation |
| --- | --- |
| 2.3 qualification, 10k/128D, 15 repeated queries on one Linux host | Persistent full-text/vector/hybrid means 10.009/11.729/13.952 ms vs ephemeral 130.884/40.881/160.125. Verified import 418.648 ms vs unindexed 7.698; indexed dev reopen 520.411 ms. Query benefit excludes fresh snapshot admission. |
| #42, 10k/3D, one optimized Docker Desktop observation, unlogged workspace | Text/vector creation 46.11/49.38 s; insert/update/delete/commit about 94–96 s; recovery 65.10 s. Backup involved roughly 10,004 files and 43 MiB physical storage. This probes many-small-file rollback costs, not a proven retrieval CPU defect or a universal native-fs latency. |

Do not average these workloads or attribute their difference solely to indexes.
The #42 core 100k/1m results use an in-memory source then bake/reopen, without
persistent retrieval. They are not evidence for 100k indexed retrieval or synced
ingestion. Retain the
[release benchmark artifact](../benchmarks/persistent-retrieval-2.3-489c734.zip),
[audit retrieval JSON](https://github.com/MiniJe/picovolt/blob/ff9e1812722e259823202f861587a2fd6e057ae2/benchmarks/pv-2.3-audit-2026-09-29.json)
and [audit core JSON](https://github.com/MiniJe/picovolt/blob/ff9e1812722e259823202f861587a2fd6e057ae2/benchmarks/pv-2.3-audit-core-2026-09-29.json)
as separately attributed evidence.

M001 must freeze a same-host comparison against released 2.3 and the integration
base, with CPU/OS/filesystem/storage, memory, compiler, feature flags, durability,
revision, corpus seed/hash and raw results. Use native Linux and Windows local
filesystems; keep container results a separate series. Five fresh-process
lifecycle runs/size and at least 30 query/mutation samples/run; publish medians,
p95, ranges and peak RSS per isolated process. A noisy comparison is inconclusive,
not a pass. Cache-warm and cold-open cases must be labelled.

Required workloads:

- 1k/5k/10k retrieval documents, 3D audit reproduction and 128D qualification;
  empty/Unicode/NULL/high-cardinality text, both vector metrics, common/rare
  terms, tenant subsets and historical/transformed fallback. One, four and
  sixteen named indexes where all byte/scalar/source caps allow it; do not claim
  16 x 10k x 128D fits the active-envelope budget without checking.
- In-memory, **unlogged Sync** and explicitly **logged Sync** workspaces,
  baked import/open, vault commit/open and WASM byte import/export. Time index
  build, begin, row maintenance, encode/flush, commit, rollback and crash recovery
  separately. Batch 1/100/256 mutations and compare unchanged projections/IDs.
- Shared snapshot admission versus retained-reader retrieval; four concurrent
  readers and queued writers, drop/deadline/cancellation and journal/reader caps.
  Record queue latency and image-copy/source-verification costs separately.
- 100/1000-commit churn at fixed live row count, staying below retained-version
  limits. Record active index bytes, all CAS bytes/blob count, logical/physical
  workspace bytes, backup/temp space, retained-log bytes/count and RSS.
  Test admission just below/above caps and with a too-small journal; never
  silently drop history or widen resource limits to make benchmarks pass.
- Unindexed 100k/1m core workloads, existing seven stabilization budgets, and
  synced SQLite/DuckDB comparisons for representative overlapping SQL work.
  Compare equal durability/query meaning and preserve competitor losses.

Mandatory proposed performance gates (relative targets, frozen by M001):

1. Preserve all existing seven absolute stabilization budgets and pinned-runner
   median regression ceilings: +15% normal workloads, +25% recovery; investigate
   p95 above +25%/+40% respectively. Keep the existing same-run 10k persistent
   query benefit over ephemeral for every qualified query shape.
2. Verified open must decode each retrieval envelope once and traverse each
   indexed source table once for all its reference builds, charging **each
   index's unchanged budgets**. Require at least **20% lower median verified-open
   time for the four-index 10k corpus** on both native hosts; single-index open
   and all corruption rejection must not regress beyond the ceilings above.
3. Canonical commit encoding must remove the extra full-body staging copy where
   practical. Require at least **20% lower measured peak allocation bytes in the
   isolated envelope encoder** at the admitted near-limit corpus, with equal
   canonical bytes. Commit median/p95 and process RSS must meet the regression
   ceilings; allocation reduction alone must not hide slower persistence.
4. Logged rollback preparation for the qualified native recipe must create no
   full-workspace backup and must track touched pages/new bytes, not all existing
   CAS files. Retained logs are separately accounted/pruned by explicit durable
   acknowledgements. Unlogged costs remain reported and supported; no claim of
   bounded total disk or constant-time reopen.

Targets 2/3 are proposed 2.4 acceptance requirements, not achieved results. If
the targets fail after sound profiling, record `BLOCKED_PERFORMANCE` and return
the scope/evidence for founder review. Do not publish a cost-reduction claim,
change the reference workload, relax validation, or quietly raise the baseline.

## 7. Six executable implementation mandates

Each mandate gets a dedicated branch/worktree, exact source/evidence SHAs,
reviewable commits and a completion report. No mandate authorizes publication.

### PV-2.4-M001 — Freeze evidence, resolve policy debt

**Dependencies:** approved outcome; re-fetch main/#42/#43/#40 first.
**Owner role:** qualification/maintainer. **Files:** benchmark harnesses/results,
release ledger, SECURITY, CONCURRENCY, BENCHMARKS, ROADMAP and binding guides.

1. Preserve/reuse #42 harnesses with pinned attribution after their review, or
   adapt a minimal approved harness without treating its PR as merged. Extend
   the existing retrieval envelope to the measurement matrix above; instrument
   phase timings, decode/scan counts and allocation peaks under test/bench only.
2. Freeze reference raw data/budgets and distinguish release vs main vs candidate.
   Verify logged configuration on Rust/C/Python/Go; document an explicit batched
   logged recipe and prune/backup procedure. Do not change defaults.
3. Correct the contradictions listed above without rewriting historical release
   decisions. Resolve 1.86 dependency guard/downstream-resolution gaps; #40 is
   independently reviewable input, not a reason to adopt Arrow 60.

**Acceptance:** reproducible clean reference run on each native host; source and
workload hashes; feature/MSRV resolution evidence; approved fixed budgets;
truthful capability/debt tables. Missing a host/credential is reported separately
as `BLOCKED_EXTERNAL`; all available work still completes.

### PV-2.4-M002 — Reduce format-8 encoding allocation cost

**Depends on:** M001. **Owner role:** retrieval/storage. **Files:**
`src/persistent.rs`, canonical encoders in `src/search.rs`/`src/vector.rs`, focused
tests and benchmarks. Coordinate any integration changes in `src/db/persistent.rs`.

Profile `CatalogEntry::encode`, body encoding and `persist_retrieval`; emit the
same bounded canonical envelope with fewer intermediate full-body buffers.
Preserve all pre-allocation/checked-arithmetic limits, source digest, generation,
dirty-state and table/catalog atomic publication. `CatalogEntry::apply` already
updates changed documents and skips identical projections; do not reinvent it
as a new feature or promise O(delta) on-disk encoding. No sidecar/new framing.

**Acceptance:** gate 3; byte-equality against 2.3 fixtures/generated valid states;
unchanged no-op projection semantics; rejection/rollback at every encoding/CAS/
manifest fault point; model/corruption tests green. Failed encoding cannot leave
a transaction committable with partial catalog state.

### PV-2.4-M003 — Share verified-open work without reducing trust checks

**Depends on:** M001; integrate after M002 if touching its encoder internals.
**Owner role:** storage/retrieval. **Files:** `src/db/persistent.rs`, private
validation interfaces in `src/persistent.rs`, snapshot/open tests and benchmarks.

Today `load_catalog` structurally validates bytes and `verify_loaded` decodes
them again; `build_entry` scans source rows separately for every descriptor.
Refactor private interfaces to pass one structurally checked decoded object to
the full verification step, and build all references for one table during one
bounded scan. Reconstruct and compare every index's exact source/canonical body;
checksums or the stored digest alone cannot authorize a cached result. Keep
per-index budgets and feature-disabled rejection. Do not trust caller-provided
verification caches or skip source checks for imported/snapshot/vault images.

**Acceptance:** gate 2; checked decode/scan counts; rehashed corrupt bodies, stale
sources, descriptor mismatches and feature-disabled cases still reject before
queries; retained readers preserve their own generations; logged/unlogged/baked/
vault/byte-import results equal the reference. Snapshot copies remain explicit.

### PV-2.4-M004 — Qualify maintained bindings and browser/edge lifecycle

**Depends on:** M001; final binary evidence follows M002/M003 integration.
**Owner role:** bindings/browser. **Files:** maintained binding tests/guides,
`bindings/js/worker.js`, browser test harness and narrowly scoped CI additions.

Add additive worker `retrieve` and serialize complete asynchronous RPC operations
so open/save/close cannot overtake preceding work. Test delayed OPFS operations,
errors and statement invalidation with a deterministic reproducer. Establish a
single application-owned writer per OPFS file; separate tabs/workers remain a
host ownership requirement. Do not advertise cross-tab locks or fsync guarantees
the platform does not provide. Confirm interrupted/failed save retains a usable
previous image where supported; record any limitation/blocker honestly.

Run native C/Python/Go lifecycle/ownership tests with matching candidate libraries,
local JS/WASM build tests and a packaged candidate browser starter. Execute actual
OPFS in Chromium and one other OPFS-capable maintained browser, recording versions,
secure context, storage quota behavior, save/reload retrieval and failure recovery.
Compile/check existing streamed-read paths and prove range-error rejection; no
remote pager/backend is added. Clarify Node WASM vs native filesystem capabilities.

**Acceptance:** exact IDs/filtering/historical results and format-8 save/reload
parity; worker ordering reproducer passes; real-browser evidence, all native
ownership/closed-handle checks green; no unsupported platform support claim.

### PV-2.4-M005 — Adversarial recovery/FFI/security qualification

**Depends on:** M001; final assessment follows M002/M003/M004 integration and
the approved independent ingress fix. **Owner role:** reviewer independent of
those implementations. **Files:** fault/model/fuzz/FFI evidence, narrowly scoped
regressions, security finding ledger. Not the #43 implementation.

Exercise process exit at old/new publication boundaries, ENOSPC, permission loss,
sync/rename failure, failed encoding, lost log tail/middle, interrupted prune,
snapshot drop and retained readers. Assert atomic rows/indexes, sequence continuity
and `TransactionOutcomeUnknown` fencing/reopen where appropriate. Use seeded
monolith/record/index/retrieval/DDL/WASM fuzz targets and supported ASan/TSan jobs;
retain coverage/crash artifacts and disclose bounded durations. Review C panic
guards and allocation/lifetime contracts; invalid raw-pointer UB is not converted
into a safe-caller guarantee.

Triage the secret-file TOCTOU and bake directory-sync observations; fix a
confirmed material issue in a separate narrow mandate/PR and independently
retest. Carry #43's network regression into exact integrated-candidate evidence:
oversized/trickle heads, concurrent unauthenticated connections, cleanup, worker/
memory/descriptor ceilings, body deadline, authenticated query/health recovery.
Keep TLS/host authorization external. No duplicated ingress rewrite here.

**Acceptance:** zero unresolved critical/high findings; all recovery/integrity
regressions pass; no shortened/disabled existing CI. Record independent specialist
security/crypto review completion or `BLOCKED_EXTERNAL` with precise reviewer
scope. Agent audit, dependency scan and power-cut certification remain distinct.

### PV-2.4-M006 — Integrated qualification, migration and release dossier

**Depends on:** M001–M005 and approved #43 resolution. **Owner role:** release
qualification. **Files:** candidate release/verification ledger, migration
evidence, benchmark raw artifacts, version/package metadata when authorized.

Freeze the integration SHA. Run the unchanged all-feature Rust/Clippy/fmt/doc,
MSRV/feature-isolation, Linux/Windows, native C/Python/Go, local WASM/npm/browser,
dependency audit, fuzz/model/crash and stabilization gates plus the new evidence
matrix. Run full golden migration/history tests and two-way 2.3/2.4 image reads;
replay physical changes only against a verified matching base. Rehearse real
owner backups/restores/migrations and obtain three external application RC trial
acceptances with provenance. Generated substitutes do not satisfy these gates.

Deliver a dossier listing candidate SHA, every check/artifact hash, target-vs-
observed performance, defects, owner acceptance and remaining external gates.
Corrective changes invalidate affected evidence and require reruns. Prepare
matching package/license/provenance metadata without changing terms. Human
release authorization, tags/publication and exact post-publication registry
installs are a **separate** release mandate; a documentation PR does none of them.

**Acceptance:** all internal gates green, external gates closed or explicitly
`BLOCKED_EXTERNAL` pending a new documented founder decision. Historical deferrals
cannot be recycled as automatic 2.4 permission. No production-at-scale or
independent-audit claim without its actual evidence.

### Dependency order and parallelism

```text
approved outcome -> M001 evidence/policy freeze
                    |-> M002 bounded canonical encoding --|
                    |-> M003 shared verified-open work ---|
                    |-> M004 bindings/browser ------------|-> M006 dossier
                    `-> M005 adversarial work -------------|
independent #43 resolution -> final M005 assessment --------|
external reviewer / real owners -> external M006 gates ----|
```

M002/M003 can profile/design in parallel; their shared private catalog interfaces
need one agreed owner and serialized integration. M004 owns JS/binding changes;
M005 starts independent fault/FFI probes early, then reviews final fixes without
auditing its own implementation. Freeze each integrated revision before final
qualification. External scheduling must not idle unrelated engineering work.

## 8. Provisional path through 3.0

| Stage | Outcome trigger and gates |
| --- | --- |
| 2.3 patch lane | Close the independent ingress finding promptly through #43's process; qualify any MSRV/security fixes separately. Do not make users wait for 2.4 or imply an unissued 2.3.1 is available. |
| 2.4 | Execute the single chosen lifecycle outcome above on format 8/Rust 1.86; preserve caps and full verification. Founder outcome review precedes execution; completed dossier precedes release decision. |
| 2.5 candidate | If a named adopter needs one-way distribution, build a host-owned replica reference client using verified checkpoint/cursor, dedupe/gap/expiry/resnapshot, restart and durable acknowledgement. Transport/auth/key custody stay host-owned; no multi-writer claim. Otherwise continue measured lifecycle/adoption work rather than minting empty scope. |
| Later 2.x candidates | Evaluate CAS reachability/reclamation, reader-copy cost or larger retrieval from measured workloads. A CAS design must preserve every retained MVCC version, pinned image and required change/backup reference before reclaiming anything. ANN requires an accepted recall/latency/memory corpus; richer text search requires relevance evidence. MSRV changes need an explicit support decision. |
| 3.0 decision | Consider a breaking pager/snapshot/format/API redesign only if accepted 2.x evidence proves the current model prevents a real workload. Produce format migration/rollback, resource/durability/security models, binding maintenance owners and an independent review before selecting it. Offline multi-device sync additionally needs tombstone/schema/conflict/device identity/long-offline contracts; it is not promised for 3.0. |

These are gated options, not simultaneously selected outcomes or dates. A major
version is justified by a deliberate compatibility change, not weekly cadence.

## 9. Hub-only work excluded from the engine

Keep private Hub service ownership and repository/deployment work separate:
tenant accounts/roles/invitations/recovery; entitlements/delivery credentials;
licensing receipts/offers/billing/cancellation; private object storage/HTTPS
delivery; release registry/channel promotion/rollback; verifier process isolation;
alerts/SMTP/on-call; off-host backups and replacement-host restore drills;
service quotas/abuse controls; paid pilots/contracts/privacy/SLA; customer corpus
operations and deployed metadata-runtime upgrades. None is required in the engine
to open/query a file, and none is changed by these mandates.

Engine-owned seams remain local bake/inspect/import/migrate, dataset signing/
verification, optional native crypto, bounded retrieval and physical checkpoints/
changes. Hub may consume them through separately qualified clients. No mandatory
account, telemetry, proprietary network protocol or bundled cloud control plane
is added. The dated Hub pilot ledger is evidence of that pilot's recorded scope,
not a verified current production inventory.

## 10. Review exit

`PV-2.4 EXECUTION PLAN — READY_FOR_FOUNDER_REVIEW` means the baseline,
contradictions, single outcome, budgets, dependency order and excluded scope are
reviewable. It does **not** mean runtime/release gates passed. Founder review
selects this outcome and acceptance targets; external reviewers/owners and any
future publication remain explicit gates. This mandate changes documentation
only and does not merge, tag, publish, deploy or implement broad 2.4 runtime work.
