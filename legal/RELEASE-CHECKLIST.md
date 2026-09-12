# Public 2.2.0 release checklist

The owner selected public source and free downloads on 12 September 2026.
The controlling terms are root LICENSE (PicoVolt Public-Source License 1.1)
and legal/PUBLIC-RELEASE.json. The unissued 1.0 template is not used for this
release. Public downloads need no checkout, account, receipt or acceptance server.

- Verify source, native, WASM and binding tests with the release compiler.
- Verify version parity, license digests, prior Apache notices and dependency notices.
- Confirm public-registry metadata and exact reproducible npm/Go starter hashes.
- Run hosted CI and platform builds, then publish immutable versioned artifacts.
- Verify exact-version clean registry installs before creating the GitHub Release.
- Report independent audit and platform qualification limits without implying
  legal clearance or security certification from automated tests.

## Historical private-offer process (unissued edition 1.0)

# Issue a PicoVolt proprietary release

Edition 1.0 is prepared. BEYOND SOFTWARE S.R.L. is identified as the licensor. The covered proprietary
release and commercial offer are not yet issued, so acceptance is not enabled. The live Hub pilot
and Apache-2.0 engine remain separate from this future engine license.

## Decisions captured in the terms

- Perpetual offline use of accepted versions, with no recurring activation.
- Commercial embedding and application SaaS permitted.
- Standalone engine resale and general-purpose competing database/distribution
  services require a separate agreement, scoped to proprietary components.
- Customer data and application code remain the customer's.
- No retroactive limits, accounts or telemetry for old Apache releases.
- Future updates, support and Hub subscriptions are separate purchases.
- A material breach has written notice and a 30-day cure period. Mandatory
  statutory rights and end-user grants are preserved.

## Facts needed to issue an offer

Complete `RELEASE-OFFER.template.json` with the actual legal licensor, customer
scope, identified proprietary components and exact artifact. Confirm the rights
inventory rather than inferring ownership from Git authorship. State a one-time
price (including zero where intended) and tax treatment. Select support scope.

Run `python scripts/prepare-license-offer.py OFFER.json` to validate the record
and bind it to the exact terms digest. This creates an immutable offer record;
it does not publish a binary, collect acceptance or silently change any license.
Review the terms with a qualified lawyer for the licensor's actual markets.

## Delivery requirements

Before a proprietary download, show the legal party, version, components, price,
terms and separate service conditions. Use an unchecked acceptance box and an
explicit action. Record the accepting person/organization, authority, timestamp,
offer digest and terms digest server-side. Give the customer a downloadable
receipt and the accepted terms. CI credentials follow administrator acceptance.
Do not interpret an ordinary download of PicoVolt 2.0 as contract acceptance.

Consumer sales additionally need statutory disclosures and any separately
required immediate-delivery consent. No consumer checkout is enabled by this
document. No paid offering is activated until the commercial record is complete.

## Sources reviewed

- [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0): prior grants and notices.
- [EU contract information](https://europa.eu/youreurope/citizens/consumers/shopping/contract-information/index_en.htm): pre-contract information and durable confirmation.
- [EU digital content rules](https://eur-lex.europa.eu/eli/dir/2019/770/oj): mandatory consumer protections.
