# Repository and website review — September 10, 2026

Reviewed the engine repository's public documentation, contribution guidance,
package metadata, release configuration and automated checks, plus the maintained
static website at `D:/picovolt-website`. The separate `D:/picovolt-app` directory
is an older React application and was not changed or used for deployment.

## Corrections

- Added an immediately runnable Rust quickstart to the main README.
- Removed obsolete claims about serialized readers, format v5, and the absence
  of incremental journals. Qualified historical performance numbers and linked
  the measured benchmark baseline.
- Updated contribution guidance to the 2.0 release and added editor conventions
  and local environment-file exclusions.
- Aligned the website, install snippets and 20 guides with the published 2.0.0
  release while preserving historical RC3 benchmark and review evidence.
- Replaced the website's 1.9.0 WASM assets with published 2.0.0 assets after
  verifying the npm tarball's SHA-512 integrity.
- Fixed the Apache CSP to permit WebAssembly, added asset revalidation and
  certificate-challenge handling, corrected canonical/domain metadata and added
  documentation URLs to the sitemap.
- Added a reproducible Hetzner package command, pinned Python build dependencies,
  real-engine smoke checks and an upload/rollback guide.

## Validation performed locally

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo test --locked --all-targets --all-features` | Passed on Windows |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --locked --doc` | 2 passed |
| `cargo audit` | Passed; 149 dependencies scanned |
| Starter-policy unit suite | 24 passed |
| Registry-only starter policy | Passed for 2.0.0 |
| Tracked Markdown file destinations | 38 files checked; no missing local file links |
| Website Markdown tests | 3 passed |
| Website integration checks | 22 pages; links, anchors, guides, controls, canonical metadata, sitemap and CSP checked |
| Website WASM smoke checks | Browse, join, aggregate, write, schema, historical read, export and reopen passed |
| Hetzner ZIP | 38 explicitly selected public files; ZIP integrity and SHA-256 recorded beside archive |

These checks are not an exhaustive manual review of every engine implementation,
an independent security audit, or a browser screenshot/accessibility audit.
Independent review and external application trials remain deferred as recorded
in the release ledger. The Apache configuration also passed the live Hetzner
checks after upload.

## Download counts

For the subsequently requested lifetime totals checked in third-party viewers,
see [Lifetime download counts](DOWNLOAD_LIFETIME_2026_09_10.md). The table below
preserves the original API snapshot and its explicitly different time windows.

Snapshot captured at 12:42 UTC on September 10, 2026. Raw API responses and
source URLs are in [DOWNLOAD_COUNTS_2026_09_10.json](DOWNLOAD_COUNTS_2026_09_10.json).
Run `python scripts/download_counts.py` to retrieve another snapshot.

| Source | Count | Scope |
| --- | ---: | --- |
| crates.io | 405 | All-time crate downloads |
| npm | 741 | API interval August 8–September 6, 2026 |
| PyPI / PyPI Stats | 876 | Reported last month; also 876 last week |
| GitHub | 59 | Release assets across all 25 releases |
| GitHub 2.0.0 | 13 | Assets for the latest release, included in the 59 |

GitHub's 59 includes 44 executable/library downloads plus 15 checksum/SBOM
downloads. Counts include repeat and automated activity; different periods and
asset types cannot be combined into a unique-user total.

## Deployment state

The new website is live at `https://picovolt.dev/` on the verified Hetzner
hosting server `www727.your-server.de` (`167.235.121.71`). DNS and SSL were already
configured; neither needed a change. The owner signed in and selected the
upload file after the browser extension blocked automatic file selection.

The previous site was archived at `/picovolt-backup-2026-09-10.zip`, outside
`public_html`. The new archive was extracted through WebFTP. Extraction caused
403 responses until recursive web-directory permissions were corrected.
The packager now also emits a WebFTP archive with explicit `0755` directory
entries and `0644` file entries for subsequent deployments.

After the correction, all 37 downloadable public files matched the validated
ZIP byte for byte. HTTPS redirection, the custom 404 body/status, WebAssembly
MIME type, CSP, HSTS, nosniff and asset revalidation checks passed. The local
record is `D:/picovolt-website/artifacts/live-verification.json`. The `.htaccess`
file is configuration; its behavior was verified through response headers.
