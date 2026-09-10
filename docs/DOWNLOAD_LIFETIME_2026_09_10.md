# Lifetime download counts — September 10, 2026

Checked third-party download viewers in a browser at approximately 13:06–13:08
UTC. These replace the rolling-month npm and Python figures in the earlier
chat when answering the lifetime question.

| Distribution | Reported lifetime downloads | Third-party viewer |
| --- | ---: | --- |
| JavaScript / npm | **3,267** | [npm-stat](https://npm-stat.com/charts.html?package=picovolt&from=2026-06-01&to=2026-09-10) |
| Python / PyPI | **2,345** | [Pepy](https://pepy.tech/projects/picovolt) |
| Rust / crates.io | **405** | [Shields.io total-download badge](https://img.shields.io/crates/d/picovolt.svg) |
| GitHub native release assets | **59** | [Majestic GitHub Release Download Tracker](https://majestic.bot/tools/github-release-downloads?repo=minije%2Fpicovolt) |
| Go modules | **Unavailable** | No complete public lifetime counter established; the checked Goproxy.cn v1/v2 module statistics returned 404 |

The four available counters sum to **6,076 reported download events**. This
is not a count of unique users, installations or active applications. It excludes
unmeasured distribution paths, such as direct Go repository downloads and
website/CDN requests. JavaScript browser and Node installations use the same
npm package and are not counted as separate distributions here.

## Coverage and observations

- **npm-stat:** showed “Total number of downloads between 2026-06-01 and
  2026-09-10” as **3,267**. The npm registry's package metadata records creation
  at `2026-06-22T15:03:31.630Z`, so the selected range begins before publication
  and covers the package's lifetime through the viewer's available data.
  npm-stat states its numbers update at most once daily and come from npm.
- **Pepy:** explicitly showed **2,345 times in total on PyPI**. CI traffic was
  included. Its chart had both “Total” and “1.*” selected and displayed a
  4.69K series sum; that double-counted overlapping series and was not used.
  The package-wide lifetime value is 2,345.
- **Shields.io:** the rendered badge read **downloads: 405**. The endpoint is
  documented as [Crates.io Total Downloads](https://shields.io/badges/crates-io-total-downloads),
  rather than recent or per-version downloads. This agrees with the earlier
  crates.io all-time counter. Lib.rs was also attempted but required a
  Cloudflare challenge; it supplied no usable count.
- **Majestic:** fetched **25 releases** across paginated GitHub pages, with
  filters disabled, and reported **59 total downloads**. Release totals were
  13 for v2.0.0, 2 for v1.9.0, 22 for v1.8.1, 9 for v1.8.0, and 13 for
  v1.7.1; the remaining releases reported zero. This covers release assets,
  including checksum and SBOM files, but not repository clones or GitHub's
  generated source archives. The earlier asset breakdown identified 44
  executable/library downloads and 15 checksum/SBOM downloads.
- **Go:** [Goproxy.cn statistics](https://goproxy.cn/stats) cover only that
  proxy. Both `github.com/!mini!je/picovolt/bindings/go` and its `/v2` statistics
  endpoint returned 404. A missing response does not establish zero downloads.

Third-party viewers reuse upstream registry data and can have different refresh
schedules and filtering. The previous npm **741** (August 8–September 6) and
PyPI Stats **876** (reported last month) were period-specific observations, not
lifetime totals. Pepy and PyPI Stats may also differ in traffic filtering.

The machine-readable observation record is
[DOWNLOAD_LIFETIME_2026_09_10.json](DOWNLOAD_LIFETIME_2026_09_10.json).
