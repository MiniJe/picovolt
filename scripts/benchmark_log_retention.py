"""Check default unpruned RC2 log capacity without discarding required history.

Use the matching release benchmark_workspace initializer and PICOVOLT_LIB.
This is a local synthetic trial, not external application evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from picovolt import Database, version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--initializer", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--rows", type=int, default=10000)
    parser.add_argument("--commits", type=int, default=200)
    args = parser.parse_args()
    assert args.rows > 0 and args.commits > 0
    observations = []
    with tempfile.TemporaryDirectory(prefix="picovolt-retention-") as temporary:
        workspace = Path(temporary) / "workspace"
        subprocess.run([args.initializer, str(workspace)], check=True, capture_output=True)
        with Database.open_dev(str(workspace)) as db:
            rows = [(i, i % 50, (i * 17) % 1000, f"payload-category-{i % 50:02}") for i in range(args.rows)]
            db.execute_many("INSERT INTO events VALUES (?, ?, ?, ?)", rows)
            db.query("BEGIN")
            for table, column in [("events", "id"), ("events", "bucket"), ("categories", "bucket")]:
                db.query(f"CREATE INDEX ON {table} ({column})")
            db.query("COMMIT")
            before = db.commit_log_status()
            for i in range(args.rows, args.rows + args.commits):
                start = time.perf_counter_ns()
                db.query("INSERT INTO events VALUES (?, ?, ?, ?)", [i, i % 50, (i * 17) % 1000, f"payload-category-{i % 50:02}"])
                observations.append((time.perf_counter_ns() - start) / 1e6)
            after = db.commit_log_status()
            assert after["head_sequence"] == before["head_sequence"] + args.commits
            assert after["pruned_through"] == 0
        with Database.open_dev(str(workspace)) as db:
            result = db.query("SELECT id, bucket, amount, payload FROM events ORDER BY id")["rows"]
            expected = [[i, i % 50, (i * 17) % 1000, f"payload-category-{i % 50:02}"] for i in range(args.rows + args.commits)]
            assert result == expected
            assert db.commit_log_status() == after
        output = {"schema_version": 1, "version": version(), "rows_initial": args.rows,
                  "additional_commits": args.commits, "prune_calls": 0,
                  "status_before": before, "status_after": after, "commit_samples_ms": observations,
                  "verified_rows_after_reopen": len(result),
                  "verified_sha256": hashlib.sha256(json.dumps(result, separators=(",", ":")).encode()).hexdigest(),
                  "native_library_sha256": hashlib.sha256(Path(os.environ["PICOVOLT_LIB"]).read_bytes()).hexdigest(),
                  "note": "Single local retention trial; not a power-loss test or external application trial."}
        Path(args.output).write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(after, indent=2))


if __name__ == "__main__":
    main()
