//! Safe, history-preserving migration of baked `.pvdb` images.
//!
//! Migration is intentionally out-of-place: the source is never opened for
//! writing, the destination must not exist, and an optional backup is copied and
//! synced before any output is produced. The migrated temporary image is deeply
//! reopened and hashed before it is atomically published.

#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{Database, PvError, Result, FORMAT_VERSION};

/// Validated, read-only description of a proposed migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationPlan {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub backup: Option<PathBuf>,
    pub source_format: u16,
    pub target_format: u16,
    pub current_transaction: u64,
    pub tables: usize,
    pub row_versions: u64,
    pub source_bytes: u64,
    pub verification_hash: String,
}

/// Options for [`migrate_file`].
#[derive(Debug, Clone, Default)]
pub struct MigrationOptions {
    /// Optional byte-for-byte backup of the source image, created first.
    pub backup: Option<PathBuf>,
}

/// Successful migration outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub backup: Option<PathBuf>,
    pub source_format: u16,
    pub target_format: u16,
    pub current_transaction: u64,
    pub tables: usize,
    pub row_versions: u64,
    pub output_bytes: u64,
    pub verification_hash: String,
    pub verified: bool,
}

/// Validate the full source image and return a dry-run plan without creating
/// the destination or backup.
pub fn plan_file_migration(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    backup: Option<&Path>,
) -> Result<MigrationPlan> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    validate_paths(source, destination, backup)?;
    let database = Database::open_prod(source)?;
    let stats = database.inspect_stats()?;
    let verification_hash = database.verification_hash()?;
    Ok(MigrationPlan {
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        backup: backup.map(Path::to_path_buf),
        source_format: stats.format_version,
        target_format: FORMAT_VERSION,
        current_transaction: stats.current_transaction,
        tables: stats.tables.len(),
        row_versions: stats.tables.iter().map(|table| table.row_versions).sum(),
        source_bytes: fs::metadata(source)?.len(),
        verification_hash,
    })
}

/// Migrate one baked image to the latest writer format. Publication is
/// same-directory and no-clobber, so a failure leaves both source and any
/// pre-existing destination untouched.
pub fn migrate_file(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    options: &MigrationOptions,
) -> Result<MigrationReport> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    let plan = plan_file_migration(source, destination, options.backup.as_deref())?;

    if let Some(backup) = &options.backup {
        copy_file_noclobber(source, backup)?;
    }

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut database = Database::open_prod(source)?;
    let _ = database.upgrade_format_to_latest();
    database.bake_to_writer(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;

    // Verification is mandatory. Callers may choose whether to create an
    // additional exact backup, but this safe API never publishes an unchecked
    // migrated image.
    let migrated = Database::open_prod(temporary.path())?;
    let migrated_stats = migrated.inspect_stats()?;
    let migrated_hash = migrated.verification_hash()?;
    let migrated_versions: u64 = migrated_stats
        .tables
        .iter()
        .map(|table| table.row_versions)
        .sum();
    if migrated_stats.format_version != FORMAT_VERSION
        || migrated_stats.current_transaction != plan.current_transaction
        || migrated_stats.tables.len() != plan.tables
        || migrated_versions != plan.row_versions
        || migrated_hash != plan.verification_hash
    {
        return Err(PvError::Corruption(
            "migrated image failed catalog, history, or content verification".into(),
        ));
    }

    let output_bytes = temporary.as_file().metadata()?.len();
    let published = temporary
        .persist_noclobber(destination)
        .map_err(|error| PvError::Io(error.error))?;
    published.sync_all()?;
    sync_parent_directory(parent)?;
    Ok(MigrationReport {
        source: plan.source,
        destination: plan.destination,
        backup: plan.backup,
        source_format: plan.source_format,
        target_format: plan.target_format,
        current_transaction: plan.current_transaction,
        tables: plan.tables,
        row_versions: plan.row_versions,
        output_bytes,
        verification_hash: plan.verification_hash,
        verified: true,
    })
}

fn validate_paths(source: &Path, destination: &Path, backup: Option<&Path>) -> Result<()> {
    if !source.is_file() {
        return Err(PvError::Schema(format!(
            "migration source must be an existing baked .pvdb file: {}",
            source.display()
        )));
    }
    if destination.exists() {
        return Err(PvError::Schema(format!(
            "migration destination already exists: {}",
            destination.display()
        )));
    }
    ensure_distinct(source, destination, "destination")?;
    if let Some(backup) = backup {
        if backup.exists() {
            return Err(PvError::Schema(format!(
                "migration backup already exists: {}",
                backup.display()
            )));
        }
        ensure_distinct(source, backup, "backup")?;
        ensure_distinct(destination, backup, "backup")?;
    }
    Ok(())
}

fn ensure_distinct(left: &Path, right: &Path, label: &str) -> Result<()> {
    let left = normalized_absolute(left)?;
    let right = normalized_absolute(right)?;
    if left == right {
        return Err(PvError::Schema(format!(
            "migration {label} must differ from the source and other outputs"
        )));
    }
    Ok(())
}

fn normalized_absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| PvError::Schema("migration path has no filename".into()))?;
    Ok(parent.join(name))
}

fn copy_file_noclobber(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut input = OpenOptions::new().read(true).open(source)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        temporary.write_all(&buffer[..read])?;
    }
    temporary.as_file().sync_all()?;
    let published = temporary
        .persist_noclobber(destination)
        .map_err(|error| PvError::Io(error.error))?;
    published.sync_all()?;
    sync_parent_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<()> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    // Windows does not expose portable directory fsync through std. The file
    // itself is synced above and persist_noclobber still provides no-overwrite
    // publication semantics.
    Ok(())
}
