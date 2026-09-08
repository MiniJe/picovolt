"""Reproducible end-to-end Python SQL comparison; see benchmarks/COMPETITORS_2_0.md.

Each engine/trial runs in a fresh process, serially. Setup, verification and
process startup are excluded from query timings. No OS cache flushing is done.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time
import uuid

# Optional isolated Windows runtime override, loaded before Python's SQLite
# extension. This never replaces the user's Python installation or its DLL.
SQLITE_LIBRARY = os.environ.get("PICOVOLT_BENCH_SQLITE_LIBRARY")
if SQLITE_LIBRARY:
    if os.name != "nt":
        raise RuntimeError("PICOVOLT_BENCH_SQLITE_LIBRARY requires Windows")
    import ctypes
    _sqlite_runtime = ctypes.WinDLL(str(Path(SQLITE_LIBRARY).resolve()))
import sqlite3

CPU_SAMPLES = {}


def peak_rss_bytes():
    if os.name == "nt":
        import ctypes
        class Counters(ctypes.Structure):
            _fields_ = [("cb", ctypes.c_uint32), ("faults", ctypes.c_uint32)] + [
                (name, ctypes.c_size_t) for name in ["peak", "working", "paged_peak", "paged",
                    "nonpaged_peak", "nonpaged", "pagefile", "pagefile_peak"]]
        counters = Counters()
        counters.cb = ctypes.sizeof(counters)
        query = ctypes.windll.psapi.GetProcessMemoryInfo
        query.argtypes = [ctypes.c_void_p, ctypes.POINTER(Counters), ctypes.c_uint32]
        query.restype = ctypes.c_int
        if not query(ctypes.c_void_p(-1), ctypes.byref(counters), counters.cb):
            raise ctypes.WinError()
        return counters.peak
    import resource
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return peak if sys.platform == "darwin" else peak * 1024


def row(i):
    return (i, i % 50, (i * 17) % 1000, f"payload-category-{i % 50:02}")


class Engine:
    def __init__(self, name, path, initializer=None):
        self.name, self.path = name, path
        if name == "picovolt":
            import picovolt
            if initializer:
                subprocess.run([initializer, str(path)], check=True, capture_output=True)
            self.db = picovolt.Database.open_dev(str(path))
            self.version = picovolt.version()
        elif name == "sqlite":
            self.db = sqlite3.connect(path, isolation_level=None)
            assert self.db.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
            self.db.execute("PRAGMA synchronous=FULL")
            assert self.db.execute("PRAGMA synchronous").fetchone()[0] == 2
            self.version = sqlite3.sqlite_version
        else:
            import duckdb
            self.db = duckdb.connect(str(path), config={"threads": "1"})
            self.version = duckdb.__version__

    def query(self, sql, params=None):
        if self.name == "picovolt":
            result = self.db.query(sql, params)
            return [tuple(r) for r in result.get("rows", [])]
        return self.db.execute(sql, params or []).fetchall()

    def close(self):
        self.db.close()


def timed(samples, name, function):
    cpu_start = time.process_time_ns()
    start = time.perf_counter_ns()
    result = function()
    samples.setdefault(name, []).append((time.perf_counter_ns() - start) / 1e6)
    CPU_SAMPLES.setdefault(name, []).append((time.process_time_ns() - cpu_start) / 1e6)
    return result


def summarize(values):
    ordered = sorted(values)
    return {"n": len(values), "median_ms": statistics.median(values),
            "p95_ms": ordered[math.ceil(len(ordered) * .95) - 1],
            "min_ms": ordered[0], "max_ms": ordered[-1]}


def worker(args):
    trial_dir = Path(args.run_dir) / f"{args.engine}-{args.trial}"
    trial_dir.mkdir(parents=True, exist_ok=False)
    db_path = trial_dir / ("workspace" if args.engine == "picovolt" else "database.db")
    engine = Engine(args.engine, db_path, args.initializer)
    samples = {}
    prune_cli = Path(args.initializer).resolve().parents[1] / ("pv.exe" if os.name == "nt" else "pv")

    def prune():
        # Keep the same shipped CLI path for RC1/RC2 comparisons. RC2 also has
        # a native Python pruning API. Include CLI startup/open/pruning here.
        sequences = [int(p.name) for p in (db_path / ".pv-log").iterdir()
                     if p.is_dir() and len(p.name) == 20 and p.name.isdigit()]
        if sequences:
            subprocess.run([str(prune_cli), "log-prune", str(db_path), str(max(sequences))],
                           check=True, capture_output=True)
    if args.engine != "picovolt":
        engine.query("CREATE TABLE events (id INTEGER, bucket INTEGER, amount INTEGER, payload TEXT)")
        engine.query("CREATE TABLE categories (bucket INTEGER, label TEXT)")
    # Warm parameter binding/imports using SQL supported by all three engines.
    assert engine.query("SELECT id FROM events WHERE id = ?", [42]) == []
    engine.query("BEGIN")
    for i in range(50):
        engine.query("INSERT INTO categories VALUES (?, ?)", [i, f"category-{i:02}"])
    engine.query("COMMIT")
    data = [row(i) for i in range(args.rows)]

    def insert_batch(rows):
        engine.query("BEGIN")
        for values in rows:
            engine.query("INSERT INTO events VALUES (?, ?, ?, ?)", values)
        engine.query("COMMIT")

    timed(samples, "load_transaction", lambda: insert_batch(data))
    def indexes():
        engine.query("BEGIN")
        for table, column in [("events", "id"), ("events", "bucket"), ("categories", "bucket")]:
            name = "" if args.engine == "picovolt" else f"{table}_{column} "
            engine.query(f"CREATE INDEX {name}ON {table} ({column})")
        engine.query("COMMIT")
    timed(samples, "create_indexes", indexes)

    # Warm each query shape five times; measure materialization, check outside timer.
    queries = []
    for i in range(200):
        key = (i * 7919 + 17) % args.rows
        queries.append(("indexed_point", "SELECT id, bucket, amount, payload FROM events WHERE id = ?",
                        [key], [data[key]]))
    for i in range(30):
        low = (i * 313) % (args.rows - 100)
        queries.append(("indexed_range_100", "SELECT id, amount FROM events WHERE id >= ? AND id < ? ORDER BY id",
                        [low, low + 100], [(r[0], r[2]) for r in data[low:low+100]]))
    groups = [(i, sum(r[2] for r in data if r[1] == i)) for i in range(50)]
    for _ in range(20):
        queries.append(("group_sum", "SELECT bucket, SUM(amount) FROM events GROUP BY bucket ORDER BY bucket", [], groups))
    top = [(r[0], r[2]) for r in sorted(data, key=lambda r: (-r[2], r[0]))[:20]]
    for _ in range(20):
        queries.append(("top_20", "SELECT id, amount FROM events ORDER BY amount DESC, id LIMIT 20", [], top))
    for i in range(20):
        key = (i * 7) % 50
        expected = [(r[0], f"category-{key:02}") for r in data if r[1] == key]
        queries.append(("indexed_join", "SELECT events.id, categories.label FROM events JOIN categories ON events.bucket = categories.bucket WHERE events.bucket = ? ORDER BY events.id", [key], expected))
    warmed = set()
    for name, sql, params, expected in queries:
        if name not in warmed:
            for _ in range(5):
                assert engine.query(sql, params) == expected, (name, args.engine)
            warmed.add(name)
        actual = timed(samples, name, lambda: engine.query(sql, params))
        assert actual == expected, (name, args.engine, actual[:3], expected[:3])

    # 60 separately committed rows, followed by 10 transactions of 100 rows.
    if args.engine == "picovolt":
        timed(samples, "initial_prune_cli", prune)
    write_start = time.perf_counter_ns()
    for i in range(args.rows, args.rows + 60):
        values = row(i)
        timed(samples, "single_row_commit", lambda: engine.query("INSERT INTO events VALUES (?, ?, ?, ?)", values))
        data.append(values)
        if args.engine == "picovolt" and (i - args.rows + 1) % 10 == 0:
            timed(samples, "prune_10_commits_cli", prune)
    for batch in range(10):
        values = [row(i) for i in range(args.rows + 60 + batch*100, args.rows + 160 + batch*100)]
        timed(samples, "batch_100_commit", lambda: insert_batch(values))
        data.extend(values)
    if args.engine == "picovolt":
        timed(samples, "prune_10_commits_cli", prune)
    samples["sustained_write_phase"] = [(time.perf_counter_ns() - write_start) / 1e6]
    def rollback():
        engine.query("BEGIN")
        engine.query("DELETE FROM events WHERE id = 0")
        engine.query("ROLLBACK")
    timed(samples, "rollback_one_delete", rollback)
    assert engine.query("SELECT COUNT(*), SUM(amount) FROM events") == [(len(data), sum(r[2] for r in data))]
    engine.close()
    engine = timed(samples, "warm_os_cache_reopen", lambda: Engine(args.engine, db_path))
    actual = engine.query("SELECT id, bucket, amount, payload FROM events ORDER BY id")
    assert actual == data
    digest = hashlib.sha256(json.dumps(actual, separators=(",", ":")).encode()).hexdigest()
    engine.close()
    files = [p for p in trial_dir.rglob("*") if p.is_file()]
    total = sum(p.stat().st_size for p in files)
    log = sum(p.stat().st_size for p in files if ".pv-log" in p.parts)
    main_peak_rss = peak_rss_bytes()
    if args.bulk_api:
        # Separate database: preserve the common SQL workload and its size.
        bulk_path = trial_dir / ("bulk-workspace" if args.engine == "picovolt" else "bulk.db")
        bulk = Engine(args.engine, bulk_path, args.initializer)
        if args.engine != "picovolt":
            bulk.query("CREATE TABLE events (id INTEGER, bucket INTEGER, amount INTEGER, payload TEXT)")
        initial = [row(i) for i in range(args.rows)]
        if args.engine == "duckdb":
            import csv
            csv_path = trial_dir / "bulk.csv"
            with csv_path.open("w", newline="", encoding="utf-8") as stream:
                csv.writer(stream).writerows(initial)
        def bulk_load():
            if args.engine == "picovolt":
                assert bulk.db.execute_many("INSERT INTO events VALUES (?, ?, ?, ?)", initial) == args.rows
            elif args.engine == "sqlite":
                bulk.query("BEGIN")
                bulk.db.executemany("INSERT INTO events VALUES (?, ?, ?, ?)", initial)
                bulk.query("COMMIT")
            else:
                path = str(csv_path.resolve()).replace("'", "''")
                bulk.query(f"COPY events FROM '{path}' (FORMAT CSV, HEADER FALSE)")
        timed(samples, "bulk_api_load_transaction", bulk_load)
        assert bulk.query("SELECT id, bucket, amount, payload FROM events ORDER BY id") == initial
        bulk.close()
        bulk = Engine(args.engine, bulk_path)
        assert bulk.query("SELECT COUNT(*), SUM(amount) FROM events") == [(args.rows, sum(r[2] for r in initial))]
        bulk.close()
    result = {"engine": args.engine, "version": engine.version, "trial": args.trial,
              "rows_initial": args.rows, "rows_final": len(data), "verified_sha256": digest,
              "file_bytes_after_close": total, "retained_log_bytes": log,
              "base_file_bytes": total-log, "metrics": {k: summarize(v) for k, v in samples.items()},
              "samples_ms": samples, "cpu_samples_ms": CPU_SAMPLES,
              "measured_cpu_ms_excluding_cli_children": {k: sum(v) for k,v in CPU_SAMPLES.items()},
              "peak_process_rss_bytes": main_peak_rss,
              "peak_process_rss_including_optional_bulk_bytes": peak_rss_bytes()}
    Path(args.output).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--initializer", required=True, help="release benchmark_workspace executable")
    parser.add_argument("--output", required=True)
    parser.add_argument("--rows", type=int, default=10000)
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--engine", choices=["picovolt", "sqlite", "duckdb"])
    parser.add_argument("--trial", type=int, default=0)
    parser.add_argument("--run-dir")
    parser.add_argument("--bulk-api", action="store_true", help="also measure native batch APIs and DuckDB COPY on a separate database")
    parser.add_argument("--picovolt-source-commit", help="source revision of the supplied PicoVolt binaries, if different from the harness")
    args = parser.parse_args()
    if args.rows <= 100 or args.trials < 1:
        parser.error("rows must exceed 100 and trials must be positive")
    if args.engine:
        worker(args)
        return
    root = Path(__file__).resolve().parents[1]
    run_dir = root / "target" / "competitor-benchmarks" / str(uuid.uuid4())
    run_dir.mkdir(parents=True)
    engines = ["picovolt", "sqlite", "duckdb"]
    results = []
    for trial in range(args.trials):
        for engine in engines[trial % 3:] + engines[:trial % 3]:
            output = run_dir / f"{engine}-{trial}.json"
            command = [sys.executable, __file__, "--engine", engine, "--trial", str(trial),
                       "--rows", str(args.rows), "--initializer", args.initializer,
                       "--run-dir", str(run_dir), "--output", str(output)]
            print(f"Trial {trial+1}/{args.trials}: {engine}", flush=True)
            if args.bulk_api:
                command.append("--bulk-api")
            subprocess.run(command, check=True)
            results.append(json.loads(output.read_text(encoding="utf-8")))
    assert len({r["verified_sha256"] for r in results}) == 1
    summary = {}
    for engine in engines:
        trials = [r for r in results if r["engine"] == engine]
        summary[engine] = {"version": trials[0]["version"], "metrics": {},
                           "file_bytes_after_close": statistics.median(r["file_bytes_after_close"] for r in trials),
                           "base_file_bytes": statistics.median(r["base_file_bytes"] for r in trials),
                           "retained_log_bytes": statistics.median(r["retained_log_bytes"] for r in trials)}
        summary[engine]["peak_process_rss_bytes"] = statistics.median(r["peak_process_rss_bytes"] for r in trials)
        summary[engine]["measured_cpu_ms_excluding_cli_children"] = {
            k: statistics.median(r["measured_cpu_ms_excluding_cli_children"][k] for r in trials)
            for k in trials[0]["measured_cpu_ms_excluding_cli_children"]}
        for metric in trials[0]["metrics"]:
            medians = [r["metrics"][metric]["median_ms"] for r in trials]
            summary[engine]["metrics"][metric] = {
                "median_of_trial_medians_ms": statistics.median(medians),
                "trial_median_min_ms": min(medians), "trial_median_max_ms": max(medians),
                "median_of_trial_p95_ms": statistics.median(r["metrics"][metric]["p95_ms"] for r in trials)}
    result = {"schema_version": 1, "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
              "picovolt_source_commit": args.picovolt_source_commit or subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
              "platform": platform.platform(), "python": sys.version, "processor": platform.processor(),
              "logical_cpus": os.cpu_count(), "rows_initial": args.rows, "trials": args.trials,
              "picovolt_artifact_sha256": {
                  str(Path(p).resolve()): hashlib.sha256(Path(p).read_bytes()).hexdigest()
                  for p in [args.initializer, os.environ.get("PICOVOLT_LIB")]
                  if p and Path(p).is_file()},
              "client": "Python public APIs; PicoVolt ctypes/JSON, SQLite stdlib, DuckDB native extension",
              "sqlite_runtime_override": None if not SQLITE_LIBRARY else {
                  "path": str(Path(SQLITE_LIBRARY).resolve()),
                  "sha256": hashlib.sha256(Path(SQLITE_LIBRARY).read_bytes()).hexdigest(),
                  "version": sqlite3.sqlite_version},
              "durability": {"picovolt": "format 6 commit log; Sync transactions; default limits; explicit CLI pruning before writes and every 10 commits, timed separately and included in sustained_write_phase",
                             "sqlite": "WAL, synchronous=FULL; default automatic checkpoint", "duckdb": "persistent database; default WAL/checkpoint; threads=1"},
              "summary": summary, "runs": results}
    Path(args.output).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
