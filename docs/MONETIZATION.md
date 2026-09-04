# PicoVolt monetization thesis

PicoVolt should remain useful, permissively licensed, and complete without a paid
account. Revenue should come from operating PicoVolt across a team or fleet—not
from weakening the engine, hiding file-format access, or charging for ordinary
SQL.

This is a hypothesis to validate, not a pricing announcement.

## What stays free

Keep the Apache-2.0 core engine, file format, CLI, language bindings, local
transactions, import/export, browser runtime, and migration tools open source.
That protects the adoption loop: a developer can discover PicoVolt, install it,
ship it, and recommend it without procurement.

Do not use delayed security fixes, proprietary file formats, telemetry, or paid
SQL syntax as monetization levers.

## Who might pay

Individual developers are users; teams operating many databases are buyers. The
strongest initial customer profiles are:

1. teams distributing versioned catalogs, documentation indexes, product data, or
   reference datasets to applications;
2. offline or edge applications that need encrypted backup and controlled
   synchronization;
3. companies embedding PicoVolt in a commercial product that need upgrade help,
   a response SLA, signed builds, or long-term support.

The purchase happens when local files become an operational fleet: releases need
to be built, signed, distributed, observed, restored, and governed.

## Product ladder

### 1. Support and design partnerships — sell now

Offer a small number of paid, founder-led engagements:

- architecture and migration review;
- priority issue triage with a written response target;
- custom binding or import-pipeline work;
- release-readiness and performance review;
- sponsored roadmap work where the result remains open source.

This is the fastest route to learning what organizations value. Publish the
boundaries clearly: support can prioritize diagnosis, but never buys a hidden
security fix or veto over the public roadmap.

### 2. PicoVolt Hub — validate before building

A managed pipeline for immutable and versioned `.pvdb` datasets:

- ingest CSV, JSONL, Parquet, SQLite, or an object-store source;
- build and verify a production image in CI;
- sign releases and retain earlier snapshots;
- distribute files through range-capable edge storage;
- issue scoped download credentials and collect opt-in operational metrics;
- promote, roll back, or expire a dataset release.

This product follows PicoVolt's existing build-and-bake model and does not require
turning the database into a generic hosted SQL service. Public datasets can remain
free and become a discovery channel for the engine.

### 3. PicoVolt Sync — only after the 2.0 change stream

A control plane for encrypted backups and replication across browser, desktop,
edge, and server instances:

- device enrollment and key rotation;
- ordered change upload, restore, and conflict visibility;
- retention policies, audit logs, and fleet health;
- team access control, SSO, private networking, and regional placement.

Do not build sync before the 2.0 transaction/change-stream contract is stable.
Otherwise the commercial product will force an unstable protocol into the core.

### 4. Enterprise assurance

Offer annual contracts for organizations that need:

- an SLA and named support channel;
- long-term supported release branches;
- air-gapped artifacts and reproducible-build evidence;
- security-review assistance, compliance documentation, and dependency policy;
- migration tooling and incident support.

Charge for the assurance and labor, not for a different database format.

## Pricing hypotheses

Comparable products show several viable anchors as verified on 2026-09-04:

- [Turso](https://turso.tech/pricing) ranges from a free plan through low-cost
  developer tiers to usage and enterprise plans, charging around storage, reads,
  writes, sync, retention, and governance.
- [SQLite Cloud](https://sqlitecloud.io/pricing) lists managed database tiers at
  $19 and $79 per month and separate offline-sync tiers at $49 and $149 per month,
  with enterprise capacity sold separately.
- [MotherDuck](https://motherduck.com/product/pricing/) keeps an individual tier
  free and starts its team-oriented Business plan at $250 per organization per
  month plus usage.
- [Cloudflare D1](https://developers.cloudflare.com/d1/platform/pricing/) uses
  generous included read/write/storage allowances followed by usage pricing.

The inference is that PicoVolt should not meter local queries. The cleaner meter
is the service it actually operates: private dataset storage and delivery for
Hub; enrolled devices, transfer, and retention for Sync; seats and response terms
for enterprise support.

Test this packaging rather than announcing it:

| Plan hypothesis | Intended user | Candidate price | What to validate |
|---|---|---:|---|
| Community | Open-source and public datasets | $0 | Does it create successful installs and public examples? |
| Hub Developer | One builder, private datasets | $19/month | Will a developer pay to avoid building a release pipeline? |
| Hub Team | Production releases and collaborators | $79/month | Are access control, rollback, and audit history valuable together? |
| Sync Team | A small production device fleet | $149/month + usage | Is per-device value clearer than per-query pricing? |
| Enterprise | SLA, security, regions, LTS | Annual contract | Is assurance the buying trigger? |

Prices are interview anchors. They are not commitments and should change when
usage costs and willingness-to-pay data exist.

## Ninety-day validation plan

### Days 1–30: learn

- Interview 15 maintainers or teams building offline, embedded, or distributable
  data products.
- Ask for the last concrete incident involving dataset releases, migration,
  backup, sync, or fleet visibility.
- Publish a one-page Hub concept with a waitlist and three packaging questions.
- Offer five free architecture reviews in exchange for permission to document
  anonymized requirements.

Success signal: at least five teams describe the same operational pain and three
agree to provide a representative dataset.

### Days 31–60: sell the manual version

- Run the Hub workflow manually: ingest, bake, verify, sign, host, and roll back.
- Ask design partners to pay for the result before automating it.
- Record time spent, storage/transfer costs, failure modes, and support load.

Success signal: three paying design partners or at least $1,000 in committed
monthly revenue. Interest without willingness to pay is not validation.

### Days 61–90: build the narrow loop

- Automate only source connection, reproducible build, release promotion,
  range-capable delivery, and rollback.
- Add billing after the core workflow succeeds repeatedly.
- Publish one measurable case study: build time, delivered bytes, rollback time,
  and engineering effort avoided.

Success signal: two customers use the workflow for a real production release and
one renews without custom engineering.

## Guardrails and kill criteria

- If interviews consistently ask for ordinary hosted PostgreSQL, do not turn
  PicoVolt into one; refine the embedded/versioned-data niche.
- If customers value support but not Hub, keep a services business while the core
  grows instead of forcing SaaS.
- If Hub cannot cover infrastructure and support at a healthy margin, meter the
  costly delivery/retention dimensions rather than local engine usage.
- If Sync requirements repeatedly demand conflict semantics the core cannot
  guarantee, defer it until the 2.0 change-stream contract is proven.
- Review commercial priorities against open-source adoption quarterly. Revenue
  work should strengthen installation, reliability, and documentation for every
  user.

## First actions

1. Add a short “commercial support / design partner” section to the project site
   only after there is an email or form that someone will answer.
2. Create a 30-minute interview script and recruit from issue reporters,
   dependents, and relevant Rust/embedded communities.
3. Build no billing system until someone has paid for the concierge workflow.
4. Track revenue, activated projects, repeat releases, and renewals alongside
   downloads; none of them substitutes for the others.
