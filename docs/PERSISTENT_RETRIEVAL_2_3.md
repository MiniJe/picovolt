# PicoVolt 2.3 — Persistent Retrieval

This is the release guide for **PicoVolt 2.3.0**. See
[qualification](RELEASE_2_3.md) for exact candidate, verification and publication
evidence. No Hub service, account, network call, telemetry, embedding provider,
or ANN dependency is involved.

## Create and use named indexes

Use an explicit stable, unique signed integer ID. Vectors remain application-
supplied finite JSON arrays stored in TEXT. Text columns accept TEXT or NULL.

```sql
CREATE TABLE articles (id, tenant, title, body, embedding);
INSERT INTO articles VALUES
  (1, 'team-a', 'Verified backups', 'Restore a database snapshot', '[1,0,0]');
CREATE INDEX article_search ON articles USING FULLTEXT (title, body)
  WITH (id_column = 'id');
CREATE INDEX article_embedding ON articles USING VECTOR (embedding)
  WITH (id_column = 'id', metric = 'cosine', dimensions = 3);
```

`metric = 'squared_euclidean'` creates an exact L2-squared index instead. Dimensions
are mandatory and immutable. Index names are database-wide and case-sensitive.
`CREATE INDEX IF NOT EXISTS` is a no-op only when the whole existing definition
matches; conflicting options are an error. `DROP INDEX [IF EXISTS] name` removes
the catalog object, not rows or historical row versions. Dropping a source table
also removes its retrieval definitions. Legacy `CREATE INDEX ON t (column)` is
unchanged and still means an ordinary secondary index.

```json
{
  "kind": "full_text", "index": "article_search",
  "sql": "SELECT id,title,body FROM articles WHERE tenant=?",
  "params": ["team-a"], "id_column": "id", "text_columns": ["title", "body"],
  "query": "database snapshot", "limit": 10
}
```

```json
{
  "kind": "vector", "index": "article_embedding",
  "sql": "SELECT id,embedding FROM articles WHERE tenant=?",
  "params": ["team-a"], "id_column": "id", "vector_column": "embedding",
  "query": [1,0,0], "metric": "cosine", "limit": 10
}
```

For hybrid requests add `text_index` and `vector_index` to the existing
[2.2 request](HYBRID_2_2.md). Keep the existing `query`, `vector_query`, `metric`,
`text_weight`, `candidate_limit`, column and SQL fields. Both lists are ranked
from one SELECT snapshot and fused with `weight / (60 + rank)`; explanation
ranks and deterministic ID tie-breaking are retained. Either optional index
reference can be omitted, using the ephemeral reference for that component.

## Semantics: filtering is part of the corpus

The authorized SELECT runs first. Plain column projections and `SELECT *` over
the current snapshot can use named indexes. WHERE, ORDER BY and LIMIT still
select the corpus; retrieval does not circumvent them. Full-text document count,
average length and each query term's document frequency are recomputed over that
selected ID set. Global BM25 followed by post-filtering is **not** used. Vector
search considers the same selected IDs and remains exhaustive and exact.

This preserves the 2.2 filtered-corpus ranking. Differential tests compare the
serialized results exactly on the same implementation/host; cross-platform
floating-point bit identity is not a new guarantee. Tokenization remains Unicode
alphanumeric, lowercase, AND matching, BM25 k1=1.2/b=0.75, with integer-ID ties.
No stemming, phrase/prefix/fuzzy matching, SQL MATCH, or embeddings are added.

Historical `BEFORE tx`, expressions/aliases, DISTINCT, grouping and HAVING cannot
use current index acceleration. They fall back to constructing the legacy index
from the exact selected snapshot, provided that query form is otherwise supported
by retrieval. Named table, ID, kind, column, metric and dimension mismatches are
still explicit errors; a misspelled reference does not silently fall back.

Requests without references retain the existing bounded per-call rebuild path.
All JSON result IDs remain decimal strings. Authorization is host-owned: do not
let a caller choose unrestricted SQL and assume an index supplies access control.

## Transactions, generations and storage

INSERT, UPDATE (including ID changes), DELETE and low-level row mutations maintain
affected indexes inside the same rollback boundary. An unchanged indexed
projection does not retokenize/reinsert its document. Failed statements abort an
explicit transaction according to the existing engine contract; callers must
not assume earlier statements remain pending. Commit publishes the table and
catalog together; rollback/drop restores the prior complete image/state.

Each descriptor records the last relevant MVCC transaction ID (`generation`),
not the physical change-stream sequence. Unrelated writes can advance the
snapshot clock without changing that generation. A private writer can inspect
`pending_write`; other readers never observe that uncommitted state. Native
`ReadTransaction::retrieve_json` uses its existing pinned private table/index
image. `SharedDatabase::retrieve_json` admits a new reader; retain a read
transaction to amortize snapshot admission for repeated queries.

Binary envelopes live in the existing CAS and are referenced by a bounded
manifest catalog. Dirty entries are encoded before publishing the manifest.
Logged commits include the new blobs and manifest in the existing physical
change record; an active, uncommitted journal rolls both back. Unlogged workspaces
use their existing rollback-image protocol. There is no independently published
retrieval sidecar. Process-crash coverage is not a hardware power-cut guarantee;
the existing [Windows/durability limits](CONCURRENCY.md) still apply.

Bake, byte import, read-only production images, dev reopen, encrypted snapshots,
and retained native snapshots carry the same catalog. Source rows remain the
authority. Open validates the binary structure, hashes, descriptor/definition,
counts, source fingerprint and a reconstructed reference index, then retains the
validated decoded index. **Open therefore performs one full source-verification
build per index.** Persistent retrieval avoids rebuilding on each query; it does
not promise O(1) opening, zero-copy snapshots or no SELECT materialization.

Changed index generations are immutable CAS blobs. A mutation can encode the
whole affected index at commit, and unreachable older generations are not
reclaimed automatically. Batch related changes in explicit transactions and
measure disk growth; dropping an index is not a compaction operation.

## Inspection and deterministic corruption policy

```sh
pv inspect articles.pvdb --json
pv retrieve articles.pvdb request.json
```

The `retrieval_indexes` inventory reports the complete definition, kind, document
count, source bytes, persisted bytes, generation, snapshot transaction, health,
pending-write flag and rebuild count. Rust exposes `Database::retrieval_indexes`,
`verify_retrieval_index(name)` and transactional `rebuild_retrieval_index(name)`;
`Database::inspect_stats` includes the same inventory. New separate CLI commands
are not required.

Every storage mode **fails open with an error** when admitted retrieval bytes are
malformed, stale or disagree with source rows. No automatic repair hides the
condition and no corrupt index answers queries. `pv inspect`/`pv retrieve` report
that failure rather than labelling the image healthy. Offline operators must
restore a verified backup/image. The rebuild API operates on an already valid,
open writable database; it is not a corruption-bypass recovery switch. A missing
compiled feature rejects a database requiring that index kind rather than
silently dropping its metadata.

## Resource envelope

| Operation/resource | Bound |
| --- | --- |
| Named indexes/database | 16 |
| Encoded envelope/index; active catalog bytes | 32 MiB; 64 MiB |
| Definition; identifier; indexed text columns | 8 KiB; 256 UTF-8 bytes; 1–16 |
| Documents/index and selected IDs | 10,000 |
| Retained source row versions/indexed table | 100,000 |
| Text source; source/document | 8 MiB; 64 KiB including column separators |
| Corpus terms; distinct terms/document | 65,536; 4,096 |
| Persisted (document, term) entries | 1,048,576 |
| Input token length | 128 UTF-8 bytes; longer tokens ignored |
| Vector dimensions; total scalar count | 1–4,096; 4,194,304 |
| Vector source text; JSON/vector | 16 MiB; 64 KiB |
| Retrieval request; SQL; parameter count | 128 KiB; 64 KiB; 256 |
| SELECT row versions; result rows; materialization | 100,000; 10,000; 16 MiB |
| Query text; distinct query terms; returned hits | 1,024 bytes; 32; 100 |
| Hybrid candidate limit | 1–100, at least result limit |

Counts, products, slices and encodings are checked before associated allocation.
These are admission limits, not total-RSS bounds. Containers, source/reference
verification and independent snapshots add overhead. Reader query limits can be
tightened; cancellation/deadlines are checked before/after ranking and at SELECT
checkpoints, not as hard preemptive interrupts inside every token/distance loop.

## Interfaces and compatibility

SQL DDL and optional JSON fields work through Rust, C `pv_query`/`pv_retrieve`,
Python `Database.query`/`retrieve`, Go `Query`/`Retrieve`, the JS SQLite-style
adapter and raw WASM `Db`, and CLI query/retrieve. WASM uses byte export/import;
it does not gain native filesystem or vault encryption. No new ABI pointer
ownership rules or embedding-provider interface is introduced.

Only databases using the capability require format 8, unless explicitly upgraded
with `pv migrate`/`upgrade_format_to_latest`. Ordinary open does not force old
index-less databases to 8. Explicit migration follows the existing policy of
stamping the newest format and does not invent retrieval definitions. Format-8
images require a 2.3-capable reader; older readers reject the version. Existing
v1–v7 golden files remain part of qualification. Exact bytes are documented in
[FORMAT.md](FORMAT.md#format-8-persistent-retrieval).

## Performance evidence

Run `cargo run --release --locked --example persistent_retrieval_envelope -- --bench`.
The JSON records 1,000/10,000 documents with 128-dimensional vectors, repeated
full-text/vector/hybrid queries with and without filters, creation/open costs,
mutation overhead, baked/workspace sizes and Linux process RSS. It compares the
named and legacy paths in one process and asserts equal results before timing.
The hosted job also requires every medium-corpus named query to beat its same-run
legacy counterpart. These observations are not SLAs; mutation samples are single
observations, not percentiles, and RSS includes several live database copies.
50,000–100,000 documents are outside the explicit 2.3 limit, not a qualified scale.
See [the release ledger](RELEASE_2_3.md) for retained evidence and exact revisions.
