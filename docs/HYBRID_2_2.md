# Hybrid retrieval in 2.2

Full-text and exact vector ranking now combine over **one filtered SELECT
snapshot**. Authorization still belongs to the host; put the authorized scope
in that SELECT before either ranking runs.

```json
{
  "kind": "hybrid",
  "sql": "SELECT id,title,embedding FROM articles WHERE workspace=?",
  "params": ["team-a"],
  "id_column": "id",
  "text_columns": ["title"],
  "vector_column": "embedding",
  "query": "verified backups",
  "vector_query": [1, 0],
  "metric": "cosine",
  "text_weight": 0.5,
  "candidate_limit": 50,
  "limit": 10
}
```

Use this with `Database::retrieve_json`, Python `db.retrieve`, Go `DB.Retrieve`,
C `pv_retrieve`, the JavaScript SQLite-style adapter/raw WASM, `pv retrieve`, or
the encrypted vault's retrieval interface. Vectors remain application-supplied;
PicoVolt does not send text to an embedding provider.

Each enabled ranking contributes `weight / (60 + rank)`, with one-based ranks.
Vector weight is `1 - text_weight`. This weighted reciprocal-rank fusion avoids
adding BM25 scores to incompatible distance units. Results include decimal-string
`id`, fusion `score`, `text_rank` and `vector_rank`; absent ranks are JSON null.
Equal fusion scores sort by integer ID.

`text_weight` must be finite and in [0,1]. At 1 only text matches are candidates;
at 0 only vector matches contribute. Both supplied inputs/columns are still
validated. `candidate_limit` is 1–100 and at least `limit`, which is 0–100.
Fusion ranks the union of those two bounded candidate lists, not every document;
changing the candidate limit can change results. This is not an ANN index or a
claim about relevance on an unmeasured corpus. The existing 2.1 text/vector and
snapshot limits continue to apply: see [the retrieval contract](RETRIEVAL_2_1.md).
