#![cfg(all(feature = "encryption", not(target_arch = "wasm32")))]
use picovolt::encryption::{self, Secret, Vault};
use picovolt::{Database, PvError, QueryResult, Value};
use std::fs;
use std::process::Command;

fn key() -> Secret {
    Secret::key([7; 32])
}

#[test]
fn vault_crash_child() {
    let Ok(directory) = std::env::var("PV_ENCRYPTION_CRASH_TEST_DIR") else {
        return;
    };
    let root = std::path::Path::new(&directory);
    let mut vault = Vault::open(root.join("vault"), key()).unwrap();
    let after = std::env::var("PV_ENCRYPTION_CRASH_AFTER_COMMIT").unwrap() == "1";
    vault
        .transaction(|db| {
            db.query("INSERT INTO t VALUES(1)")?;
            if !after {
                fs::write(root.join("ready"), b"ready")?;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            Ok(())
        })
        .unwrap();
    fs::write(root.join("ready"), b"ready").unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn process_termination_preserves_committed_state_and_releases_lock() {
    for after in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vault");
        let mut vault = Vault::create(&path, key()).unwrap();
        vault
            .transaction(|db| db.query("CREATE TABLE t(id)"))
            .unwrap();
        drop(vault);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "vault_crash_child", "--nocapture"])
            .env("PV_ENCRYPTION_CRASH_TEST_DIR", root.path())
            .env(
                "PV_ENCRYPTION_CRASH_AFTER_COMMIT",
                if after { "1" } else { "0" },
            )
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !root.path().join("ready").exists()
            && start.elapsed() < std::time::Duration::from_secs(20)
        {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let ready = root.path().join("ready").exists();
        let _ = child.kill();
        child.wait().unwrap();
        assert!(ready, "child did not reach crash boundary");
        let mut vault = Vault::open(&path, key()).unwrap();
        let QueryResult::Rows { rows, .. } = vault.query("SELECT * FROM t", &[]).unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), usize::from(after));
    }
}
fn fixture() -> Database {
    let mut db = Database::open_memory();
    db.query("CREATE TABLE secrets(id PRIMARY KEY, body)")
        .unwrap();
    db.query("INSERT INTO secrets VALUES(1,'private-customer-sentinel-739')")
        .unwrap();
    db
}
fn rows(db: &mut Database) -> usize {
    match db.query("SELECT * FROM secrets").unwrap() {
        QueryResult::Rows { rows, .. } => rows.len(),
        _ => panic!(),
    }
}
#[test]
fn snapshots_hide_contents_and_authenticate_every_region() {
    let mut db = fixture();
    let encrypted = encryption::seal(&mut db, &key()).unwrap();
    assert!(!encrypted.windows(16).any(|b| b == b"private-customer"));
    assert_ne!(encrypted, encryption::seal(&mut db, &key()).unwrap());
    assert!(!encryption::inspect(&encrypted).unwrap().authenticated);
    assert_eq!(rows(&mut encryption::open(&encrypted, &key()).unwrap()), 1);
    assert!(encryption::open(&encrypted, &Secret::key([8; 32])).is_err());
    for position in [
        0,
        7,
        8,
        9,
        16,
        31,
        32,
        55,
        56,
        63,
        64,
        encrypted.len() - 17,
        encrypted.len() - 1,
    ] {
        let mut changed = encrypted.clone();
        changed[position] ^= 1;
        assert!(
            encryption::open(&changed, &key()).is_err(),
            "accepted changed byte {position}"
        );
    }
    for len in [0, 1, 63, 64, 79, encrypted.len() - 1] {
        assert!(encryption::open(&encrypted[..len], &key()).is_err());
    }
    let mut trailing = encrypted.clone();
    trailing.push(0);
    assert!(encryption::open(&trailing, &key()).is_err());
    let mut huge = encrypted.clone();
    huge[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(encryption::inspect(&huge).is_err());
}
#[test]
fn passwords_use_fixed_argon2_profile_and_require_correct_secret() {
    let password = Secret::password(b"a long test-only passphrase").unwrap();
    let bytes = encryption::seal(&mut fixture(), &password).unwrap();
    assert!(encryption::inspect(&bytes)
        .unwrap()
        .key_derivation
        .starts_with("Argon2id"));
    assert_eq!(rows(&mut encryption::open(&bytes, &password).unwrap()), 1);
    assert!(encryption::open(
        &bytes,
        &Secret::password(b"a different test passphrase").unwrap()
    )
    .is_err());
    assert!(encryption::open(&bytes, &key()).is_err());
    assert!(Secret::password(b"short").is_err());
    assert!(Secret::password(&[0; 1025]).is_err());
}
#[test]
fn vault_commits_rolls_back_and_preserves_history_without_plaintext_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.pve");
    let mut vault = Vault::create(&path, key()).unwrap();
    assert!(Vault::open(&path, key()).is_err());
    vault
        .transaction(|db| {
            db.query("CREATE TABLE secrets(id PRIMARY KEY,body)")?;
            db.query("INSERT INTO secrets VALUES(1,'private-customer-sentinel-739')")?;
            Ok(())
        })
        .unwrap();
    let committed = fs::read(&path).unwrap();
    let result: picovolt::Result<()> = vault.transaction(|db| {
        db.query("INSERT INTO secrets VALUES(2,'must-roll-back')")?;
        Err(PvError::Query("test failure".into()))
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), committed);
    assert!(vault.query("DELETE FROM secrets", &[]).is_err());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = vault.transaction::<()>(|db| {
            db.query("INSERT INTO secrets VALUES(3,'panic')")?;
            panic!("test panic")
        });
    }));
    assert!(panic.is_err());
    assert_eq!(fs::read(&path).unwrap(), committed);
    for item in fs::read_dir(dir.path()).unwrap() {
        let item = item.unwrap();
        if item.path().extension().is_some_and(|e| e == "pvlock") {
            assert_eq!(item.metadata().unwrap().len(), 0);
            continue;
        }
        let bytes = fs::read(item.path()).unwrap();
        assert!(!bytes
            .windows(b"private-customer-sentinel-739".len())
            .any(|b| b == b"private-customer-sentinel-739"));
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    let tx = vault.inspect().unwrap().current_transaction;
    vault
        .transaction(|db| db.query("UPDATE secrets SET body='changed' WHERE id=1"))
        .unwrap();
    drop(vault);
    let mut vault = Vault::open(&path, key()).unwrap();
    let old = vault
        .query(&format!("SELECT body FROM secrets BEFORE {tx}"), &[])
        .unwrap();
    assert!(
        matches!(old,QueryResult::Rows{rows,..} if rows[0][0]==Value::Text("private-customer-sentinel-739".into()))
    );
}
#[test]
fn backup_restore_rotation_and_external_change_detection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let backup = dir.path().join("backup");
    let mut vault = Vault::create(&path, key()).unwrap();
    vault
        .execute_batch(&[
            ("CREATE TABLE t(id)".into(), vec![]),
            ("INSERT INTO t VALUES(?)".into(), vec![Value::Int(42)]),
        ])
        .unwrap();
    vault.backup(&backup).unwrap();
    assert!(vault.backup(&backup).is_err());
    vault.rotate_key(Secret::key([9; 32])).unwrap();
    drop(vault);
    assert!(Vault::open(&path, key()).is_err());
    let mut vault = Vault::open(&path, Secret::key([9; 32])).unwrap();
    let mut restored = encryption::open(&fs::read(&backup).unwrap(), &key()).unwrap();
    assert!(
        matches!(restored.query("SELECT * FROM t").unwrap(),QueryResult::Rows{rows,..} if rows.len()==1)
    );
    assert!(encryption::open(&fs::read(&backup).unwrap(), &Secret::key([9; 32])).is_err());
    fs::write(&path, fs::read(&backup).unwrap()).unwrap();
    assert!(vault
        .transaction(|db| db.query("INSERT INTO t VALUES(99)"))
        .is_err());
    assert!(vault.query("SELECT * FROM t", &[]).is_err());
    assert_eq!(fs::read(path).unwrap(), fs::read(backup).unwrap());
}
#[test]
fn cli_key_vault_batch_verify_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let key = dir.path().join("key");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pv"))
            .args(args)
            .output()
            .unwrap()
    };
    let k = key.to_str().unwrap();
    let p = path.to_str().unwrap();
    assert!(run(&["crypto", "keygen", k]).status.success());
    assert!(!run(&["crypto", "keygen", k]).status.success());
    assert!(run(&["vault", "create", p, "--key-file", k])
        .status
        .success());
    assert!(!run(&["vault", "create", p, "--key-file", k])
        .status
        .success());
    let commands = dir.path().join("commands.json");
    fs::write(
        &commands,
        r#"[{"sql":"CREATE TABLE t(id)"},{"sql":"INSERT INTO t VALUES(?)","params":[42]}]"#,
    )
    .unwrap();
    let result = run(&[
        "vault",
        "batch",
        p,
        "--key-file",
        k,
        commands.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(run(&["crypto", "verify", p, "--key-file", k])
        .status
        .success());
    let restored = dir.path().join("restored");
    assert!(run(&[
        "crypto",
        "restore",
        p,
        restored.to_str().unwrap(),
        "--key-file",
        k
    ])
    .status
    .success());
    assert!(!run(&[
        "crypto",
        "restore",
        p,
        restored.to_str().unwrap(),
        "--key-file",
        k
    ])
    .status
    .success());
    let result = run(&[
        "vault",
        "query",
        restored.to_str().unwrap(),
        "--key-file",
        k,
        "SELECT * FROM t",
    ]);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("42"));
}
#[cfg(unix)]
#[test]
fn unix_secrets_and_ciphertext_are_private_and_symlinks_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let keyfile = dir.path().join("key");
    key().write_key_file(&keyfile).unwrap();
    assert_eq!(
        fs::metadata(&keyfile).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&keyfile, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Secret::from_key_file(&keyfile).is_err());
    let path = dir.path().join("vault");
    let vault = Vault::create(&path, key()).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(vault);
    let alias = dir.path().join("alias");
    symlink(&path, &alias).unwrap();
    assert!(Vault::open(alias, key()).is_err());
}
