//! Golden-corpus and failure-path gates for the 1.9 format migrator.

use std::fs;
use std::path::{Path, PathBuf};

use picovolt::{migrate_file, plan_file_migration, Database, MigrationOptions, FORMAT_VERSION};

fn golden_images() -> Vec<PathBuf> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut images = fs::read_dir(fixtures)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("pvdb"))
        .collect::<Vec<_>>();
    images.sort();
    assert!(!images.is_empty());
    images
}

#[test]
fn migrates_and_deeply_verifies_the_complete_golden_corpus() {
    for source in golden_images() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("migrated.pvdb");
        let backup = temp.path().join("source.backup.pvdb");

        let plan = plan_file_migration(&source, &destination, Some(&backup)).unwrap();
        assert_eq!(plan.target_format, FORMAT_VERSION);
        assert!(
            !destination.exists(),
            "dry-run planning wrote a destination"
        );
        assert!(!backup.exists(), "dry-run planning wrote a backup");

        let report = migrate_file(
            &source,
            &destination,
            &MigrationOptions {
                backup: Some(backup.clone()),
            },
        )
        .unwrap_or_else(|error| panic!("{}: {error}", source.display()));
        assert!(report.verified);
        assert_eq!(report.target_format, FORMAT_VERSION);
        assert_eq!(fs::read(&backup).unwrap(), fs::read(&source).unwrap());

        let original = Database::open_prod(&source).unwrap();
        let migrated = Database::open_prod(&destination).unwrap();
        assert_eq!(
            migrated.inspect_stats().unwrap().format_version,
            FORMAT_VERSION
        );
        assert_eq!(
            original.verification_hash().unwrap(),
            migrated.verification_hash().unwrap()
        );
        assert_eq!(original.table_names(), migrated.table_names());
        for transaction in 0..=original.current_tx() {
            for table in original.table_names() {
                assert_eq!(
                    original.select(&table, Some(transaction)).unwrap(),
                    migrated.select(&table, Some(transaction)).unwrap(),
                    "{} table {table} transaction {transaction}",
                    source.display()
                );
            }
        }
    }
}

#[test]
fn migration_is_no_clobber_and_rejects_aliasing_paths() {
    let source = golden_images().remove(0);
    assert!(plan_file_migration(&source, &source, None).is_err());

    let temp = tempfile::tempdir().unwrap();
    let occupied = temp.path().join("occupied.pvdb");
    fs::write(&occupied, b"keep me").unwrap();
    assert!(plan_file_migration(&source, &occupied, None).is_err());
    assert_eq!(fs::read(&occupied).unwrap(), b"keep me");

    let destination = temp.path().join("new.pvdb");
    let occupied_backup = temp.path().join("backup.pvdb");
    fs::write(&occupied_backup, b"keep backup").unwrap();
    assert!(plan_file_migration(&source, &destination, Some(&occupied_backup)).is_err());
    assert!(!destination.exists());
    assert_eq!(fs::read(&occupied_backup).unwrap(), b"keep backup");
}

#[test]
fn corrupt_source_fails_before_creating_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("corrupt.pvdb");
    let destination = temp.path().join("destination.pvdb");
    let backup = temp.path().join("backup.pvdb");
    fs::write(&source, b"not a picovolt image").unwrap();
    assert!(plan_file_migration(&source, &destination, Some(&backup)).is_err());
    assert!(!destination.exists());
    assert!(!backup.exists());
}
