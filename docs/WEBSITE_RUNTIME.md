# PicoVolt Web Runtime: internal stability policy

Prepared 2026-09-10. Internal deployment line: `web-stable-2.0.0.1`, initially
using the unchanged verified Apache-2.0 2.0.0 WASM binary. This is a managed
deployment identity, not a new engine release or a change to its license.

## Availability objective

Target **99.99% successful one-minute probes over a rolling 30-day window** for
both site delivery and the stable demo's load/query/export/reopen journey.
Each journey has its own budget; success on one cannot hide failure of the other.
For 43,200 minutes that allows 4.32 minutes of unavailability. At one-minute
resolution, five failed checks exceed the target. Missing checks count as unknown
and must be reported; they cannot support an uptime claim. Scheduled maintenance
counts as downtime. This is an SLO, not an achieved measurement or guarantee.

The current single Hetzner origin has no automatic failover or deployed external
synthetic monitoring. 99.99% is therefore not established. A static browser WASM
runtime removes a database-server dependency, but DNS, TLS, hosting, browser
compatibility and bad deployments can still fail.

## Stable and experimental separation

- Pin exact engine assets and SHA-256 digests in the website runtime manifest.
  Existing demo imports the stable path only. Never load a floating registry/CDN
  version or an experiment based on query strings or browser storage.
- Keep experiments on `/labs.html` and, when real binaries exist, separate
  versioned asset paths and disposable data. No shared production database,
  service worker or storage namespace. Experiments cannot promote themselves.
- Default all experimental flags off. Show “planned” until a genuine build has
  passed feature-specific checks; never present roadmap text as a running feature.
- Internal early builds may precede public releases after review. Internal does
  not mean secret when browser JavaScript/WASM is sent to visitors; protect
  confidential builds behind server authorization and license delivery.

## Release and rollback

Run the real WASM smoke suite, static checks and runtime-digest validation before
every deployment. Stage a complete release, verify it, then activate it atomically
where the host supports directory swaps. WebFTP in-place extraction is not atomic;
it cannot serve as a zero-downtime deployment guarantee. Retain at least the last
two validated bundles and their manifests/backups outside the public directory.

After activation, verify HTTPS, headers, custom 404 and every public file, then
run external browser synthetics. Roll back to the last complete verified release
on engine-load, query or integrity failures. An internal critical-fix branch may
maintain the website runtime independently of public legacy support. Never erase
Apache notices when making internal fixes.

## Longevity and operational work still required

- Proposed minimum retention horizon: five years of build inputs, dependency
  locks, source revisions, tests, notices and deploy/rollback instructions.
  This is an internal planning target, not a funded service commitment.
- Assign primary and backup maintainers, incident response and budget before
  promising customers a support duration or response SLA.
- Provision two independent probe locations and test alert delivery. Use synthetic
  demo data; do not record visitors' SQL or datasets.
- Provision a second static origin and test DNS/CDN failover, TLS renewal and
  recovery. Choose/pay for providers only after infrastructure is specified.
- Monitor certificate/domain renewal, backup integrity and browser/OS support.
  Rehearse restore and review error budgets before admitting risky changes.

No automation, paid infrastructure or uptime measurement has been provisioned
by this policy document. The website manifest and stable/experimental routing
are the implemented first step.
