# PicoVolt Hub: build order and acceptance gates

Updated: 2026-09-11. Build a managed release pipeline for immutable `.pvdb`
datasets. The invite-only service is live on Hetzner. No paying customer, price or SLA is claimed.
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

As of 2026-09-11, the hosted private pilot implements operator-issued revocable
account keys, private projects with pinned publisher keys, bounded uploads
verified by the real PicoVolt CLI, authorized artifact delivery, immutable
release identities, conditional channel promotion/rollback and release activity.
Accounts, projects, releases and channels are stored in native PicoVolt 2.0.0.
The application runs on an approved Hetzner CX33 with private disks and HTTPS.

The public PHP status endpoint performs bounded on-request HTTPS/integrity
probes, including the Hub metadata query, and marks stale observations unknown.
It is not independent continuous monitoring. Fifteen integration/storage/client
tests passed on Windows and the Hetzner server. The pilot has no formal SLA.

`scripts/hub_release.py` prepares a local release from an existing signed dataset
and an independently supplied public key. It verifies the copied image with the
existing CLI and records SHA-256 transport digests. It does not upload or issue
entitlements; its unsigned descriptor does not authenticate a publisher.
See [the preparation guide](HUB_RELEASES.md).

Implemented in the private Hub client: bounded whole-file HTTPS download, signature verification and atomic local activation with retained previous files. Keep object storage
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

The deployed pilot includes native PicoVolt metadata, private releases, channels,
backups with restore checks, a redesigned console and verified client activation.
Tamper, interruption, stale-revision and wrong-key tests pass.
Hub 0.4 implements project-scoped, expiring delivery credentials with revocation,
portable workspace exports (original datasets, manifests, channels and activity),
and cursor-based history beyond the old list limits. These remain within the
single-node private pilot. Next implementation priorities: verifier process
isolation, independent failure notifications and documented account recovery. These precede
public signup, billing and storage expansion. Before paid pilot: verify the licensor record, complete contracts/privacy,
meter costs and assign incident/restore ownership. Before general
availability: tenant-isolation review, restore rehearsal, alerts, quotas, abuse
controls and billing cancellation/export.

Interview application teams before expanding. First business gate: one paying
pilot completing two releases and a rollback. If they only need a one-time file
download, do not build a fleet platform. If search blocks adoption, prioritize
that narrow feature. Downloads are not evidence of willingness to pay.


## Next delivery order

Implementation update, 2026-09-11: private Hub 0.5 is deployed on Hetzner with
isolated native verification, workspace switching, account-bound invitations,
publisher/reader roles, single-use recovery codes and license acceptance/order
records. 36 service checks passed on the target host, including a separate
root-only ownership test. The real workspace and restore drill remain healthy.
Paid offers/card checkout are not enabled. Independent SMTP notifications and
encrypted off-host backups are implemented but await production configuration.

The private 2.1.0 engine now implements bounded BM25 full-text search, exact
vector similarity, and SELECT-snapshot retrieval across Rust, C, Python, Go,
JavaScript/WASM and CLI. See [API and bounds](RETRIEVAL_2_1.md) and the
[qualification ledger](RELEASE_2_1.md). Indexes are application-owned and rebuilt
per JSON retrieval call. Production stays on 2.0; 2.1 is not publicly published.

Remaining release order: activate independent alerting and off-host recovery;
complete a replacement-host restore drill; configure reviewed paid offers and
payment/invoice/delivery integration; qualify search relevance/performance at customer scale; then evaluate replication or storage expansion from actual demand.

The original work breakdown below describes the acceptance gates for those areas.

1. **Verifier isolation and failure alerts.** Run untrusted dataset inspection in
   a separate, resource-limited process boundary with no network or access to
   Hub credentials/metadata. Add off-host health and backup-failure notifications
   with an agreed operator destination. Prove a failing verifier and failed
   backup cannot quietly pass. This is the next gate before outside uploads.
2. **Account recovery and team membership.** Verified owner onboarding, recovery,
   owner/publisher/reader roles, removal and an auditable invitation lifecycle.
   Machine delivery keys are implemented separately and do not substitute for
   human identity. Keep private pilots operator-provisioned until this is ready.
3. **License receipts and billing.** Pin the exact issued artifact, terms digest,
   contracting party and acceptance record. Then add invoices, cancellation,
   entitlement checks and a written support scope for the first paid pilot.
4. **Storage resilience.** Add a private off-host recovery destination on
   Hetzner, automate restore drills and exercise loss of the application server.
   Add object storage only when measured delivery/storage demand justifies it.
5. **Engine features driven by a real dataset.** Full-text search for an actual
   catalog/docs corpus comes before vector indexes. Encrypted backups precede
   broader at-rest encryption. Replication/offline sync follow only after gap,
   recovery, conflict and retention contracts have application tests.
