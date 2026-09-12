# Retrieval in PicoVolt 2.1.0

2.1 adds ranked full-text search and exact nearest-neighbor vector search to the
engine. They are default Cargo features (`full-text`, `vector-search`) and work
on development, memory and read-only baked databases. No file-format migration,
network service, embedding provider or telemetry is introduced.

## Full-text search

```python
from picovolt import Database

with Database.open_memory() as db:
    db.query('CREATE TABLE articles (id, tenant, title, body, embedding)')
    db.query('INSERT INTO articles VALUES (?, ?, ?, ?, ?)',
             [1, 'team-a', 'Database backups', 'Restore a verified snapshot', '[1,0,0]'])
    hits = db.retrieve({
        'kind': 'full_text',
        'sql': 'SELECT id,title,body FROM articles WHERE tenant=?',
        'params': ['team-a'],
        'id_column': 'id', 'text_columns': ['title', 'body'],
        'query': 'verified snapshot', 'limit': 10,
    })
    print(hits)  # [{'id': '1', 'score': ...}]
```

Terms are Unicode alphanumeric tokens lowercased and matched with AND semantics.
Ranking is BM25 (k1=1.2, b=0.75); equal scores sort by integer ID. No stemming,
phrase/fuzzy/prefix syntax or language-specific normalization is claimed.
The database query applies tenant/category filters before corpus statistics and
ranking. Authentication and the choice of authorized SQL still belong to the host.

## Vector search

Use a JSON array of finite numbers in a TEXT column. Supply embeddings generated
by your own application; PicoVolt does not generate or send them to a provider.

```json
{
  "kind": "vector",
  "sql": "SELECT id,embedding FROM articles WHERE tenant=?",
  "params": ["team-a"],
  "id_column": "id",
  "vector_column": "embedding",
  "query": [1, 0, 0],
  "metric": "cosine",
  "limit": 10
}
```

Results contain `id` and `distance`, smallest first. Metrics are `cosine`
(distance 0–2, nonzero vectors required) and `squared_euclidean`. This is exact
exhaustive search, not an approximate graph/ANN index. Equal distances sort by ID.
Invalid dimensions, NaN/infinity and zero cosine vectors fail explicitly.

## Interfaces and reusable indexes

- Rust: `db.retrieve_json(&request_json)`; reuse `search::SearchIndex::from_rows`
  or `vector::VectorIndex` for repeated queries without rebuilding. Both support
  atomic validated replacement and removal. Applications own index lifetime and
  rebuild after relevant commits; no implicit database-change subscription exists.
- Python: `db.retrieve(request_dict)` returns a list.
- JavaScript: `db.retrieve(request)` through `picovolt/sqlite`; raw WASM `Db`
  accepts a JSON string and returns a JSON string. Free raw handles after use.
- Go: `db.Retrieve(requestJSON)` returns a JSON string and an error.
- C: `pv_retrieve(db, request_json)`, released with `pv_string_free`.
- CLI: `pv retrieve database.pvdb request.json`; requires an existing database.

All JSON result IDs are decimal **strings**, preserving all signed 64 bits in
browsers. Rust index results retain `i64` IDs. SQL parameters here support null,
text, bool and signed integers. SQL must be one SELECT; writes, transaction
commands and multi-statement input are rejected before execution.

## Resource and consistency contract

Each call rebuilds an index from one bounded SELECT result: at most 100,000
scanned row versions, 10,000 result rows and 16 MiB materialized query data.
The request is limited to 128 KiB and SQL to 64 KiB. Results are limited to 100.
SQL ORDER/LIMIT narrows the indexed corpus; no hidden truncation occurs.

Full-text bounds: 8 MiB source text, 64 KiB/document, 65,536 corpus terms,
4,096 distinct terms/document, 1,024 query bytes and 32 distinct query terms.
Tokens longer than 128 UTF-8 bytes are ignored. Vector bounds: 4,096 dimensions,
10,000 documents and 4,194,304 scalar elements. These are explicit admission
budgets, not a measured production memory SLA. No persistent search index,
distributed replication or concurrent mutation of a retained index is supplied.

Existing 2.0 files remain readable. Production Hub and the main website retain
their qualified 2.0 runtime until separate upgrade qualification completes.
