#![cfg(feature = "enterprise")]

use std::sync::{Arc, Mutex};

use picovolt::{
    AuditEvent, AuditEventKind, AuditSink, Database, EnterpriseCapabilities, EnterpriseConfig,
};

#[derive(Default)]
struct EventCollector(Mutex<Vec<AuditEvent>>);

impl AuditSink for EventCollector {
    fn record(&self, event: &AuditEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[test]
fn transaction_events_are_scoped_and_data_minimizing() {
    let sink = Arc::new(EventCollector::default());
    let mut db = Database::open_memory();
    db.configure_enterprise(EnterpriseConfig::new("orders-eu", "production").unwrap());
    db.set_audit_sink(sink.clone());
    db.query("CREATE TABLE orders (id PRIMARY KEY, amount)")
        .unwrap();

    db.transaction(|tx| {
        tx.query("INSERT INTO orders VALUES (1, 25)")?;
        Ok(())
    })
    .unwrap();

    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, AuditEventKind::TransactionBegan);
    assert_eq!(events[1].kind, AuditEventKind::TransactionCommitted);
    assert!(events
        .iter()
        .all(|event| { event.database_id == "orders-eu" && event.environment == "production" }));
    let json = serde_json::to_string(&*events).unwrap();
    assert!(!json.contains("INSERT"));
    assert!(!json.contains("amount"));
}

#[test]
fn capabilities_do_not_claim_unimplemented_enterprise_features() {
    let capabilities = EnterpriseCapabilities::current();
    assert!(capabilities.crash_safe_transactions);
    assert!(capabilities.transaction_audit_events);
    assert!(!capabilities.encryption_at_rest);
    assert!(!capabilities.replication_stream);
    assert!(!capabilities.identity_provider);
}
