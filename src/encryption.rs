// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
//! Authenticated encrypted snapshots and native, single-writer encrypted vaults.
//!
//! Vaults work in RAM and publish complete ciphertext snapshots. They never stage
//! plaintext on disk. This does not encrypt existing development directories,
//! operating-system swap, application exports, or process memory.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{AeadInPlace, KeyInit},
    Tag, XChaCha20Poly1305, XNonce,
};
use fs2::FileExt;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::engine::query::{bind_params, parse, Statement};
use crate::{Database, DatabaseStats, PvError, QueryLimits, QueryResult, Result, Value};

/// Maximum plaintext image size. Opening/committing also needs database and copy memory.
pub const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;
pub const HEADER_BYTES: usize = 64;
const TAG_BYTES: usize = 16;
const MAGIC: &[u8; 8] = b"PVENC\0\x01\0";

/// Owned secret material is zeroized on drop; deliberately has no Debug impl.
/// Caller-owned copies and loaded database pages are not comprehensively zeroized.
pub struct Secret(SecretKind);
enum SecretKind {
    Key(Zeroizing<[u8; 32]>),
    Password(Zeroizing<Vec<u8>>),
}

impl Secret {
    pub fn key(bytes: [u8; 32]) -> Self {
        Self(SecretKind::Key(Zeroizing::new(bytes)))
    }
    /// Password bytes are exact (no trimming), bounded to 12..=1024 bytes.
    pub fn password(bytes: &[u8]) -> Result<Self> {
        if !(12..=1024).contains(&bytes.len()) {
            return Err(invalid("password must contain 12..=1024 bytes"));
        }
        Ok(Self(SecretKind::Password(Zeroizing::new(bytes.to_vec()))))
    }
    pub fn generate() -> Result<Self> {
        let mut key = Zeroizing::new([0; 32]);
        getrandom::fill(key.as_mut()).map_err(|_| invalid("OS random source unavailable"))?;
        Ok(Self(SecretKind::Key(key)))
    }
    /// Load an exact 32-byte raw key. Restrictive Unix permissions are required.
    pub fn from_key_file(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = read_secret_file(path.as_ref(), 32)?;
        if bytes.len() != 32 {
            return Err(invalid("key file must contain exactly 32 bytes"));
        }
        let mut key = Zeroizing::new([0; 32]);
        key.copy_from_slice(&bytes);
        Ok(Self(SecretKind::Key(key)))
    }
    pub fn from_password_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::password(&read_secret_file(path.as_ref(), 1024)?)
    }
    /// Writes a raw key once; never overwrites a key. Passwords cannot be exported here.
    pub fn write_key_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let SecretKind::Key(key) = &self.0 else {
            return Err(invalid("not a raw key"));
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(key.as_ref())?;
        file.sync_all()?;
        Ok(())
    }
    fn mode(&self) -> u8 {
        match self.0 {
            SecretKind::Key(_) => 0,
            SecretKind::Password(_) => 1,
        }
    }
    fn derive(&self, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
        let mut out = Zeroizing::new([0; 32]);
        match &self.0 {
            SecretKind::Key(key) => out.copy_from_slice(key.as_ref()),
            SecretKind::Password(password) => {
                // Fixed profile: hostile headers cannot request arbitrary KDF work.
                let params = Params::new(65536, 3, 1, Some(32))
                    .map_err(|_| invalid("invalid KDF profile"))?;
                Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
                    .hash_password_into(password, salt, out.as_mut())
                    .map_err(|_| invalid("key derivation failed"))?;
            }
        }
        Ok(out)
    }
}

fn invalid(message: &str) -> PvError {
    PvError::Corruption(format!("encrypted snapshot: {message}"))
}

fn read_secret_file(path: &Path, max: usize) -> Result<Zeroizing<Vec<u8>>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("secret must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid(
                "secret file permissions must exclude group and other access",
            ));
        }
    }
    let mut bytes = Zeroizing::new(Vec::new());
    File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(invalid("secret file exceeds limit"));
    }
    Ok(bytes)
}

/// Public envelope facts only. Inspection does not authenticate the header.
#[derive(Debug, Serialize)]
pub struct EnvelopeInfo {
    pub format: &'static str,
    pub cipher: &'static str,
    pub key_derivation: &'static str,
    pub plaintext_bytes: usize,
    pub authenticated: bool,
}

pub fn inspect(bytes: &[u8]) -> Result<EnvelopeInfo> {
    if bytes.len() < HEADER_BYTES + TAG_BYTES
        || &bytes[..8] != MAGIC
        || bytes[8] > 1
        || bytes[9..16].iter().any(|b| *b != 0)
    {
        return Err(invalid("unsupported or malformed envelope"));
    }
    let length = u64::from_le_bytes(bytes[56..64].try_into().expect("header field"));
    if length > MAX_IMAGE_BYTES as u64
        || bytes.len() as u64 != length + (HEADER_BYTES + TAG_BYTES) as u64
    {
        return Err(invalid("invalid image length or image exceeds 256 MiB"));
    }
    Ok(EnvelopeInfo {
        format: "PVENC-1",
        cipher: "XChaCha20-Poly1305",
        key_derivation: if bytes[8] == 0 {
            "raw-256-bit"
        } else {
            "Argon2id-v19-m65536-t3-p1"
        },
        plaintext_bytes: length as usize,
        authenticated: false,
    })
}

/// Seal the entire baked image, including history, schema, indexes and blobs.
/// Explicit transactions must be committed or rolled back first.
pub fn seal(db: &mut Database, secret: &Secret) -> Result<Vec<u8>> {
    if db.in_transaction() {
        return Err(PvError::Transaction(
            "cannot seal an active transaction".into(),
        ));
    }
    let mut plaintext = Zeroizing::new(db.bake_to_bytes()?);
    if plaintext.len() > MAX_IMAGE_BYTES {
        return Err(PvError::ResourceLimit(
            "encrypted image exceeds 256 MiB".into(),
        ));
    }
    let mut header = [0u8; HEADER_BYTES];
    header[..8].copy_from_slice(MAGIC);
    header[8] = secret.mode();
    getrandom::fill(&mut header[16..56]).map_err(|_| invalid("OS random source unavailable"))?;
    header[56..64].copy_from_slice(&(plaintext.len() as u64).to_le_bytes());
    let key = secret.derive(&header[16..32])?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| invalid("invalid key"))?;
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(&header[32..56]), &header, &mut plaintext)
        .map_err(|_| invalid("encryption failed"))?;
    let mut output = Vec::with_capacity(HEADER_BYTES + plaintext.len() + TAG_BYTES);
    output.extend_from_slice(&header);
    output.extend_from_slice(&plaintext);
    output.extend_from_slice(&tag);
    Ok(output)
}

/// Authenticate every byte before parsing or exposing any database plaintext.
pub fn open(bytes: &[u8], secret: &Secret) -> Result<Database> {
    let info = inspect(bytes)?;
    if bytes[8] != secret.mode() {
        return Err(invalid("authentication failed"));
    }
    let key = secret.derive(&bytes[16..32])?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| invalid("invalid key"))?;
    let mut plaintext =
        Zeroizing::new(bytes[HEADER_BYTES..HEADER_BYTES + info.plaintext_bytes].to_vec());
    cipher
        .decrypt_in_place_detached(
            XNonce::from_slice(&bytes[32..56]),
            &bytes[..HEADER_BYTES],
            &mut plaintext,
            Tag::from_slice(&bytes[HEADER_BYTES + info.plaintext_bytes..]),
        )
        .map_err(|_| invalid("authentication failed"))?;
    Database::import_bytes(&plaintext)
}

pub fn read_file(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_IMAGE_BYTES + HEADER_BYTES + TAG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    inspect(&bytes)?;
    Ok(bytes)
}

/// Atomically publish ciphertext only. New outputs never overwrite existing paths.
/// A directory-sync failure after replacement reports an uncertain outcome.
fn publish(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(bytes)?;
    pending.as_file().sync_all()?;
    let result = if replace {
        pending.persist(path)
    } else {
        pending.persist_noclobber(path)
    };
    result.map_err(|e| e.error)?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|e| {
            PvError::TransactionOutcomeUnknown(format!(
                "ciphertext published but directory sync failed: {e}"
            ))
        })?;
    Ok(())
}

pub fn save_new(db: &mut Database, path: impl AsRef<Path>, secret: &Secret) -> Result<()> {
    publish(path.as_ref(), &seal(db, secret)?, false)
}

/// One process owns a vault until drop. Disk contains ciphertext plus an empty
/// advisory lock file. Full-image transactions are intended for bounded databases.
/// Keep the containing directory private; locks do not defend against a malicious
/// process that can replace lock files. Old valid snapshots can be replayed unless
/// the host separately pins their expected digest/version.
pub struct Vault {
    db: Database,
    secret: Secret,
    path: PathBuf,
    lock: File,
    digest: blake3::Hash,
    poisoned: bool,
}

impl Vault {
    fn acquire(path: &Path) -> Result<(PathBuf, File)> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| invalid("vault requires a file name"))?;
        let path = parent.canonicalize()?.join(name);
        match fs::symlink_metadata(&path) {
            Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
                return Err(invalid("vault must be a regular file"))
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let mut lock_name = name.to_os_string();
        lock_name.push(".pvlock");
        let lock_path = path.with_file_name(lock_name);
        if let Ok(m) = fs::symlink_metadata(&lock_path) {
            if !m.is_file() || m.file_type().is_symlink() {
                return Err(invalid("invalid lock file"));
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(lock_path)?;
        lock.try_lock_exclusive().map_err(|_| {
            PvError::Transaction("vault is already open or cannot be locked".into())
        })?;
        Ok((path, lock))
    }
    pub fn create(path: impl AsRef<Path>, secret: Secret) -> Result<Self> {
        let (path, lock) = Self::acquire(path.as_ref())?;
        let mut db = Database::open_memory();
        let bytes = seal(&mut db, &secret)?;
        publish(&path, &bytes, false)?;
        Ok(Self {
            db,
            secret,
            path,
            lock,
            digest: blake3::hash(&bytes),
            poisoned: false,
        })
    }
    pub fn open(path: impl AsRef<Path>, secret: Secret) -> Result<Self> {
        let (path, lock) = Self::acquire(path.as_ref())?;
        let bytes = read_file(&path)?;
        let db = open(&bytes, &secret)?;
        Ok(Self {
            db,
            secret,
            path,
            lock,
            digest: blake3::hash(&bytes),
            poisoned: false,
        })
    }
    fn ready(&self) -> Result<()> {
        if self.poisoned {
            Err(PvError::TransactionOutcomeUnknown(
                "reopen the vault before further operations".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn persist(&mut self, bytes: &[u8]) -> Result<()> {
        self.ready()?;
        if blake3::hash(&read_file(&self.path)?) != self.digest {
            self.poisoned = true;
            return Err(PvError::Transaction(
                "vault changed outside this handle; reopen it".into(),
            ));
        }
        if let Err(error) = publish(&self.path, bytes, true) {
            if matches!(error, PvError::TransactionOutcomeUnknown(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        self.digest = blake3::hash(bytes);
        Ok(())
    }
    /// The callback runs on a private candidate. Failure/panic leaves the current
    /// vault untouched. Success is returned only after ciphertext publication.
    pub fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Database) -> Result<T>,
    ) -> Result<T> {
        self.ready()?;
        let image = Zeroizing::new(self.db.bake_to_bytes()?);
        let mut candidate = Database::import_bytes(&image)?;
        let result = candidate.transaction(operation)?;
        if !candidate.is_memory_backed() {
            return Err(invalid("vault candidate must remain in memory"));
        }
        let bytes = seal(&mut candidate, &self.secret)?;
        self.persist(&bytes)?;
        self.db = candidate;
        Ok(result)
    }
    /// SELECT-only access. Mutation goes through transaction/execute_batch.
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.ready()?;
        if sql.len() > 65536 || params.len() > 256 {
            return Err(PvError::ResourceLimit("vault query limits exceeded".into()));
        }
        if !matches!(parse(&bind_params(sql, params)?)?, Statement::Select { .. }) {
            return Err(PvError::Query(
                "vault query requires SELECT; use a transaction for writes".into(),
            ));
        }
        self.db.query_with_limits(
            sql,
            params,
            QueryLimits::new(100_000, 16 * 1024 * 1024, 10_000, None),
        )
    }
    pub fn execute_batch(
        &mut self,
        statements: &[(String, Vec<Value>)],
    ) -> Result<Vec<QueryResult>> {
        if statements.is_empty()
            || statements.len() > 256
            || statements
                .iter()
                .any(|(s, p)| s.len() > 65536 || p.len() > 256)
        {
            return Err(PvError::ResourceLimit(
                "vault batch requires 1..=256 bounded statements".into(),
            ));
        }
        self.transaction(|db| {
            statements
                .iter()
                .map(|(sql, params)| {
                    if matches!(parse(&bind_params(sql, params)?)?, Statement::Select { .. }) {
                        return Err(PvError::Query(
                            "use query for SELECT, not a write batch".into(),
                        ));
                    }
                    db.query_with_limits(
                        sql,
                        params,
                        QueryLimits::new(100_000, 16 * 1024 * 1024, 10_000, None),
                    )
                })
                .collect()
        })
    }
    pub fn inspect(&self) -> Result<DatabaseStats> {
        self.ready()?;
        self.db.inspect_stats()
    }
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    pub fn retrieve_json(&mut self, request: &str) -> Result<String> {
        self.ready()?;
        self.db.retrieve_json(request)
    }
    /// Freshly re-encrypted backup, independently verified before publishing.
    pub fn backup(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.ready()?;
        let bytes = seal(&mut self.db, &self.secret)?;
        open(&bytes, &self.secret)?.inspect_stats()?;
        publish(path.as_ref(), &bytes, false)
    }
    /// Re-encrypt with fresh randomness. Old backups still require their old key.
    pub fn rotate_key(&mut self, new_secret: Secret) -> Result<()> {
        self.ready()?;
        let bytes = seal(&mut self.db, &new_secret)?;
        open(&bytes, &new_secret)?.inspect_stats()?;
        self.persist(&bytes)?;
        self.secret = new_secret;
        Ok(())
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}
