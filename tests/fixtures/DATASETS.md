# Release-gate datasets and golden images

`iris.data` is the unmodified 150-row Iris dataset distributed by the
[UCI Machine Learning Repository](https://archive.ics.uci.edu/dataset/53/iris).

- Citation: Fisher, R. (1936). Iris [Dataset]. UCI Machine Learning Repository.
  https://doi.org/10.24432/C56C76.
- License: [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
- Download: https://archive.ics.uci.edu/ml/machine-learning-databases/iris/iris.data
- Retrieved: 2026-09-04.
- SHA-256: `6f608b71a7317216319b4d27b4d9bc84e6abd734eda7872b71a458569e2656c0`.

The test loads all original rows into SQLite, compares PicoVolt's grouped
results against SQLite, exports/imports Parquet, and bakes/reopens the result.
It repeats the original rows 20 times solely to exercise multiple Parquet row
groups and a database larger than the two-page buffer pool. The fixture itself
is unchanged, including the historical values documented by UCI.

The `golden_v*.pvdb` files are generated compatibility fixtures, not downloaded
datasets. Each freezes an on-disk format generation so current readers and the
1.9 migration tool can verify every retained transaction snapshot. In
particular, `golden_v1_9_0.pvdb` is a format-v5 image containing packed decimal
cold pages, MVCC history, and a persisted index. Regenerate goldens only with
`cargo run --example make_golden`; tests require older fixtures to remain
byte-for-byte unchanged.
