# Public data used by the 1.8 release gate

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
