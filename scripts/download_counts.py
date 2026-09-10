#!/usr/bin/env python3
"""Report public download counters without equating downloads with users."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
from pathlib import Path
from urllib.request import Request, urlopen

SOURCES = {
    "crates": "https://crates.io/api/v1/crates/picovolt",
    "npm": "https://api.npmjs.org/downloads/point/last-month/picovolt",
    "pypi": "https://pypistats.org/api/packages/picovolt/recent",
    "github": "https://api.github.com/repos/MiniJe/picovolt/releases?per_page=100",
}


def fetch(source):
    name, url = source
    try:
        with urlopen(Request(url, headers={"User-Agent": "PicoVolt-download-report"}), timeout=30) as response:
            data = json.load(response)
        if name == "crates":
            data = {key: data["crate"][key] for key in ("downloads", "recent_downloads", "max_stable_version")}
        elif name == "github":
            releases = data
            data = {
                "releases_returned": len(releases),
                "complete": len(releases) < 100,
                "releases": [{"tag": release["tag_name"], "downloads": sum(a["download_count"] for a in release["assets"]),
                              "assets": [{"name": a["name"], "downloads": a["download_count"]} for a in release["assets"]]}
                             for release in releases],
            }
            data["asset_downloads"] = sum(r["downloads"] for r in data["releases"])
        return name, {"url": url, "data": data}
    except Exception as error:
        return name, {"url": url, "error": str(error)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with ThreadPoolExecutor(max_workers=4) as pool:
        sources = dict(pool.map(fetch, SOURCES.items()))
    report = {"checked_at": datetime.now(timezone.utc).isoformat(), "sources": sources,
              "note": "Counters include automation and repeat downloads. Time windows differ; do not add them as unique users. GitHub counts release assets, including checksums and SBOMs, not source clones."}
    output = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.write_text(output, encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
