//! Stable integration seams for commercial and regulated PicoVolt deployments.
//!
//! This module does not gate engine features or contact a PicoVolt service. It
//! provides versioned, data-minimizing events and capability discovery so a host
//! application can attach its own audit, fleet, backup, or control plane.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::TxId;

/// Schema version carried by every enterprise audit event.
pub const AUDIT_EVENT_SCHEMA_VERSION: u16 = 1;
/// Schema version of [`EnterpriseStatus`].
pub const ENTERPRISE_STATUS_SCHEMA_VERSION: u16 = 1;

/// Host-supplied identity for one deployed database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterpriseConfig {
    /// Stable, non-secret identifier used to correlate events.
    pub database_id: String,
    /// Deployment label such as `development`, `staging`, or `production`.
    pub environment: String,
}

impl EnterpriseConfig {
    /// Validate and construct a deployment identity.
    pub fn new(
        database_id: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self, String> {
        let database_id = database_id.into();
        let environment = environment.into();
        if database_id.trim().is_empty() {
            return Err("database_id must not be empty".into());
        }
        if environment.trim().is_empty() {
            return Err("environment must not be empty".into());
        }
        if database_id.len() > 256 || environment.len() > 64 {
            return Err("enterprise identity exceeds its length limit".into());
        }
        Ok(Self {
            database_id,
            environment,
        })
    }
}

/// Data-minimizing lifecycle events emitted by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditEventKind {
    /// A multi-statement transaction established its rollback point.
    TransactionBegan,
    /// A multi-statement transaction crossed its durable commit point.
    TransactionCommitted,
    /// A multi-statement transaction restored its prior state.
    TransactionRolledBack,
}

/// Versioned event delivered to a host-provided sink.
///
/// Query text, values, filesystem paths, user identities, and secrets are never
/// included. Hosts can enrich the event after it crosses their own trust boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Event payload schema version.
    pub schema_version: u16,
    /// Host-supplied database identity.
    pub database_id: String,
    /// Host-supplied deployment label.
    pub environment: String,
    /// Lifecycle action.
    pub kind: AuditEventKind,
    /// Latest engine transaction id at event emission.
    pub transaction_id: TxId,
}

impl AuditEvent {
    pub(crate) fn pending(kind: AuditEventKind, transaction_id: TxId) -> PendingAuditEvent {
        PendingAuditEvent {
            kind,
            transaction_id,
        }
    }
}

pub(crate) struct PendingAuditEvent {
    kind: AuditEventKind,
    transaction_id: TxId,
}

/// Destination implemented by the embedding application.
///
/// Delivery is synchronous and best-effort in 1.6. A sink must not panic. A
/// compliance archive that requires acknowledged, durable delivery should record
/// these events into its own write-ahead queue.
pub trait AuditSink: Send + Sync {
    /// Observe one event.
    fn record(&self, event: &AuditEvent);
}

/// Capabilities exposed for control-plane negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterpriseCapabilities {
    /// Capability schema version.
    pub schema_version: u16,
    /// Crash-recoverable filesystem transactions are available.
    pub crash_safe_transactions: bool,
    /// Versioned transaction lifecycle events can be attached.
    pub transaction_audit_events: bool,
    /// Encryption at rest is implemented by the core.
    pub encryption_at_rest: bool,
    /// A replication/change-stream protocol is implemented by the core.
    pub replication_stream: bool,
    /// Built-in identity or SSO is implemented by the core.
    pub identity_provider: bool,
}

impl EnterpriseCapabilities {
    /// Capabilities of this engine release.
    pub const fn current() -> Self {
        Self {
            schema_version: 1,
            crash_safe_transactions: true,
            transaction_audit_events: true,
            encryption_at_rest: false,
            replication_stream: false,
            identity_provider: false,
        }
    }
}

/// Offline deployment inventory suitable for a host-owned fleet manager.
///
/// Producing this value performs no I/O and sends no telemetry. The embedding
/// application decides whether, where, and how it is transported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterpriseStatus {
    /// Status payload schema version.
    pub schema_version: u16,
    /// PicoVolt package version that produced this payload.
    pub engine_version: String,
    /// On-disk format written by this engine.
    pub format_version: u16,
    /// Host-supplied deployment identity, when configured.
    pub deployment: Option<EnterpriseConfig>,
    /// Last transaction visible to this handle.
    pub current_transaction_id: TxId,
    /// Capabilities that this build genuinely implements.
    pub capabilities: EnterpriseCapabilities,
}

impl EnterpriseStatus {
    pub(crate) fn current(
        deployment: Option<EnterpriseConfig>,
        current_transaction_id: TxId,
    ) -> Self {
        Self {
            schema_version: ENTERPRISE_STATUS_SCHEMA_VERSION,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            format_version: crate::FORMAT_VERSION,
            deployment,
            current_transaction_id,
            capabilities: EnterpriseCapabilities::current(),
        }
    }
}

/// Per-handle enterprise integration state.
#[derive(Clone, Default)]
pub(crate) struct EnterpriseRuntime {
    config: Option<EnterpriseConfig>,
    sink: Option<Arc<dyn AuditSink>>,
}

impl EnterpriseRuntime {
    pub(crate) fn configure(&mut self, config: EnterpriseConfig) {
        self.config = Some(config);
    }

    pub(crate) fn config(&self) -> Option<&EnterpriseConfig> {
        self.config.as_ref()
    }

    pub(crate) fn set_sink(&mut self, sink: Arc<dyn AuditSink>) {
        self.sink = Some(sink);
    }

    pub(crate) fn emit(&self, pending: PendingAuditEvent) {
        let (Some(config), Some(sink)) = (&self.config, &self.sink) else {
            return;
        };
        sink.record(&AuditEvent {
            schema_version: AUDIT_EVENT_SCHEMA_VERSION,
            database_id: config.database_id.clone(),
            environment: config.environment.clone(),
            kind: pending.kind,
            transaction_id: pending.transaction_id,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collector(Mutex<Vec<AuditEvent>>);

    impl AuditSink for Collector {
        fn record(&self, event: &AuditEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn rejects_empty_or_oversized_identity() {
        assert!(EnterpriseConfig::new("", "production").is_err());
        assert!(EnterpriseConfig::new("db", "").is_err());
        assert!(EnterpriseConfig::new("x".repeat(257), "production").is_err());
    }

    #[test]
    fn runtime_emits_only_after_configuration_and_sink() {
        let sink = Arc::new(Collector::default());
        let mut runtime = EnterpriseRuntime::default();
        runtime.emit(AuditEvent::pending(AuditEventKind::TransactionBegan, 3));
        runtime.set_sink(sink.clone());
        runtime.configure(EnterpriseConfig::new("orders", "production").unwrap());
        runtime.emit(AuditEvent::pending(AuditEventKind::TransactionCommitted, 4));
        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].database_id, "orders");
        assert_eq!(events[0].transaction_id, 4);
    }
}
