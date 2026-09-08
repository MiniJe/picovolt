//! Bounded, checksummed page undo journal and ordered physical change stream.
//!
//! The active directory is synced before any page can be overwritten. Each
//! original page is synced once before its first write. Data and the change
//! record are synced before renaming `active` to its sequence number: that
//! rename is the commit point. Recovery of `active` is idempotent undo.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::vle::PAGES_PER_CHUNK;
use crate::{PvError, Result, MANIFEST_FILE, PAGE_SIZE};

pub const COMMIT_LOG_DIR: &str = ".pv-log";
const HARD_MAX_RECORD: u64 = 256 * 1024 * 1024;

/// Limits include retained undo data and serialized change records. Reaching a
/// limit rejects a write; consumers explicitly prune acknowledged history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLogOptions {
    pub max_transaction_bytes: u64,
    pub max_retained_bytes: u64,
    pub max_retained_commits: usize,
}

impl Default for CommitLogOptions {
    fn default() -> Self {
        Self {
            max_transaction_bytes: 64 * 1024 * 1024,
            max_retained_bytes: 256 * 1024 * 1024,
            max_retained_commits: 4096,
        }
    }
}

impl CommitLogOptions {
    pub(crate) fn validate(self) -> Result<Self> {
        if self.max_transaction_bytes == 0
            || self.max_transaction_bytes > HARD_MAX_RECORD
            || self.max_retained_bytes < self.max_transaction_bytes
            || self.max_retained_commits == 0
            || self.max_retained_commits > 65536
        {
            return Err(PvError::ResourceLimit("invalid commit-log limits".into()));
        }
        Ok(self)
    }
}

/// One final physical page image, including its native page checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageChange {
    pub page_id: u64,
    pub bytes: Vec<u8>,
}

/// A newly referenced content-addressed blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobChange {
    pub hash: String,
    pub bytes: Vec<u8>,
}

/// A durable commit. Sequence numbers order commits, independently of MVCC ids
/// (a transaction can contain several mutations or catalog-only changes).
/// Physical replication requires a matching verified base image and format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCommit {
    pub schema_version: u32,
    pub sequence: u64,
    pub before_tx: u64,
    pub after_tx: u64,
    pub manifest: Vec<u8>,
    pub pages: Vec<PageChange>,
    pub blobs: Vec<BlobChange>,
}

/// Host-owned extension point. Encryption, transport, acknowledgements and
/// idempotent replay belong to the consumer; the engine makes no network calls.
pub trait ChangeSink {
    fn accept(&mut self, commit: &ChangeCommit) -> Result<()>;
}

/// A quiescent image and the exclusive change cursor that follows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCheckpoint {
    pub sequence: u64,
    pub transaction: u64,
    pub verification_hash: String,
}

/// Retention diagnostics; cursors are commit sequences, not MVCC transaction IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitLogStatus {
    pub head_sequence: u64,
    pub pruned_through: u64,
    pub retained_commits: usize,
    pub retained_bytes: u64,
    pub limits: CommitLogOptions,
}

pub(crate) fn status(root: &Path, options: CommitLogOptions) -> Result<CommitLogStatus> {
    let log = root.join(COMMIT_LOG_DIR);
    let (head_sequence, pruned_through, commits) = inventory(root)?;
    Ok(CommitLogStatus {
        head_sequence,
        pruned_through,
        retained_commits: commits.len(),
        retained_bytes: if log.exists() { tree_size(&log)? } else { 0 },
        limits: options,
    })
}

pub(crate) fn head(root: &Path) -> Result<u64> {
    Ok(inventory(root)?.0)
}

// The manifest is published before the journal rename. While undo is active,
// its checked before-image is the last committed manifest instead.
fn inventory(root: &Path) -> Result<(u64, u64, Vec<u64>)> {
    let log = root.join(COMMIT_LOG_DIR);
    let commits = sequences(&log)?;
    let floor = checkpoint(&log)?;
    let before = log.join("active").join("before");
    let bytes = if before.exists() {
        read_checked(&before)?
    } else if root.join(MANIFEST_FILE).exists() {
        read_bounded(&root.join(MANIFEST_FILE), HARD_MAX_RECORD)?
    } else {
        Vec::new()
    };
    let manifest: Option<ManifestMeta> = if bytes.is_empty() {
        None
    } else {
        Some(serde_json::from_slice(&bytes)?)
    };
    if let Some(manifest) = manifest
        .as_ref()
        .filter(|m| m.format_version > crate::FORMAT_VERSION)
    {
        return Err(PvError::Corruption(format!(
            "unsupported workspace format version {} (maximum {})",
            manifest.format_version,
            crate::FORMAT_VERSION
        )));
    }
    let surviving_head = commits.last().copied().unwrap_or(0).max(floor);
    let head = match manifest.as_ref().and_then(|m| m.commit_sequence) {
        Some(anchor) => anchor,
        None => {
            // Upgrade intact legacy logs only when the latest record proves
            // that it describes the live database. An empty legacy log cannot
            // distinguish fully pruned history from lost acknowledged writes.
            if let Some(sequence) = commits.last().filter(|seq| **seq > floor) {
                let change = decode_change(&read_checked(
                    &log.join(format!("{sequence:020}")).join("change"),
                )?)?;
                if change.sequence != *sequence || change.manifest != bytes {
                    return Err(PvError::Corruption("legacy commit history does not match the database; restore a verified backup".into()));
                }
            } else if floor > 0
                || manifest
                    .as_ref()
                    .is_some_and(|m| m.format_version >= crate::FORMAT_VERSION_COMMIT_LOG)
            {
                return Err(PvError::Corruption("legacy commit history has no verifiable sequence anchor; restore a verified backup".into()));
            }
            surviving_head
        }
    };
    if floor > head || surviving_head > head {
        return Err(PvError::Corruption(
            "commit history exceeds the manifest sequence anchor".into(),
        ));
    }
    let mut expected = floor;
    for sequence in commits.iter().copied().filter(|seq| *seq > floor) {
        if expected.checked_add(1) != Some(sequence) {
            return Err(PvError::Corruption(
                "commit history is missing an unpruned sequence".into(),
            ));
        }
        safe_file(&log.join(format!("{sequence:020}")).join("change"))?;
        expected = sequence;
    }
    if expected != head {
        return Err(PvError::Corruption(
            "commit history is missing its acknowledged tail; restore a verified backup".into(),
        ));
    }
    Ok((head, floor, commits))
}

#[derive(Serialize, Deserialize)]
struct Header {
    sequence: u64,
    before_tx: u64,
    page_count: u64,
}

// Deserialize only recovery metadata. Persisted indexes can dwarf these fields;
// building a generic JSON tree for them on every commit wastes CPU and memory.
#[derive(Deserialize)]
struct ManifestMeta {
    #[serde(default)]
    format_version: u16,
    #[serde(default)]
    commit_sequence: Option<u64>,
    clock: u64,
    page_count: u64,
    cas_hashes: Vec<String>,
}

pub(crate) struct Journal {
    root: PathBuf,
    header: Header,
    touched: BTreeSet<u64>,
    options: CommitLogOptions,
    used: u64,
    retained: u64,
    old_hashes: BTreeSet<String>,
}

impl Journal {
    pub(crate) fn sequence(&self) -> u64 {
        self.header.sequence
    }

    pub(crate) fn begin(root: &Path, options: CommitLogOptions) -> Result<Self> {
        let options = options.validate()?;
        let (head, _, commits) = inventory(root)?;
        let log = root.join(COMMIT_LOG_DIR);
        fs::create_dir_all(&log)?;
        safe_directory(&log)?;
        if log.join("active").exists() {
            return Err(PvError::Transaction("commit log needs recovery".into()));
        }
        cleanup_inactive(&log)?;
        if commits.len() >= options.max_retained_commits {
            return Err(PvError::ResourceLimit(
                "commit log is full; prune acknowledged commits".into(),
            ));
        }
        let retained = tree_size(&log)?;
        let before = read_bounded(&root.join(MANIFEST_FILE), options.max_transaction_bytes)?;
        let manifest: ManifestMeta = serde_json::from_slice(&before)?;
        let header = Header {
            sequence: head
                .checked_add(1)
                .ok_or_else(|| PvError::ResourceLimit("commit sequence exhausted".into()))?,
            before_tx: manifest.clock,
            page_count: manifest.page_count,
        };
        let staging = log.join("preparing");
        if staging.exists() {
            safe_directory(&staging)?;
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir(&staging)?;
        let mut journal = Self {
            root: root.to_path_buf(),
            header,
            touched: BTreeSet::new(),
            options,
            used: 0,
            retained,
            old_hashes: manifest.cas_hashes.into_iter().collect(),
        };
        journal.reserve(before.len() as u64 + 1024)?;
        write_checked(&staging.join("before"), &before)?;
        write_checked(
            &staging.join("header"),
            &serde_json::to_vec(&journal.header)?,
        )?;
        fs::create_dir(staging.join("undo"))?;
        sync_dir(&staging)?;
        fs::rename(staging, log.join("active"))?;
        sync_dir(&log)?;
        sync_dir(root)?;
        crash_point("prepared");
        Ok(journal)
    }

    fn reserve(&mut self, bytes: u64) -> Result<()> {
        let used = self
            .used
            .checked_add(bytes)
            .ok_or_else(|| PvError::ResourceLimit("journal size overflow".into()))?;
        if used > self.options.max_transaction_bytes
            || self.retained.saturating_add(used) > self.options.max_retained_bytes
        {
            return Err(PvError::ResourceLimit(
                "commit log byte limit reached; reduce transaction or prune history".into(),
            ));
        }
        self.used = used;
        Ok(())
    }

    pub(crate) fn before_write(&mut self, id: u64) -> Result<()> {
        if self.touched.contains(&id) {
            return Ok(());
        }
        self.reserve(PAGE_SIZE as u64 + 32)?;
        if id < self.header.page_count {
            let page = read_page(&self.root, id)?;
            let undo = self.root.join(COMMIT_LOG_DIR).join("active/undo");
            let pending = undo.join(format!("{id:020}.tmp"));
            write_checked(&pending, &page)?;
            fs::rename(pending, undo.join(format!("{id:020}")))?;
            sync_dir(&undo)?;
            crash_point("undo_synced");
        }
        self.touched.insert(id);
        Ok(())
    }

    pub(crate) fn commit(&mut self) -> Result<()> {
        let log = self.root.join(COMMIT_LOG_DIR);
        let active = log.join("active");
        let manifest = read_bounded(
            &self.root.join(MANIFEST_FILE),
            self.options.max_transaction_bytes,
        )?;
        let after: ManifestMeta = serde_json::from_slice(&manifest)?;
        if after.commit_sequence != Some(self.header.sequence) {
            return Err(PvError::Corruption(
                "commit manifest sequence anchor mismatch".into(),
            ));
        }
        let mut change = ChangeCommit {
            schema_version: 1,
            sequence: self.header.sequence,
            before_tx: self.header.before_tx,
            after_tx: after.clock,
            manifest,
            pages: Vec::new(),
            blobs: Vec::new(),
        };
        let mut payload_bytes = change.manifest.len() as u64;
        for &page_id in &self.touched {
            payload_bytes = payload_bytes.saturating_add(PAGE_SIZE as u64);
            if payload_bytes > self.options.max_transaction_bytes.saturating_sub(self.used) {
                return Err(PvError::ResourceLimit("change payload byte limit".into()));
            }
            change.pages.push(PageChange {
                page_id,
                bytes: read_page(&self.root, page_id)?.to_vec(),
            });
        }
        for hash in &after.cas_hashes {
            if !self.old_hashes.contains(hash) {
                if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(PvError::Corruption("invalid journal blob hash".into()));
                }
                let path = self.root.join("blobs").join(&hash[..2]).join(hash);
                let bytes = read_bounded(
                    &path,
                    self.options
                        .max_transaction_bytes
                        .saturating_sub(self.used)
                        .saturating_sub(payload_bytes),
                )?;
                payload_bytes = payload_bytes.saturating_add(bytes.len() as u64);
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)?
                    .sync_all()?;
                sync_dir(path.parent().expect("blob parent"))?;
                sync_dir(&self.root.join("blobs"))?;
                if blake3::hash(&bytes).to_hex().as_str() != hash {
                    return Err(PvError::Corruption("journal blob hash mismatch".into()));
                }
                change.blobs.push(BlobChange {
                    hash: hash.into(),
                    bytes,
                });
            }
        }
        // Streaming serialization enforces the budget before allocating a
        // potentially large JSON representation of binary pages and blobs.
        let mut encoded = LimitedBytes {
            bytes: Vec::new(),
            limit: self.options.max_transaction_bytes.saturating_sub(self.used),
        };
        encode_change(&mut encoded, &change).map_err(|e| {
            PvError::ResourceLimit(format!("commit record exceeds transaction budget: {e}"))
        })?;
        self.reserve(encoded.bytes.len() as u64 + 32)?;
        write_checked(&active.join("change"), &encoded.bytes)?;
        sync_dir(&active)?;
        crash_point("record_synced");
        sync_dir(&self.root.join("chunks"))?;
        sync_dir(&self.root)?;
        fs::rename(&active, log.join(format!("{:020}", self.header.sequence)))?;
        crash_point("commit_renamed");
        sync_dir(&log).map_err(|error| {
            PvError::TransactionOutcomeUnknown(format!(
                "commit renamed but directory sync failed: {error}"
            ))
        })?;
        Ok(())
    }
}

struct LimitedBytes {
    bytes: Vec<u8>,
    limit: u64,
}
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if (self.bytes.len() as u64).saturating_add(bytes.len() as u64) > self.limit {
            return Err(std::io::Error::other("commit record byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn recover(root: &Path) -> Result<()> {
    let log = root.join(COMMIT_LOG_DIR);
    if !log.exists() {
        head(root)?;
        return Ok(());
    }
    safe_directory(&log)?;
    let active = log.join("active");
    if !active.exists() {
        head(root)?;
        return cleanup_inactive(&log);
    }
    safe_directory(&active)?;
    let header: Header = serde_json::from_slice(&read_checked(&active.join("header"))?)?;
    let before = read_checked(&active.join("before"))?;
    let manifest: ManifestMeta = serde_json::from_slice(&before)?;
    if manifest.clock != header.before_tx || manifest.page_count != header.page_count {
        return Err(PvError::Corruption(
            "journal header/manifest mismatch".into(),
        ));
    }
    if head(root)?.checked_add(1) != Some(header.sequence) {
        return Err(PvError::Corruption(
            "active journal sequence mismatch".into(),
        ));
    }
    let undo = active.join("undo");
    safe_directory(&undo)?;
    let mut pages = Vec::new();
    for entry in fs::read_dir(&undo)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".tmp") {
            continue;
        }
        let id = parse_sequence(&name.to_string_lossy())?;
        if id >= header.page_count {
            return Err(PvError::Corruption(
                "undo page outside original image".into(),
            ));
        }
        let bytes = read_checked(&entry.path())?;
        if bytes.len() != PAGE_SIZE {
            return Err(PvError::Corruption("invalid undo page size".into()));
        }
        // Validate all entries before changing the live workspace. Bound memory
        // by retaining only ids; the second pass rechecks each payload.
        pages.push(id);
        if pages.len() as u64 * (PAGE_SIZE as u64 + 32) > HARD_MAX_RECORD {
            return Err(PvError::ResourceLimit("undo journal too large".into()));
        }
    }
    safe_directory(&root.join("chunks"))?;
    for id in pages {
        let bytes = read_checked(&undo.join(format!("{id:020}")))?;
        let path = chunk_path(root, id);
        safe_file(&path)?;
        let mut file = OpenOptions::new().write(true).open(path)?;
        file.seek(SeekFrom::Start(id % PAGES_PER_CHUNK * PAGE_SIZE as u64))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    // New append pages and blobs are unreachable under the original manifest;
    // retaining them avoids destructive directory replacement during recovery.
    safe_file(&root.join(MANIFEST_FILE))?;
    let mut tmp = tempfile::NamedTempFile::new_in(root)?;
    tmp.write_all(&before)?;
    tmp.as_file().sync_all()?;
    tmp.persist(root.join(MANIFEST_FILE))
        .map_err(|e| PvError::Io(e.error))?;
    sync_dir(root)?;
    // Rename makes cleanup crash-safe: an interrupted delete can never leave
    // a partially deleted active recovery journal.
    let discarded = log.join("discarded");
    if discarded.exists() {
        safe_directory(&discarded)?;
        fs::remove_dir_all(&discarded)?;
    }
    fs::rename(active, &discarded)?;
    sync_dir(&log)?;
    fs::remove_dir_all(discarded)?;
    cleanup_inactive(&log)?;
    Ok(())
}

// These names can never contain a committed record or the active undo image.
// Clear interrupted preparation/cleanup before charging retained history.
fn cleanup_inactive(log: &Path) -> Result<()> {
    for name in ["preparing", "discarded"] {
        let path = log.join(name);
        if path.exists() {
            // Validate the entire bounded tree before recursively removing it.
            tree_size(&path)?;
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

pub(crate) fn changes(root: &Path, after: u64, limit: usize) -> Result<Vec<ChangeCommit>> {
    let (head, floor, commits) = inventory(root)?;
    if after > head {
        return Err(PvError::Transaction(
            "change cursor is beyond the committed head".into(),
        ));
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    if limit > 4096 {
        return Err(PvError::ResourceLimit(
            "change batch exceeds 4096 commits".into(),
        ));
    }
    let log = root.join(COMMIT_LOG_DIR);
    if after < floor {
        return Err(PvError::Transaction(format!(
            "change cursor {after} was pruned through {floor}; obtain a new base image"
        )));
    }
    let mut out = Vec::new();
    let mut total = 0u64;
    for sequence in commits.into_iter().filter(|seq| *seq > after).take(limit) {
        let path = log.join(format!("{sequence:020}")).join("change");
        total = total.saturating_add(fs::metadata(&path)?.len());
        if total > HARD_MAX_RECORD {
            return Err(PvError::ResourceLimit(
                "change batch exceeds 256 MiB; request fewer commits".into(),
            ));
        }
        let change = decode_change(&read_checked(&path)?)?;
        if change.schema_version != 1
            || change.sequence != sequence
            || sequence != after + out.len() as u64 + 1
        {
            return Err(PvError::Corruption(
                "change stream sequence or schema mismatch".into(),
            ));
        }
        out.push(change);
    }
    Ok(out)
}

pub(crate) fn prune(root: &Path, through: u64) -> Result<()> {
    let log = root.join(COMMIT_LOG_DIR);
    let (head, old, commits) = inventory(root)?;
    if through < old {
        return Ok(());
    }
    if through > head {
        return Err(PvError::Transaction("cannot prune a future commit".into()));
    }
    let mut tmp = tempfile::NamedTempFile::new_in(&log)?;
    let bytes = through.to_le_bytes();
    tmp.write_all(blake3::hash(&bytes).as_bytes())?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(log.join("checkpoint"))
        .map_err(|e| PvError::Io(e.error))?;
    sync_dir(&log)?;
    for seq in commits.into_iter().filter(|seq| *seq <= through) {
        let path = log.join(format!("{seq:020}"));
        safe_directory(&path)?;
        fs::remove_dir_all(path)?;
    }
    sync_dir(&log)
}

fn checkpoint(log: &Path) -> Result<u64> {
    let path = log.join("checkpoint");
    if !path.exists() {
        return Ok(0);
    }
    let bytes = read_checked(&path)?;
    Ok(u64::from_le_bytes(bytes.try_into().map_err(|_| {
        PvError::Corruption("invalid log checkpoint".into())
    })?))
}

fn sequences(log: &Path) -> Result<Vec<u64>> {
    if !log.exists() {
        return Ok(Vec::new());
    }
    safe_directory(log)?;
    let mut result = Vec::new();
    for entry in fs::read_dir(log)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() == 20 && name.bytes().all(|c| c.is_ascii_digit()) {
            safe_directory(&entry.path())?;
            result.push(parse_sequence(&name)?);
            if result.len() > 65536 {
                return Err(PvError::ResourceLimit("too many retained commits".into()));
            }
        }
    }
    result.sort_unstable();
    Ok(result)
}

fn parse_sequence(name: &str) -> Result<u64> {
    if name.len() != 20 || !name.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PvError::Corruption("invalid journal entry name".into()));
    }
    name.parse()
        .map_err(|_| PvError::Corruption("journal entry overflow".into()))
}
const CHANGE_MAGIC: &[u8; 8] = b"PVCHG001";

fn encode_change(out: &mut impl Write, change: &ChangeCommit) -> std::io::Result<()> {
    out.write_all(CHANGE_MAGIC)?;
    for n in [
        change.sequence,
        change.before_tx,
        change.after_tx,
        change.manifest.len() as u64,
    ] {
        out.write_all(&n.to_le_bytes())?;
    }
    out.write_all(&change.manifest)?;
    out.write_all(&(change.pages.len() as u64).to_le_bytes())?;
    for page in &change.pages {
        out.write_all(&page.page_id.to_le_bytes())?;
        out.write_all(&page.bytes)?;
    }
    out.write_all(&(change.blobs.len() as u64).to_le_bytes())?;
    for blob in &change.blobs {
        out.write_all(blob.hash.as_bytes())?;
        out.write_all(&(blob.bytes.len() as u64).to_le_bytes())?;
        out.write_all(&blob.bytes)?;
    }
    Ok(())
}

fn decode_change(bytes: &[u8]) -> Result<ChangeCommit> {
    if !bytes.starts_with(CHANGE_MAGIC) {
        // Existing rc.1 JSON records remain readable. The public ChangeCommit
        // JSON representation is unchanged; only the disk envelope is compact.
        return Ok(serde_json::from_slice(bytes)?);
    }
    struct Input<'a>(&'a [u8]);
    impl<'a> Input<'a> {
        fn take(&mut self, n: u64) -> Result<&'a [u8]> {
            let n = usize::try_from(n)
                .ok()
                .filter(|&n| n <= self.0.len())
                .ok_or_else(|| PvError::Corruption("truncated binary commit".into()))?;
            let (head, tail) = self.0.split_at(n);
            self.0 = tail;
            Ok(head)
        }
        fn number(&mut self) -> Result<u64> {
            Ok(u64::from_le_bytes(
                self.take(8)?.try_into().expect("eight bytes"),
            ))
        }
    }
    let mut input = Input(&bytes[CHANGE_MAGIC.len()..]);
    let sequence = input.number()?;
    let before_tx = input.number()?;
    let after_tx = input.number()?;
    let len = input.number()?;
    let manifest = input.take(len)?.to_vec();
    let count = input.number()?;
    if count > input.0.len() as u64 / (PAGE_SIZE as u64 + 8) {
        return Err(PvError::Corruption(
            "invalid binary commit page count".into(),
        ));
    }
    let mut pages = Vec::new();
    for _ in 0..count {
        pages.push(PageChange {
            page_id: input.number()?,
            bytes: input.take(PAGE_SIZE as u64)?.to_vec(),
        });
    }
    let count = input.number()?;
    if count > input.0.len() as u64 / 72 {
        return Err(PvError::Corruption(
            "invalid binary commit blob count".into(),
        ));
    }
    let mut blobs = Vec::new();
    for _ in 0..count {
        let hash = std::str::from_utf8(input.take(64)?)
            .map_err(|_| PvError::Corruption("invalid binary commit blob hash".into()))?
            .to_string();
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(PvError::Corruption(
                "invalid binary commit blob hash".into(),
            ));
        }
        let len = input.number()?;
        blobs.push(BlobChange {
            hash,
            bytes: input.take(len)?.to_vec(),
        });
    }
    if !input.0.is_empty() {
        return Err(PvError::Corruption("trailing binary commit data".into()));
    }
    Ok(ChangeCommit {
        schema_version: 1,
        sequence,
        before_tx,
        after_tx,
        manifest,
        pages,
        blobs,
    })
}
fn chunk_path(root: &Path, id: u64) -> PathBuf {
    root.join("chunks")
        .join(format!("chunk_{:05}.pvd", id / PAGES_PER_CHUNK))
}
fn read_page(root: &Path, id: u64) -> Result<[u8; PAGE_SIZE]> {
    let path = chunk_path(root, id);
    safe_directory(&root.join("chunks"))?;
    safe_file(&path)?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(id % PAGES_PER_CHUNK * PAGE_SIZE as u64))?;
    let mut page = [0; PAGE_SIZE];
    file.read_exact(&mut page)?;
    Ok(page)
}
fn safe_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(PvError::Corruption(format!(
            "expected regular journal file: {}",
            path.display()
        )));
    }
    Ok(())
}
fn safe_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(PvError::Corruption(format!(
            "expected real journal directory: {}",
            path.display()
        )));
    }
    Ok(())
}
fn tree_size(path: &Path) -> Result<u64> {
    tree_size_bounded(path, 0, &mut 0)
}

fn tree_size_bounded(path: &Path, depth: usize, entries: &mut usize) -> Result<u64> {
    if depth > 4 {
        return Err(PvError::ResourceLimit(
            "commit log directory nesting exceeds limit".into(),
        ));
    }
    safe_directory(path)?;
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        *entries += 1;
        if *entries > 262144 {
            return Err(PvError::ResourceLimit(
                "commit log entry count exceeds limit".into(),
            ));
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(PvError::Corruption("symlink in commit log".into()));
        }
        total = total.saturating_add(if metadata.is_dir() {
            tree_size_bounded(&entry.path(), depth + 1, entries)?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}
fn write_checked(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(blake3::hash(bytes).as_bytes())?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    safe_file(path)?;
    let mut file = File::open(path)?.take(max.saturating_add(1));
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(PvError::ResourceLimit(
            "journal file exceeds byte limit".into(),
        ));
    }
    Ok(bytes)
}
fn read_checked(path: &Path) -> Result<Vec<u8>> {
    let bytes = read_bounded(path, HARD_MAX_RECORD)?;
    if bytes.len() < 32 || blake3::hash(&bytes[32..]).as_bytes() != &bytes[..32] {
        return Err(PvError::Corruption("commit log checksum mismatch".into()));
    }
    Ok(bytes[32..].to_vec())
}
#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

// Fault injection exists only in the unit-test binary, never production builds.
#[cfg(test)]
pub(crate) fn crash_point(stage: &str) {
    if std::env::var("PICOVOLT_JOURNAL_CRASH_STAGE").as_deref() == Ok(stage) {
        std::process::exit(86);
    }
}
#[cfg(not(test))]
#[inline]
pub(crate) fn crash_point(_stage: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, Value};

    #[test]
    fn binary_commits_and_legacy_json_round_trip_and_reject_truncation() {
        let change = ChangeCommit {
            schema_version: 1,
            sequence: 9,
            before_tx: 10,
            after_tx: 12,
            manifest: br#"{"clock":12}"#.to_vec(),
            pages: vec![PageChange {
                page_id: 3,
                bytes: vec![7; PAGE_SIZE],
            }],
            blobs: vec![BlobChange {
                hash: blake3::hash(b"payload").to_hex().to_string(),
                bytes: b"payload".to_vec(),
            }],
        };
        let mut bytes = Vec::new();
        encode_change(&mut bytes, &change).unwrap();
        assert_eq!(decode_change(&bytes).unwrap(), change);
        assert_eq!(
            decode_change(&serde_json::to_vec(&change).unwrap()).unwrap(),
            change
        );
        for end in 0..bytes.len() {
            assert!(decode_change(&bytes[..end]).is_err(), "prefix {end}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_change(&trailing).is_err());
        // Hostile manifest length cannot overflow or cause speculative allocation.
        bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_change(&bytes).is_err());
    }

    #[test]
    fn crash_worker() {
        let Some(root) = std::env::var_os("PICOVOLT_JOURNAL_CRASH_ROOT") else {
            return;
        };
        let mut db = Database::open_dev(root).unwrap();
        db.enable_commit_log(CommitLogOptions::default()).unwrap();
        db.begin_transaction().unwrap();
        db.query("UPDATE t SET value = 'replacement long blob payload' WHERE id = 1")
            .unwrap();
        db.query("INSERT INTO t VALUES (2, 'new long content addressed blob')")
            .unwrap();
        db.flush_now().unwrap();
        crash_point("pages_flushed");
        db.commit_transaction().unwrap();
        std::process::exit(86);
    }

    #[test]
    fn crash_boundaries_recover_database_and_stream_together() {
        for stage in [
            "prepared",
            "undo_synced",
            "pages_flushed",
            "record_synced",
            "commit_renamed",
            "committed",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut db = Database::open_dev(temp.path()).unwrap();
            db.enable_commit_log(CommitLogOptions::default()).unwrap();
            db.transaction(|db| {
                db.query("CREATE TABLE t (id, value)")?;
                db.query("INSERT INTO t VALUES (1, 'original long blob payload')")?;
                Ok(())
            })
            .unwrap();
            drop(db);
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "journal::tests::crash_worker", "--nocapture"])
                .env("PICOVOLT_JOURNAL_CRASH_ROOT", temp.path())
                .env("PICOVOLT_JOURNAL_CRASH_STAGE", stage)
                .output()
                .unwrap();
            assert_eq!(
                child.status.code(),
                Some(86),
                "stage {stage}: {}",
                String::from_utf8_lossy(&child.stderr)
            );
            let mut reopened = Database::open_dev(temp.path()).unwrap();
            let rows = reopened.query("SELECT * FROM t ORDER BY id").unwrap();
            let committed = stage == "commit_renamed" || stage == "committed";
            assert_eq!(
                rows.rows().unwrap().len(),
                if committed { 2 } else { 1 },
                "stage {stage}"
            );
            assert_eq!(
                rows.rows().unwrap()[0][1],
                Value::Text(
                    if committed {
                        "replacement long blob payload"
                    } else {
                        "original long blob payload"
                    }
                    .into()
                )
            );
            assert_eq!(
                reopened.changes_since(0, 10).unwrap().len(),
                if committed { 2 } else { 1 }
            );
            drop(reopened);
            assert!(
                Database::open_dev(temp.path()).is_ok(),
                "second recovery at {stage}"
            );
        }
    }

    #[test]
    fn checksum_failure_preserves_recovery_evidence_and_live_data() {
        let temp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(temp.path()).unwrap();
        db.query("CREATE TABLE t (id)").unwrap();
        db.enable_commit_log(CommitLogOptions::default()).unwrap();
        db.begin_transaction().unwrap();
        db.query("INSERT INTO t VALUES (1)").unwrap();
        db.flush_now().unwrap();
        drop(db);
        let active = temp.path().join(COMMIT_LOG_DIR).join("active");
        let original = fs::read(temp.path().join(MANIFEST_FILE)).unwrap();
        fs::write(active.join("before"), b"corrupt").unwrap();
        assert!(Database::open_dev(temp.path()).is_err());
        assert!(active.exists());
        assert_eq!(fs::read(temp.path().join(MANIFEST_FILE)).unwrap(), original);
    }

    #[test]
    fn intact_legacy_history_upgrades_but_missing_tail_is_rejected() {
        for missing_tail in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let mut db = Database::open_dev(temp.path()).unwrap();
            db.enable_commit_log(CommitLogOptions::default()).unwrap();
            db.query("CREATE TABLE t(id)").unwrap();
            db.query("INSERT INTO t VALUES(1)").unwrap();
            drop(db);
            let log = temp.path().join(COMMIT_LOG_DIR);
            let mut final_manifest = Vec::new();
            for sequence in 1..=2 {
                let path = log.join(format!("{sequence:020}")).join("change");
                let mut change = decode_change(&read_checked(&path).unwrap()).unwrap();
                let mut manifest: serde_json::Value =
                    serde_json::from_slice(&change.manifest).unwrap();
                manifest.as_object_mut().unwrap().remove("commit_sequence");
                manifest["format_version"] = 6.into();
                change.manifest = serde_json::to_vec(&manifest).unwrap();
                final_manifest = change.manifest.clone();
                let mut encoded = Vec::new();
                encode_change(&mut encoded, &change).unwrap();
                fs::rename(&path, path.with_extension("preserved-rc3")).unwrap();
                write_checked(&path, &encoded).unwrap();
            }
            fs::write(temp.path().join(MANIFEST_FILE), final_manifest).unwrap();
            if missing_tail {
                fs::rename(
                    log.join(format!("{:020}", 2)),
                    temp.path().join("preserved-tail"),
                )
                .unwrap();
                assert!(Database::open_dev(temp.path()).is_err());
            } else {
                let mut db = Database::open_dev(temp.path()).unwrap();
                db.query("INSERT INTO t VALUES(2)").unwrap();
                assert_eq!(db.changes_since(2, 10).unwrap()[0].sequence, 3);
                drop(db);
                assert_eq!(head(temp.path()).unwrap(), 3);
            }
        }
    }

    #[test]
    fn interrupted_prune_keeps_anchor_and_ignores_expired_directories() {
        let temp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(temp.path()).unwrap();
        db.enable_commit_log(CommitLogOptions::default()).unwrap();
        db.query("CREATE TABLE t(id)").unwrap();
        db.query("INSERT INTO t VALUES(1)").unwrap();
        drop(db);
        let log = temp.path().join(COMMIT_LOG_DIR);
        write_checked(&log.join("checkpoint"), &2u64.to_le_bytes()).unwrap();
        let mut db = Database::open_dev(temp.path()).unwrap();
        db.query("INSERT INTO t VALUES(2)").unwrap();
        assert_eq!(db.changes_since(2, 10).unwrap()[0].sequence, 3);
        db.prune_changes(3).unwrap();
        drop(db);
        assert_eq!(head(temp.path()).unwrap(), 3);
    }

    #[test]
    fn interrupted_cleanup_does_not_consume_retention_budget() {
        let temp = tempfile::tempdir().unwrap();
        let mut db = Database::open_dev(temp.path()).unwrap();
        let options = CommitLogOptions {
            max_transaction_bytes: 64 * 1024,
            max_retained_bytes: 64 * 1024,
            max_retained_commits: 8,
        };
        db.enable_commit_log(options).unwrap();
        db.query("CREATE TABLE t (id)").unwrap();
        let log = temp.path().join(COMMIT_LOG_DIR);
        for name in ["preparing", "discarded"] {
            fs::create_dir(log.join(name)).unwrap();
            fs::write(log.join(name).join("partial"), vec![0; 64 * 1024]).unwrap();
        }
        // Same-process retry must not charge abandoned bytes to the next write.
        db.query("INSERT INTO t VALUES (1)").unwrap();
        assert!(!log.join("preparing").exists());
        assert!(!log.join("discarded").exists());
        drop(db);
        fs::create_dir(log.join("discarded")).unwrap();
        fs::write(log.join("discarded/partial"), b"interrupted deletion").unwrap();
        let mut reopened = Database::open_dev(temp.path()).unwrap();
        assert!(!log.join("discarded").exists());
        assert_eq!(
            reopened.query("SELECT id FROM t").unwrap().rows().unwrap(),
            &[vec![Value::Int(1)]]
        );
        assert_eq!(reopened.changes_since(0, 10).unwrap().len(), 2);
    }
}
