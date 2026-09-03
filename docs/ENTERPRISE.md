# Enterprise integration foundation

PicoVolt's Apache-2.0 engine remains complete without an account, license key,
network connection, or proprietary service. The optional Cargo `enterprise`
feature provides stable seams for teams that operate PicoVolt as part of a
larger fleet or regulated system; it does not unlock database functionality.

Enable it with:

```toml
[dependencies]
picovolt = { version = "1.6", features = ["enterprise"] }
```

## What 1.6 provides

- `EnterpriseConfig` assigns a host-controlled database identifier and
  environment label.
- `AuditSink` receives versioned transaction lifecycle events.
- `EnterpriseCapabilities` lets a future control plane negotiate only features
  the engine actually implements.

Audit events deliberately exclude SQL text, row values, filesystem paths,
secrets, and user identity. The embedding application owns transport, retention,
identity enrichment, redaction, and access control.

```rust
use picovolt::{AuditEvent, AuditSink, Database, EnterpriseConfig};
use std::sync::Arc;

struct LogSink;

impl AuditSink for LogSink {
    fn record(&self, event: &AuditEvent) {
        eprintln!("{}: {:?}", event.database_id, event.kind);
    }
}

let mut db = Database::open_memory();
db.configure_enterprise(EnterpriseConfig::new("orders-eu", "production").unwrap());
db.set_audit_sink(Arc::new(LogSink));
db.begin_transaction()?;
db.query("CREATE TABLE orders (id PRIMARY KEY, total)")?;
db.commit_transaction()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Delivery is synchronous and best-effort in 1.6. A deployment that needs
acknowledged audit durability should make its sink append to a host-owned durable
queue before returning.

## Explicitly not implemented yet

The capability flags for encryption at rest, replication/change streams, and an
identity provider are `false`. A future enterprise product can build against
these versioned seams, but documentation and sales material must not represent
those capabilities as shipped.

Likely future layers are a signed dataset build/distribution service, encrypted
backup and restore, fleet inventory, and enterprise support. Replication and
sync remain downstream of the 2.0 ordered change-stream contract described in
[ROADMAP.md](../ROADMAP.md). The commercial hypothesis and open-core boundary are
documented in [MONETIZATION.md](MONETIZATION.md).
