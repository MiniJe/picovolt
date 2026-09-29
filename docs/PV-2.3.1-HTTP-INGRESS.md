# PV-2.3.1 HTTP ingress qualification

This patch addresses the confirmed A7/HIGH availability finding in
[PR #42](https://github.com/MiniJe/picovolt/pull/42) and
`docs/PV-2.3-HARDENING-REPORT.md`. It changes only the HTTP server boundary;
the database file format and Rust 1.86 MSRV remain unchanged.

## Pre-fix reproduction

The unmodified `github/main` release build was run in `rust:1.86` with
`PICOVOLT_SERVER_TOKEN=audit-test`. Measurements came from `/proc/1/status` in
the server container while raw TCP sockets held unfinished headers open:

| Probe | Threads | VmRSS |
| --- | ---: | ---: |
| Idle | 23 | 3,292 KiB |
| 128 unfinished headers | 147 | 5,764 KiB |
| Plus one unfinished 4 MiB header | 148 | 10,012 KiB |

`tiny_http` 0.12 parses request lines and headers into an unrestricted buffer
and expands its parsing thread pool before PicoVolt receives a request. The
application's bearer check and 1 MiB query body limit therefore cannot stop
the A7 path. The server now owns the TCP accept and HTTP/1.x parsing boundary.

## Ingress contract

| Resource | Bound / behavior |
| --- | --- |
| Request head | 16 KiB total, including request line and headers |
| Request line | 4 KiB |
| Header count | 64 |
| Incomplete head | 5 second absolute deadline from worker admission; HTTP 408 |
| Simultaneous active connections | 32 fixed workers |
| Waiting accepted connections | 32 socket queue; additional accepted sockets receive HTTP 503 |
| Body | Existing 1 MiB query limit retained; 30 second absolute read deadline |
| Connection lifecycle | One request per connection, explicit `Connection: close`; socket closes on response, rejection, or timeout |

Malformed framing, duplicate content lengths, and conflicting transfer
encodings are rejected. Normal authenticated JSON requests, including chunked
transfer encoding and `Expect: 100-continue`, continue to work. Query size,
Origin, content type, authentication, response size, engine queue, and query
timeouts remain in place.

`scripts/verify_http_ingress.py` runs a real server with a token, repeats the
A7 socket pressure, and checks thread, resident-memory, and file-descriptor
ceilings. It also checks overload, timeout cleanup, a subsequent healthy
request, allowed and oversized bodies, and the existing HTTP gates. Linux CI
runs it after building the server binary.

Final Rust 1.86 Linux run against the patched binary:

| Probe | Threads | VmRSS | Open descriptors |
| --- | ---: | ---: | ---: |
| Idle | 34 | 5,392 KiB | 4 |
| Before unfinished 4 MiB header, after functional probes | 34 | 6,864 KiB | 4 |
| After unfinished 4 MiB header | 34 | 6,884 KiB | 4 |
| 96 unfinished connections | 34 | 7,184 KiB | 68 |
| After timeout and socket cleanup | 34 | 7,184 KiB | 4 |

The 4 MiB header changed process RSS by only 20 KiB. Of 96 unfinished
connections, 32 received HTTP 503 overload responses; active and waiting
connections stayed within their fixed caps. The script observed HTTP 408 after five seconds
for both idle and byte-trickle incomplete headers, HTTP 431 for oversized
request lines/headers and excess header count, and HTTP 200 for a healthy
authenticated request after the attack.

## Qualification run

- Rust 1.86: `cargo test --locked --all-targets --all-features`, strict Clippy,
  `cargo fmt --all -- --check`, `cargo check --locked --all-features`, server-only
  build and Clippy, and two doctests passed.
- Native C ABI smoke passed; Python ctypes tests 12/12 passed; Go test and vet
  passed on 1.26 and 1.27.
- The seven JS/WASM integration tests passed against the published
  `picovolt@2.3.0` npm archive. This checks the existing release artifact,
  not a fresh local WASM rebuild.
- The 32 policy tests passed with the all-feature CLI present; the registry
  starter policy, public release notice check, workflow actionlint, and
  `cargo audit` passed.

## Version metadata

This code PR does not tag or publish. `scripts/check_public_release.py` and the
release workflow require Cargo, Python, and `legal/PUBLIC-RELEASE.json` versions
to agree before a `v2.3.1` tag can be published. The current notice records an
already published and specifically authorized 2.3.0 release, so it must not be
relabeled as 2.3.1 in this code patch. Release preparation must create accurate
2.3.1 candidate metadata and complete the version-parity gate before tagging.
