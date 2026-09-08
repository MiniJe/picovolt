"""Serial, fresh-process primary-key bulk benchmark with full reopen checks.

Uses shipped Python APIs, default constraint indexes, and durable transactions.
RC2 at 50k is omitted because the independent review already recorded timeouts;
it is never treated as a completed measurement or extrapolated latency.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time
import uuid

from benchmark_competitors import Engine, peak_rss_bytes


def worker(args):
    folder = Path(args.run_dir) / f"{args.engine}-{args.rows}-{args.trial}"
    folder.mkdir(parents=True)
    name = "picovolt" if args.engine.startswith("picovolt") else args.engine
    path = folder / ("workspace" if name == "picovolt" else "database.db")
    engine = Engine(name, path)
    if name == "picovolt":
        engine.db.enable_commit_log()
    engine.query("CREATE TABLE t(id INTEGER PRIMARY KEY, g INTEGER, amount INTEGER)")
    data = [(i, i % 100, i % 101) for i in range(args.rows)]
    csv = folder / "input.csv"
    csv.write_text("id,g,amount\n" + "".join(f"{i},{g},{a}\n" for i, g, a in data), encoding="utf-8")
    cpu = time.process_time_ns()
    start = time.perf_counter_ns()
    if name == "picovolt":
        engine.db.execute_many("INSERT INTO t VALUES(?,?,?)", data)
    elif name == "sqlite":
        engine.query("BEGIN")
        engine.db.executemany("INSERT INTO t VALUES(?,?,?)", data)
        engine.query("COMMIT")
    else:
        engine.query(f"COPY t FROM '{csv.as_posix()}' (HEADER, DELIMITER ',')")
    elapsed = (time.perf_counter_ns() - start) / 1e6
    cpu_ms = (time.process_time_ns() - cpu) / 1e6
    peak = peak_rss_bytes()
    assert engine.query("SELECT * FROM t ORDER BY id") == data
    version = engine.version
    engine.close()
    engine = Engine(name, path)
    actual = engine.query("SELECT * FROM t ORDER BY id")
    assert actual == data
    digest = hashlib.sha256(json.dumps(actual, separators=(",", ":")).encode()).hexdigest()
    # Verify uniqueness after reopen, outside all measured regions.
    rejected = False
    try:
        engine.query("INSERT INTO t VALUES(0,0,0)")
    except Exception:
        rejected = True
    assert rejected, "PRIMARY KEY was not enforced"
    assert engine.query("SELECT COUNT(*) FROM t") == [(args.rows,)]
    engine.close()
    stored = [p for p in folder.rglob("*") if p.is_file() and p != csv]
    result = dict(engine=args.engine, version=version, rows=args.rows, trial=args.trial,
                  elapsed_ms=elapsed, cpu_ms=cpu_ms, peak_process_rss_bytes=peak,
                  stored_bytes=sum(p.stat().st_size for p in stored),
                  verified_sha256=digest, uniqueness_after_reopen=True)
    Path(args.output).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rc2-library", required=True)
    parser.add_argument("--rc3-library", required=True)
    parser.add_argument("--rc3-source", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--engine")
    parser.add_argument("--rows", type=int)
    parser.add_argument("--trial", type=int, default=0)
    parser.add_argument("--run-dir")
    args = parser.parse_args()
    if args.engine:
        worker(args)
        return
    root = Path(__file__).resolve().parents[1]
    run_dir = root / "target/primary-key-benchmarks" / str(uuid.uuid4())
    run_dir.mkdir(parents=True)
    runs = []
    for rows in [1000, 10000, 50000]:
        engines = ["picovolt-rc3", "sqlite", "duckdb"]
        if rows < 50000:
            engines.append("picovolt-rc2")
        for trial in range(args.trials):
            for engine in engines[trial % len(engines):] + engines[:trial % len(engines)]:
                output = run_dir / f"{engine}-{rows}-{trial}.json"
                env = os.environ.copy()
                env["PICOVOLT_LIB"] = args.rc2_library if engine == "picovolt-rc2" else args.rc3_library
                command = [sys.executable, __file__, "--rc2-library", args.rc2_library,
                           "--rc3-library", args.rc3_library, "--rc3-source", args.rc3_source,
                           "--output", str(output), "--run-dir", str(run_dir), "--engine", engine,
                           "--rows", str(rows), "--trial", str(trial)]
                print(f"{rows} rows, trial {trial + 1}/{args.trials}: {engine}", flush=True)
                subprocess.run(command, check=True, env=env, timeout=180)
                runs.append(json.loads(output.read_text(encoding="utf-8")))
        assert len({r["verified_sha256"] for r in runs if r["rows"] == rows}) == 1
    summary = {}
    for rows in [1000, 10000, 50000]:
        summary[str(rows)] = {}
        for engine in sorted({r["engine"] for r in runs if r["rows"] == rows}):
            trials = [r for r in runs if r["rows"] == rows and r["engine"] == engine]
            summary[str(rows)][engine] = dict(
                version=trials[0]["version"], n=len(trials),
                median_ms=statistics.median(r["elapsed_ms"] for r in trials),
                min_ms=min(r["elapsed_ms"] for r in trials), max_ms=max(r["elapsed_ms"] for r in trials),
                median_cpu_ms=statistics.median(r["cpu_ms"] for r in trials),
                median_peak_process_rss_bytes=statistics.median(r["peak_process_rss_bytes"] for r in trials),
                median_stored_bytes=statistics.median(r["stored_bytes"] for r in trials))
    result = dict(schema_version=1, date_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                  platform=platform.platform(), python=sys.version, trials=args.trials,
                  harness_commit=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
                  sources={"rc2": "eeb45c025b53f1b764d1e50774d6aaf700e17c77", "rc3": args.rc3_source},
                  library_sha256={p: hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in [args.rc2_library, args.rc3_library]},
                  method="Fresh serial processes; native bulk APIs/COPY; synced PicoVolt default log, SQLite WAL FULL, DuckDB persistent threads=1. CSV generation excluded; binding and commit included. Default PRIMARY KEY indexes, no manually added indexes. No cache flushing. CPU and process peak measured through ingestion; full rows and uniqueness checked after reopen.",
                  omissions=["RC2 50k: prior independent review timeout; no extrapolated result"],
                  summary=summary, runs=runs)
    Path(args.output).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
