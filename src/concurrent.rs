//! Native multi-task access to PicoVolt's single-threaded engine.
//!
//! [`SharedDatabase`] is a cloneable, `Send + Sync` coordinator. One worker
//! thread owns the underlying [`Database`], while callers submit work through a
//! bounded FIFO channel. Writers have exclusive admission; explicit readers
//! execute concurrently on private, bounded immutable snapshots. Filesystem
//! writes use an incremental page journal and expose ordered durable changes.

use std::any::Any;
use std::cell::Cell;
use std::io::Write;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::engine::query::{bind_params, parse, Statement};
use crate::{CancellationToken, Database, PvError, QueryLimits, QueryResult, Result, TxId, Value};

/// Default number of accepted operations that may wait behind the worker.
pub const DEFAULT_SHARED_QUEUE_CAPACITY: usize = 64;
const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Construction options for [`SharedDatabase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedDatabaseOptions {
    queue_capacity: usize,
    max_readers: usize,
    max_snapshot_bytes: u64,
    commit_log: crate::CommitLogOptions,
}

impl SharedDatabaseOptions {
    /// Use a bounded command queue with room for `queue_capacity` waiting
    /// operations. A zero value is normalized to one.
    pub const fn new(queue_capacity: usize) -> Self {
        Self {
            queue_capacity: if queue_capacity == 0 {
                1
            } else {
                queue_capacity
            },
            max_readers: 16,
            max_snapshot_bytes: 256 * 1024 * 1024,
            commit_log: crate::CommitLogOptions {
                max_transaction_bytes: 64 * 1024 * 1024,
                max_retained_bytes: 256 * 1024 * 1024,
                max_retained_commits: 4096,
            },
        }
    }

    /// Number of operations that may wait behind the worker.
    pub const fn queue_capacity(self) -> usize {
        self.queue_capacity
    }

    /// Maximum live snapshot workers and bytes per private snapshot image.
    /// Limits reject admission rather than evicting a caller's pinned view.
    pub const fn with_snapshot_limits(mut self, max_readers: usize, max_bytes: u64) -> Self {
        self.max_readers = max_readers;
        self.max_snapshot_bytes = max_bytes;
        self
    }

    pub const fn with_commit_log(mut self, options: crate::CommitLogOptions) -> Self {
        self.commit_log = options;
        self
    }

    fn effective_queue_capacity(self) -> usize {
        self.queue_capacity
    }
}

impl Default for SharedDatabaseOptions {
    fn default() -> Self {
        Self::new(DEFAULT_SHARED_QUEUE_CAPACITY)
    }
}

/// Per-operation resource and cancellation controls.
///
/// Limits apply to each SQL statement independently. The deadline also bounds
/// the lifetime of an explicit transaction session. Cancellation is observed
/// before queued work starts, while a session is idle, between statements, and
/// at bounded-query checkpoints.
#[derive(Clone, Debug)]
pub struct RequestOptions {
    limits: QueryLimits,
    cancellation: CancellationToken,
}

impl RequestOptions {
    /// Build request options with explicit query limits.
    pub fn new(limits: QueryLimits) -> Self {
        Self {
            limits,
            cancellation: CancellationToken::new(),
        }
    }

    /// Attach a token that another task may use to cancel this operation.
    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Replace the per-statement query limits.
    pub fn with_limits(mut self, limits: QueryLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The configured per-statement query limits.
    pub const fn limits(&self) -> QueryLimits {
        self.limits
    }

    /// The cooperative cancellation token for this request.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    fn check(&self) -> Result<()> {
        self.cancellation.check()?;
        if self
            .limits
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(PvError::ResourceLimit("deadline expired".into()));
        }
        Ok(())
    }

    fn merged_limits(&self, requested: QueryLimits) -> QueryLimits {
        QueryLimits::new(
            self.limits.max_rows_scanned.min(requested.max_rows_scanned),
            self.limits
                .max_materialized_bytes
                .min(requested.max_materialized_bytes),
            self.limits.max_result_rows.min(requested.max_result_rows),
            earliest_deadline(self.limits.deadline, requested.deadline),
        )
    }
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self::new(unbounded_limits())
    }
}

type Reply<T> = SyncSender<Result<T>>;

enum Command {
    Checkpoint {
        destination: std::path::PathBuf,
        reply: Reply<crate::SnapshotCheckpoint>,
    },
    Compact {
        max_pages: usize,
        reply: Reply<crate::CompactionReport>,
    },
    Changes {
        after: u64,
        limit: usize,
        reply: Reply<Vec<crate::ChangeCommit>>,
    },
    Prune {
        through: u64,
        reply: Reply<()>,
    },
    Query {
        sql: String,
        params: Vec<Value>,
        options: RequestOptions,
        reply: Reply<QueryResult>,
    },
    CurrentTx {
        reply: Reply<TxId>,
    },
    IsWritable {
        reply: Reply<bool>,
    },
    BeginRead {
        options: RequestOptions,
        reply: Reply<ReadSessionStart>,
    },
    BeginWrite {
        options: RequestOptions,
        reply: Reply<WriteSessionStart>,
    },
}

struct ReadSessionStart {
    sender: SyncSender<ReadCommand>,
    snapshot_tx: TxId,
}

enum ReadCommand {
    Query {
        sql: String,
        params: Vec<Value>,
        limits: QueryLimits,
        reply: Reply<QueryResult>,
    },
    Close {
        reply: Reply<()>,
    },
}

struct WriteSessionStart {
    sender: SyncSender<WriteCommand>,
}

enum WriteCommand {
    Query {
        sql: String,
        params: Vec<Value>,
        limits: QueryLimits,
        reply: Reply<QueryResult>,
    },
    Commit {
        reply: Reply<()>,
    },
    Rollback {
        reply: Reply<()>,
    },
}

struct SharedInner {
    sender: Mutex<Option<SyncSender<Command>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for SharedInner {
    fn drop(&mut self) {
        let sender = self
            .sender
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        drop(sender);

        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            // Dropping a JoinHandle detaches it. This keeps handle Drop
            // non-blocking; disconnecting the channels still makes the worker
            // roll back any live write and exit in the background.
            drop(worker);
        }
    }
}

/// A cloneable native handle for safely sharing one PicoVolt database across
/// application threads or tasks.
///
/// Accepted operations execute in FIFO admission order on one worker thread. A
/// write transaction is one admitted operation and owns that worker until it commits,
/// rolls back, expires, is cancelled, or its handle is dropped. Submission is
/// non-blocking: a full bounded queue returns [`PvError::Busy`]. Once admitted,
/// the calling thread waits for a terminal response.
/// Read transactions release this worker after their private snapshot is built.
///
/// This type is not available on `wasm32`; browser callers already use a Web
/// Worker ownership boundary.
#[derive(Clone)]
pub struct SharedDatabase {
    inner: Arc<SharedInner>,
}

impl SharedDatabase {
    /// Open a fresh in-memory database with the default queue capacity.
    pub fn open_memory() -> Result<Self> {
        Self::open_memory_with_options(SharedDatabaseOptions::default())
    }

    /// Open a fresh in-memory database with explicit coordinator options.
    pub fn open_memory_with_options(options: SharedDatabaseOptions) -> Result<Self> {
        Self::spawn(|| Ok(Database::open_memory()), options)
    }

    /// Open or create a development workspace with the default queue capacity.
    pub fn open_dev(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_dev_with_options(path, SharedDatabaseOptions::default())
    }

    /// Open or create a development workspace with explicit coordinator
    /// options.
    pub fn open_dev_with_options(
        path: impl AsRef<Path>,
        options: SharedDatabaseOptions,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::spawn(
            move || {
                let mut database = Database::open_dev(path)?;
                database.enable_commit_log(options.commit_log)?;
                Ok(database)
            },
            options,
        )
    }

    /// Open an immutable production image with the default queue capacity.
    pub fn open_prod(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_prod_with_options(path, SharedDatabaseOptions::default())
    }

    /// Open an immutable production image with explicit coordinator options.
    pub fn open_prod_with_options(
        path: impl AsRef<Path>,
        options: SharedDatabaseOptions,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::spawn(move || Database::open_prod(path), options)
    }

    fn spawn(
        open: impl FnOnce() -> Result<Database> + Send + 'static,
        options: SharedDatabaseOptions,
    ) -> Result<Self> {
        options.commit_log.validate()?;
        if options.max_readers == 0 || options.max_readers > 1024 || options.max_snapshot_bytes == 0
        {
            return Err(PvError::ResourceLimit("invalid snapshot limits".into()));
        }
        let (sender, receiver) = mpsc::sync_channel(options.effective_queue_capacity());
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("picovolt-shared".into())
            .spawn(move || {
                let opened = catch_unwind(AssertUnwindSafe(open));
                match opened {
                    Ok(Ok(mut database)) => {
                        if ready_sender.send(Ok(())).is_err() {
                            return;
                        }
                        run_worker(&receiver, &mut database, options);
                        if database.in_transaction() {
                            let _ = safe_rollback(&mut database);
                        }
                    }
                    Ok(Err(error)) => {
                        let _ = ready_sender.send(Err(error));
                    }
                    Err(panic) => {
                        let _ = ready_sender.send(Err(PvError::Transaction(format!(
                            "shared database panicked while opening: {}",
                            panic_payload(panic)
                        ))));
                    }
                }
            })?;

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(SharedInner {
                    sender: Mutex::new(Some(sender)),
                    worker: Mutex::new(Some(worker)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(PvError::DatabaseClosed)
            }
        }
    }

    /// Execute one SQL statement with unbounded query limits.
    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        self.query_with_options(sql, &[], RequestOptions::default())
    }

    /// Execute one parameterized SQL statement with unbounded query limits.
    pub fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.query_with_options(sql, params, RequestOptions::default())
    }

    /// Execute one parameterized SQL statement with explicit limits and
    /// cancellation.
    ///
    /// Transaction-control SQL is rejected because its lifetime would span
    /// independent queue entries. Use [`begin_write`](Self::begin_write) for
    /// multi-statement work. A one-shot mutation runs inside an explicit
    /// rollback boundary, so cancellation or failure cannot publish partial work.
    pub fn query_with_options(
        &self,
        sql: &str,
        params: &[Value],
        options: RequestOptions,
    ) -> Result<QueryResult> {
        let sql = sql.to_owned();
        let params = params.to_vec();
        self.request(|reply| Command::Query {
            sql,
            params,
            options,
            reply,
        })
    }

    /// Begin a snapshot-stable, read-only transaction using default request
    /// options.
    pub fn begin_read(&self) -> Result<ReadTransaction> {
        self.begin_read_with_options(RequestOptions::default())
    }

    /// Begin a snapshot-stable, read-only transaction. No writer or other read
    /// transaction executes until this handle closes or is dropped.
    pub fn begin_read_with_options(&self, options: RequestOptions) -> Result<ReadTransaction> {
        let handle_options = options.clone();
        let start = self.request(|reply| Command::BeginRead { options, reply })?;
        Ok(ReadTransaction {
            sender: Some(start.sender),
            snapshot_tx: start.snapshot_tx,
            options: handle_options,
            _owner: Arc::clone(&self.inner),
            not_sync: PhantomData,
        })
    }

    /// Begin an exclusive write transaction using default request options.
    pub fn begin_write(&self) -> Result<WriteTransaction> {
        self.begin_write_with_options(RequestOptions::default())
    }

    /// Begin an exclusive write transaction. It commits or rolls back only
    /// through the returned handle; dropping the handle rolls it back.
    pub fn begin_write_with_options(&self, options: RequestOptions) -> Result<WriteTransaction> {
        let handle_options = options.clone();
        let start = self.request(|reply| Command::BeginWrite { options, reply })?;
        Ok(WriteTransaction {
            sender: Some(start.sender),
            options: handle_options,
            _owner: Arc::clone(&self.inner),
            not_sync: PhantomData,
        })
    }

    /// Return the latest committed MVCC transaction id.
    pub fn current_tx(&self) -> Result<TxId> {
        self.request(|reply| Command::CurrentTx { reply })
    }

    /// Read a bounded batch of durable changes after an exclusive sequence cursor.
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<Vec<crate::ChangeCommit>> {
        self.request(|reply| Command::Changes {
            after,
            limit,
            reply,
        })
    }

    /// Prune commits acknowledged by all consumers. Old cursors then fail explicitly.
    pub fn prune_changes(&self, through: u64) -> Result<()> {
        self.request(|reply| Command::Prune { through, reply })
    }

    /// Queue maintenance with the same writer ordering and journal guarantees.
    pub fn compact_step(&self, max_pages: usize) -> Result<crate::CompactionReport> {
        self.request(|reply| Command::Compact { max_pages, reply })
    }

    /// Export and verify a committed image together with its exact following
    /// change cursor. The writer queue stays reserved for the complete export.
    pub fn export_checkpoint(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<crate::SnapshotCheckpoint> {
        let destination = destination.as_ref().to_path_buf();
        self.request(|reply| Command::Checkpoint { destination, reply })
    }

    /// Deliver a batch outside the database worker. A sink error never reverses
    /// a durable commit. Consumers must deduplicate sequences when retrying.
    pub fn visit_changes(
        &self,
        after: u64,
        limit: usize,
        sink: &mut impl crate::ChangeSink,
    ) -> Result<u64> {
        let mut cursor = after;
        for change in self.changes_since(after, limit)? {
            sink.accept(&change)?;
            cursor = change.sequence;
        }
        Ok(cursor)
    }

    /// Whether the underlying database accepts mutations.
    pub fn is_writable(&self) -> Result<bool> {
        self.request(|reply| Command::IsWritable { reply })
    }

    fn request<T>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Result<T> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let command = build(reply_sender);
        let sender_guard = self
            .inner
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(sender) = sender_guard.as_ref() else {
            return Err(PvError::DatabaseClosed);
        };
        match sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(PvError::Busy(
                    "the bounded shared-database queue is full".into(),
                ))
            }
            Err(TrySendError::Disconnected(_)) => return Err(PvError::DatabaseClosed),
        }
        drop(sender_guard);
        reply_receiver.recv().map_err(|_| PvError::DatabaseClosed)?
    }
}

/// An explicit, snapshot-stable read transaction.
///
/// Read transactions are movable between threads but deliberately not `Sync`;
/// one caller issues one operation at a time through `&mut self`.
pub struct ReadTransaction {
    sender: Option<SyncSender<ReadCommand>>,
    snapshot_tx: TxId,
    options: RequestOptions,
    _owner: Arc<SharedInner>,
    not_sync: PhantomData<Cell<()>>,
}

impl ReadTransaction {
    /// The latest committed transaction visible when this read began.
    pub const fn snapshot_tx(&self) -> TxId {
        self.snapshot_tx
    }

    /// Execute a read-only statement with the transaction's limits.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.query_with(sql, &[])
    }

    /// Execute a parameterized read-only statement with the transaction's
    /// limits.
    pub fn query_with(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.query_with_limits(sql, params, self.options.limits)
    }

    /// Execute a parameterized read-only statement with limits no weaker than
    /// the transaction's original limits.
    pub fn query_with_limits(
        &mut self,
        sql: &str,
        params: &[Value],
        limits: QueryLimits,
    ) -> Result<QueryResult> {
        self.options.check()?;
        let limits = self.options.merged_limits(limits);
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let command = ReadCommand::Query {
            sql: sql.to_owned(),
            params: params.to_vec(),
            limits,
            reply: reply_sender,
        };
        let sender = self.sender.as_ref().ok_or(PvError::DatabaseClosed)?;
        sender.send(command).map_err(|_| self.session_error())?;
        reply_receiver.recv().map_err(|_| self.session_error())?
    }

    /// End the read transaction and release the worker for the next admitted
    /// operation.
    pub fn close(mut self) -> Result<()> {
        let sender = self.sender.take().ok_or(PvError::DatabaseClosed)?;
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        sender
            .send(ReadCommand::Close {
                reply: reply_sender,
            })
            .map_err(|_| self.session_error())?;
        drop(sender);
        reply_receiver.recv().map_err(|_| self.session_error())?
    }

    fn session_error(&self) -> PvError {
        self.options
            .check()
            .err()
            .unwrap_or(PvError::DatabaseClosed)
    }
}

impl Drop for ReadTransaction {
    fn drop(&mut self) {
        // Disconnecting is intentionally non-blocking. The worker treats it as
        // an end-of-session signal and resumes the public queue.
        self.sender.take();
    }
}

/// An explicit exclusive write transaction.
///
/// A statement error, cancellation, deadline, panic, or dropped handle aborts
/// the complete transaction before the worker accepts another operation.
pub struct WriteTransaction {
    sender: Option<SyncSender<WriteCommand>>,
    options: RequestOptions,
    _owner: Arc<SharedInner>,
    not_sync: PhantomData<Cell<()>>,
}

impl WriteTransaction {
    /// Execute a statement with the transaction's limits.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.query_with(sql, &[])
    }

    /// Execute a parameterized statement with the transaction's limits.
    pub fn query_with(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.query_with_limits(sql, params, self.options.limits)
    }

    /// Execute a parameterized statement with limits no weaker than the
    /// transaction's original limits.
    pub fn query_with_limits(
        &mut self,
        sql: &str,
        params: &[Value],
        limits: QueryLimits,
    ) -> Result<QueryResult> {
        self.options.check()?;
        let limits = self.options.merged_limits(limits);
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let command = WriteCommand::Query {
            sql: sql.to_owned(),
            params: params.to_vec(),
            limits,
            reply: reply_sender,
        };
        let sender = self.sender.as_ref().ok_or(PvError::TransactionClosed)?;
        sender.send(command).map_err(|_| self.session_error())?;
        let result = reply_receiver.recv().map_err(|_| self.session_error())?;
        if result.is_err() {
            self.sender.take();
        }
        result
    }

    /// Commit this transaction and release the worker.
    pub fn commit(mut self) -> Result<()> {
        self.finish(WriteCommandKind::Commit)
    }

    /// Roll this transaction back and release the worker.
    pub fn rollback(mut self) -> Result<()> {
        self.finish(WriteCommandKind::Rollback)
    }

    fn finish(&mut self, kind: WriteCommandKind) -> Result<()> {
        let sender = self.sender.take().ok_or(PvError::TransactionClosed)?;
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        let command = match kind {
            WriteCommandKind::Commit => WriteCommand::Commit {
                reply: reply_sender,
            },
            WriteCommandKind::Rollback => WriteCommand::Rollback {
                reply: reply_sender,
            },
        };
        sender.send(command).map_err(|_| self.session_error())?;
        drop(sender);
        reply_receiver.recv().map_err(|_| self.session_error())?
    }

    fn session_error(&self) -> PvError {
        self.options
            .check()
            .err()
            .unwrap_or(PvError::TransactionClosed)
    }
}

impl Drop for WriteTransaction {
    fn drop(&mut self) {
        // Disconnecting is intentionally non-blocking. The worker rolls back
        // before it resumes the public queue.
        self.sender.take();
    }
}

enum WriteCommandKind {
    Commit,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatementClass {
    Read,
    Write,
    Control,
}

fn run_worker(
    receiver: &Receiver<Command>,
    database: &mut Database,
    config: SharedDatabaseOptions,
) {
    let readers = Arc::new(AtomicUsize::new(0));
    while let Ok(command) = receiver.recv() {
        let keep_running = match command {
            Command::Checkpoint { destination, reply } => {
                administrative_call(|| database.export_checkpoint(destination), reply)
            }
            Command::Compact { max_pages, reply } => {
                administrative_call(|| database.compact_step(max_pages), reply)
            }
            Command::Changes {
                after,
                limit,
                reply,
            } => {
                let _ = reply.send(database.changes_since(after, limit));
                true
            }
            Command::Prune { through, reply } => {
                let _ = reply.send(database.prune_changes(through));
                true
            }
            Command::Query {
                sql,
                params,
                options,
                reply,
            } => execute_top_level(database, sql, params, options, reply),
            Command::CurrentTx { reply } => {
                let _ = reply.send(Ok(database.current_tx()));
                true
            }
            Command::IsWritable { reply } => {
                let _ = reply.send(Ok(database.is_writable()));
                true
            }
            Command::BeginRead { options, reply } => {
                begin_read_session(database, options, reply, &readers, config)
            }
            Command::BeginWrite { options, reply } => begin_write_session(database, options, reply),
        };
        if !keep_running {
            break;
        }
    }
}

fn administrative_call<T>(operation: impl FnOnce() -> Result<T>, reply: Reply<T>) -> bool {
    match call_database(operation) {
        DatabaseCall::Ok(value) => {
            let _ = reply.send(Ok(value));
            true
        }
        DatabaseCall::Error(error) => {
            let safe = !outcome_is_unknown(&error);
            let _ = reply.send(Err(error));
            safe
        }
        DatabaseCall::Panicked(error) => {
            let _ = reply.send(Err(error));
            false
        }
    }
}

fn execute_top_level(
    database: &mut Database,
    sql: String,
    params: Vec<Value>,
    options: RequestOptions,
    reply: Reply<QueryResult>,
) -> bool {
    if let Err(error) = options.check() {
        let _ = reply.send(Err(error));
        return true;
    }
    let class = match classify_sql(&sql, &params) {
        Ok(class) => class,
        Err(error) => {
            let _ = reply.send(Err(error));
            return true;
        }
    };
    let (result, safe) = match class {
        StatementClass::Control => (
            Err(PvError::Transaction(
                "transaction-control SQL is not accepted by SharedDatabase; use begin_write".into(),
            )),
            true,
        ),
        StatementClass::Read => match call_database(|| {
            database.query_with_limits_cancellable(
                &sql,
                &params,
                options.limits,
                Some(options.cancellation.clone()),
            )
        }) {
            DatabaseCall::Ok(value) => (Ok(value), true),
            DatabaseCall::Error(error) => {
                let safe = !outcome_is_unknown(&error);
                (Err(error), safe)
            }
            DatabaseCall::Panicked(error) => (Err(error), false),
        },
        StatementClass::Write => run_write(database, &options, |database| {
            database.query_with_limits_cancellable(
                &sql,
                &params,
                options.limits,
                Some(options.cancellation.clone()),
            )
        }),
    };
    let _ = reply.send(result);
    safe
}

struct ReaderPermit(Arc<AtomicUsize>);
impl Drop for ReaderPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct SnapshotWriter<'a> {
    file: &'a mut dyn Write,
    remaining: u64,
    options: &'a RequestOptions,
}

enum ReadImage {
    Memory(Vec<u8>),
    File(tempfile::NamedTempFile),
}
impl ReadImage {
    fn writer(&mut self) -> &mut dyn Write {
        match self {
            Self::Memory(bytes) => bytes,
            Self::File(file) => file.as_file_mut(),
        }
    }
    fn open(&self) -> Result<Database> {
        match self {
            Self::Memory(bytes) => Database::import_bytes(bytes),
            Self::File(file) => Database::open_prod(file.path()),
        }
    }
}
impl Write for SnapshotWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.options.check().map_err(std::io::Error::other)?;
        if bytes.len() as u64 > self.remaining {
            return Err(std::io::Error::other("snapshot byte limit exceeded"));
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn begin_read_session(
    database: &mut Database,
    options: RequestOptions,
    reply: Reply<ReadSessionStart>,
    readers: &Arc<AtomicUsize>,
    config: SharedDatabaseOptions,
) -> bool {
    if let Err(error) = options.check() {
        let _ = reply.send(Err(error));
        return true;
    }
    if readers.load(Ordering::Acquire) >= config.max_readers {
        let _ = reply.send(Err(PvError::Busy("snapshot reader limit reached".into())));
        return true;
    }
    readers.fetch_add(1, Ordering::AcqRel);
    let permit = ReaderPermit(Arc::clone(readers));
    let snapshot_tx = database.current_tx();
    let snapshot = call_database(|| {
        let mut file = if database.is_memory_backed() {
            ReadImage::Memory(Vec::new())
        } else {
            ReadImage::File(tempfile::NamedTempFile::new()?)
        };
        let mut writer = SnapshotWriter {
            file: file.writer(),
            remaining: config.max_snapshot_bytes,
            options: &options,
        };
        database.bake_to_writer(&mut writer).map_err(|error| {
            options.check().err().unwrap_or_else(|| match error {
                PvError::Io(ref io) if io.to_string().contains("snapshot byte limit") => {
                    PvError::ResourceLimit("snapshot byte limit exceeded".into())
                }
                other => other,
            })
        })?;
        options.check()?;
        Ok(file)
    });
    let snapshot = match snapshot {
        DatabaseCall::Ok(file) => file,
        DatabaseCall::Error(error) => {
            let _ = reply.send(Err(error));
            return true;
        }
        DatabaseCall::Panicked(error) => {
            let _ = reply.send(Err(error));
            return false;
        }
    };
    let error_reply = reply.clone();
    if let Err(error) = thread::Builder::new()
        .name("picovolt-reader".into())
        .spawn(move || {
            let opened = call_database(|| snapshot.open());
            let mut database = match opened {
                DatabaseCall::Ok(database) => database,
                DatabaseCall::Error(error) | DatabaseCall::Panicked(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            drop(snapshot);
            if let Err(error) = options.check() {
                let _ = reply.send(Err(error));
                return;
            }
            let (sender, receiver) = mpsc::sync_channel(1);
            if reply
                .send(Ok(ReadSessionStart {
                    sender,
                    snapshot_tx,
                }))
                .is_ok()
            {
                let completion = serve_read_session(&mut database, snapshot_tx, &options, receiver);
                drop(database);
                drop(permit);
                if let Some((reply, result)) = completion {
                    let _ = reply.send(result);
                }
            }
        })
    {
        let _ = error_reply.send(Err(error.into()));
    }
    true
}

fn serve_read_session(
    database: &mut Database,
    snapshot_tx: TxId,
    options: &RequestOptions,
    receiver: Receiver<ReadCommand>,
) -> Option<(Reply<()>, Result<()>)> {
    loop {
        if options.check().is_err() {
            return None;
        }
        match receiver.recv_timeout(session_poll_timeout(options)) {
            Ok(ReadCommand::Close { reply }) => {
                return Some((reply, options.check()));
            }
            Ok(ReadCommand::Query {
                sql,
                params,
                limits,
                reply,
            }) => {
                let (result, safe) =
                    execute_read(database, snapshot_tx, options, &sql, &params, limits);
                let _ = reply.send(result);
                if !safe {
                    return None;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn execute_read(
    database: &mut Database,
    snapshot_tx: TxId,
    options: &RequestOptions,
    sql: &str,
    params: &[Value],
    limits: QueryLimits,
) -> (Result<QueryResult>, bool) {
    if let Err(error) = options.check() {
        return (Err(error), true);
    }
    let bound = match bind_params(sql, params) {
        Ok(bound) => bound,
        Err(error) => return (Err(error), true),
    };
    let statement = match parse(&bound) {
        Ok(statement) => statement,
        Err(error) => return (Err(error), true),
    };
    if classify_statement(&statement) != StatementClass::Read {
        return (
            Err(PvError::Transaction(
                "mutating SQL is not allowed in a read transaction".into(),
            )),
            true,
        );
    }
    if let Some(requested_tx) = statement_before(&statement) {
        if requested_tx > snapshot_tx {
            return (
                Err(PvError::Transaction(format!(
                    "read transaction is pinned at {snapshot_tx}, but the statement requested snapshot {requested_tx}"
                ))),
                true,
            );
        }
    }
    match call_database(|| {
        database.query_with_limits_cancellable(
            sql,
            params,
            options.merged_limits(limits),
            Some(options.cancellation.clone()),
        )
    }) {
        DatabaseCall::Ok(value) => (Ok(value), true),
        DatabaseCall::Error(error) => {
            let safe = !outcome_is_unknown(&error);
            (Err(error), safe)
        }
        DatabaseCall::Panicked(error) => (Err(error), false),
    }
}

fn begin_write_session(
    database: &mut Database,
    options: RequestOptions,
    reply: Reply<WriteSessionStart>,
) -> bool {
    if let Err(error) = options.check() {
        let _ = reply.send(Err(error));
        return true;
    }
    if !database.is_writable() {
        let _ = reply.send(Err(PvError::ReadOnly));
        return true;
    }
    match call_database(|| database.begin_transaction()) {
        DatabaseCall::Ok(()) => {}
        DatabaseCall::Error(error) => {
            let fatal = matches!(error, PvError::TransactionOutcomeUnknown(_));
            let (error, safe) = finish_failed_write(database, error, fatal);
            let _ = reply.send(Err(error));
            return safe;
        }
        DatabaseCall::Panicked(error) => {
            let (error, _) = finish_failed_write(database, error, true);
            let _ = reply.send(Err(error));
            return false;
        }
    }
    if let Err(error) = options.check() {
        let (error, safe) = finish_failed_write(database, error, false);
        let _ = reply.send(Err(error));
        return safe;
    }

    let (sender, receiver) = mpsc::sync_channel(1);
    if reply.send(Ok(WriteSessionStart { sender })).is_err() {
        return safe_rollback(database).1;
    }
    serve_write_session(database, &options, receiver)
}

fn serve_write_session(
    database: &mut Database,
    options: &RequestOptions,
    receiver: Receiver<WriteCommand>,
) -> bool {
    loop {
        if let Err(error) = options.check() {
            return finish_failed_write(database, error, false).1;
        }
        match receiver.recv_timeout(session_poll_timeout(options)) {
            Ok(WriteCommand::Query {
                sql,
                params,
                limits,
                reply,
            }) => {
                let class = classify_sql(&sql, &params);
                let result = match class {
                    Ok(StatementClass::Control) => DatabaseCall::Error(PvError::Transaction(
                        "transaction-control SQL is owned by WriteTransaction".into(),
                    )),
                    Ok(StatementClass::Read | StatementClass::Write) => call_database(|| {
                        database.query_with_limits_cancellable(
                            &sql,
                            &params,
                            options.merged_limits(limits),
                            Some(options.cancellation.clone()),
                        )
                    }),
                    Err(error) => DatabaseCall::Error(error),
                };
                match result {
                    DatabaseCall::Ok(value) => {
                        if let Err(error) = options.check() {
                            let (error, safe) = finish_failed_write(database, error, false);
                            let _ = reply.send(Err(error));
                            return safe;
                        }
                        let _ = reply.send(Ok(value));
                    }
                    DatabaseCall::Error(error) => {
                        let (error, safe) = finish_failed_write(database, error, false);
                        let _ = reply.send(Err(error));
                        return safe;
                    }
                    DatabaseCall::Panicked(error) => {
                        let (error, _) = finish_failed_write(database, error, true);
                        let _ = reply.send(Err(error));
                        return false;
                    }
                }
            }
            Ok(WriteCommand::Commit { reply }) => {
                if let Err(error) = options.check() {
                    let (error, safe) = finish_failed_write(database, error, false);
                    let _ = reply.send(Err(error));
                    return safe;
                }
                let result = call_database(|| database.commit_transaction());
                match result {
                    DatabaseCall::Ok(()) => {
                        let _ = reply.send(Ok(()));
                        return true;
                    }
                    DatabaseCall::Error(error) => {
                        let (error, safe) = finish_failed_write(database, error, false);
                        let _ = reply.send(Err(error));
                        return safe;
                    }
                    DatabaseCall::Panicked(error) => {
                        let (error, _) = finish_failed_write(database, error, true);
                        let _ = reply.send(Err(error));
                        return false;
                    }
                }
            }
            Ok(WriteCommand::Rollback { reply }) => {
                let (result, safe) = rollback_for_response(database);
                let _ = reply.send(result);
                return safe;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return safe_rollback(database).1,
        }
    }
}

fn run_write<T>(
    database: &mut Database,
    options: &RequestOptions,
    operation: impl FnOnce(&mut Database) -> Result<T>,
) -> (Result<T>, bool) {
    if let Err(error) = options.check() {
        return (Err(error), true);
    }
    if !database.is_writable() {
        return (Err(PvError::ReadOnly), true);
    }
    match call_database(|| database.begin_transaction()) {
        DatabaseCall::Ok(()) => {}
        DatabaseCall::Error(error) => {
            let fatal = matches!(error, PvError::TransactionOutcomeUnknown(_));
            let (error, safe) = finish_failed_write(database, error, fatal);
            return (Err(error), safe);
        }
        DatabaseCall::Panicked(error) => {
            let (error, _) = finish_failed_write(database, error, true);
            return (Err(error), false);
        }
    }
    let result = catch_unwind(AssertUnwindSafe(|| operation(database)));
    match result {
        Ok(Ok(value)) => {
            if let Err(error) = options.check() {
                let (error, safe) = finish_failed_write(database, error, false);
                return (Err(error), safe);
            }
            match call_database(|| database.commit_transaction()) {
                DatabaseCall::Ok(()) => (Ok(value), true),
                DatabaseCall::Error(error) => {
                    let (error, safe) = finish_failed_write(database, error, false);
                    (Err(error), safe)
                }
                DatabaseCall::Panicked(error) => {
                    let (error, _) = finish_failed_write(database, error, true);
                    (Err(error), false)
                }
            }
        }
        Ok(Err(error)) => {
            let (error, safe) = finish_failed_write(database, error, false);
            (Err(error), safe)
        }
        Err(panic) => {
            let error =
                PvError::Transaction(format!("shared write panicked: {}", panic_payload(panic)));
            let (error, _) = finish_failed_write(database, error, true);
            (Err(error), false)
        }
    }
}

fn finish_failed_write(
    database: &mut Database,
    primary: PvError,
    poison_after_rollback: bool,
) -> (PvError, bool) {
    let poison_after_rollback = poison_after_rollback || outcome_is_unknown(&primary);
    if !database.in_transaction() {
        return (primary, !poison_after_rollback);
    }
    let (error, safe) = match safe_rollback(database).0 {
        Ok(()) => (primary, true),
        Err(rollback) => (
            PvError::TransactionOutcomeUnknown(format!(
                "operation failed ({primary}); rollback also failed ({rollback}); the shared database was closed"
            )),
            false,
        ),
    };
    (error, safe && !poison_after_rollback)
}

fn safe_rollback(database: &mut Database) -> (Result<()>, bool) {
    if !database.in_transaction() {
        return (Ok(()), true);
    }
    match call_database(|| database.rollback_transaction()) {
        DatabaseCall::Ok(()) => (Ok(()), true),
        DatabaseCall::Error(error) | DatabaseCall::Panicked(error) => (Err(error), false),
    }
}

fn rollback_for_response(database: &mut Database) -> (Result<()>, bool) {
    match safe_rollback(database) {
        (Ok(()), safe) => (Ok(()), safe),
        (Err(error), _) => (
            Err(PvError::TransactionOutcomeUnknown(format!(
                "explicit rollback failed ({error}); the shared database was closed"
            ))),
            false,
        ),
    }
}

fn outcome_is_unknown(error: &PvError) -> bool {
    matches!(error, PvError::TransactionOutcomeUnknown(_))
}

enum DatabaseCall<T> {
    Ok(T),
    Error(PvError),
    Panicked(PvError),
}

fn call_database<T>(operation: impl FnOnce() -> Result<T>) -> DatabaseCall<T> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(value)) => DatabaseCall::Ok(value),
        Ok(Err(error)) => DatabaseCall::Error(error),
        Err(panic) => DatabaseCall::Panicked(PvError::Transaction(format!(
            "shared database operation panicked: {}",
            panic_payload(panic)
        ))),
    }
}

fn classify_sql(sql: &str, params: &[Value]) -> Result<StatementClass> {
    let bound = bind_params(sql, params)?;
    Ok(classify_statement(&parse(&bound)?))
}

fn classify_statement(statement: &Statement) -> StatementClass {
    match statement {
        Statement::Select { .. } | Statement::SelectJoin { .. } => StatementClass::Read,
        Statement::Explain { statement } => classify_statement(statement),
        Statement::Begin | Statement::Commit | Statement::Rollback => StatementClass::Control,
        _ => StatementClass::Write,
    }
}

fn statement_before(statement: &Statement) -> Option<TxId> {
    match statement {
        Statement::Select { before, .. } | Statement::SelectJoin { before, .. } => *before,
        Statement::Explain { statement } => statement_before(statement),
        _ => None,
    }
}

fn session_poll_timeout(options: &RequestOptions) -> Duration {
    match options.limits.deadline {
        Some(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .min(SESSION_POLL_INTERVAL),
        None => SESSION_POLL_INTERVAL,
    }
}

fn earliest_deadline(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

const fn unbounded_limits() -> QueryLimits {
    QueryLimits::new(usize::MAX, usize::MAX, usize::MAX, None)
}

fn panic_payload(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_last_transaction_owner_detaches_a_busy_worker() {
        let (main_sender, main_receiver) = mpsc::sync_channel(1);
        let (session_sender, session_receiver) = mpsc::sync_channel(1);
        let (rollback_started_sender, rollback_started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        let worker = thread::spawn(move || {
            drop(main_receiver);
            assert!(session_receiver.recv().is_err());
            rollback_started_sender.send(()).unwrap();
            let _ = release_receiver.recv();
        });
        let inner = Arc::new(SharedInner {
            sender: Mutex::new(Some(main_sender)),
            worker: Mutex::new(Some(worker)),
        });
        let database = SharedDatabase {
            inner: Arc::clone(&inner),
        };
        let transaction = WriteTransaction {
            sender: Some(session_sender),
            options: RequestOptions::default(),
            _owner: inner,
            not_sync: PhantomData,
        };
        drop(database);

        let (drop_finished_sender, drop_finished_receiver) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(transaction);
            drop_finished_sender.send(()).unwrap();
        });
        rollback_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(drop_finished_receiver
            .recv_timeout(Duration::from_millis(250))
            .is_ok());

        release_sender.send(()).unwrap();
        dropper.join().unwrap();
    }
}
