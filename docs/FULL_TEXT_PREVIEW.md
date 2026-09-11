# Full-text index in the private 2.1.0 build

This private 2.1 Rust feature builds an application-owned inverted index from a
consistent PicoVolt SELECT result. It adds no storage format and does not change
published PicoVolt 2.0 packages. Enable `full-text` to use `search::SearchIndex`.

Queries match all distinct Unicode alphanumeric terms after lowercasing, then
rank with BM25 (k1 1.2, b 0.75). Equal scores sort by ascending integer document
ID. Updates replace the indexed document; deletions remove postings and update
corpus statistics. A rejected update leaves the old document searchable.

Use `SearchIndex::from_rows(&result, "id", &["title", "body"])` to build from one
query result with unique integer IDs. Rebuild after database commits and replace
the application's previous index only when the new build succeeds. The index
does not subscribe to commits or persist itself; the database remains the source
of truth. Rebuilding from an older snapshot produces an independent older index.

Bounds: 10,000 documents, 64 KiB/document, 8 MiB total source bytes, 65,536 indexed
terms, 4,096 distinct terms/document, 1,024 query bytes, 32 distinct query terms
and 100 results. These are input/cardinality bounds, not a measured memory SLA.
Tokens longer than 128 UTF-8 bytes are ignored. Vocabulary admission is
conservative when replacing a document at the global term cap.

No stemming, accent folding, stop-word list, phrase/prefix/fuzzy search, SQL
MATCH syntax, persistent full-text index or public package release is
claimed. The existing lexical guide search stays on the stable engine.
The 2.1 binding APIs are implemented; see [retrieval](RETRIEVAL_2_1.md).

Run it on the real documentation dataset:

```sh
cargo run --features full-text --example search_guides -- guides.pvdb transactions
```

Before release: representative relevance judgments, memory/latency measurements,
concurrent rebuild ownership, and the new release's licensing and
distribution checks. Keep this preview separate from the published 2.0 binary.
