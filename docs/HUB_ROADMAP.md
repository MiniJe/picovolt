# PicoVolt Hub: build order and acceptance gates

Decision: 2026-09-10. Build a managed release pipeline for immutable `.pvdb`
datasets. No live hosted service, paying customer, price or SLA is claimed.
See [the licensing transition](../legal/TRANSITION.md).

## First customer and smallest useful product

An application team distributes catalogs to browser, desktop or edge clients.
They manually copy files and struggle to identify or roll back bad releases.
Hub should prepare, verify, publish, promote and retrieve immutable versions.
Start with user-prepared `.pvdb` files and existing `pv bake`, `pv inspect` and
`pv dataset sign/verify`. Reuse existing imports and cryptography.

| Priority | Work | Reason and completion evidence |
|---|---|---|
| P0 | License boundary and Hub website | Explain availability; preserve legacy grants; draft terms visible; no false acceptance |
| P0 | Local immutable release preparation | Existing signature verification, digests, refusal to overwrite and corrupt-input rejection |
| P1 | Whole-file HTTPS/object-store delivery | First useful delivery without engine I/O changes; private storage, bounded streaming, exact hash/length, atomic activation |
| P1 | Tenant identity, entitlements and receipts | Required for private data/proprietary binaries; cross-tenant denial tests, scoped credentials, pinned terms and receipt export |
| P1 | Registry and channel promotion/rollback | Unique releases; conditional channel writes; audit trail; previous release retained |
| P1 | One paid manual pilot | Representative dataset, repeated release, rollback, measured cost and written support scope |
| P2 | Range reads and caching | Only when whole-file delivery proves costly; strict Content-Range/ETag, no mixed versions, bounded memory and interrupted-fetch recovery |
| P2 | Full-text search | Promote for an actual catalog/docs customer; relevance corpus, index-size/update and latency budgets |
| P2 | Encrypted backups, then at-rest storage | Customer sensitivity may raise priority; key custody/rotation, journal/temp/backup coverage and independent review |
| P3 | One-way change-stream replication | One writer; prove duplicates, gaps, restart, expired-cursor resnapshot and durable acknowledgement |
| P4 | Bidirectional offline sync | Requires deletion/conflict/schema/device-identity semantics and long-offline recovery |
| P4 | Vector indexes | Wait for a customer corpus with recall/latency/memory targets |
| Demand-led | Additional adapters | Named adopter, platform CI and maintenance owner before expansion |

## First implementation

`scripts/hub_release.py` prepares a local release from an existing signed dataset
and an independently supplied public key. It verifies the copied image with the
existing CLI and records SHA-256 transport digests. It does not upload or issue
entitlements; its unsigned descriptor does not authenticate a publisher.
See [the preparation guide](HUB_RELEASES.md).

Next: bounded whole-file download and atomic local activation. Keep object storage
separate from mutable database persistence. Accept operator-provided files before
remote ingestion; fetching arbitrary server-side URLs requires SSRF/isolation
controls. Do not put signing keys or customer data in the static website.

## Service architecture and gates

1. Isolated build worker imports/bakes/inspects/signs under resource budgets.
2. Private object storage holds immutable images and signed manifests. Hetzner
   static hosting serves the website and documentation.
3. A separately provisioned API owns tenants, release metadata, acceptance receipts
   and authorization. Private access is never simulated with browser state.
4. Compare-and-swap channel pointers support audited promotion and authorized
   rollback. Clients pin expected identity/version, digest and trusted publisher
   key; a valid signature alone does not prevent replay.

First iteration delivers local preparation, license draft and Hub pages. Next
iteration proves one provider/client path, including tamper and interruption
tests. Before paid pilot: identify seller, complete contracts/privacy, provision
API/storage, meter costs and assign incident/restore ownership. Before general
availability: tenant-isolation review, restore rehearsal, alerts, quotas, abuse
controls and billing cancellation/export.

Interview application teams before expanding. First business gate: one paying
pilot completing two releases and a rollback. If they only need a one-time file
download, do not build a fleet platform. If search blocks adoption, prioritize
that narrow feature. Downloads are not evidence of willingness to pay.
