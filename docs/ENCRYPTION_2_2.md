# Encrypted storage in PicoVolt 2.2

2.2 adds native encrypted snapshots and single-writer vaults. The `encryption`
Cargo feature is enabled by default on native targets. It is not a browser/WASM
encryption API. Existing plaintext `.pvdb` files and development directories keep
their current format and behavior; encryption is an explicit choice.

## Start with a vault

```sh
pv crypto keygen private.key
pv vault create app.pve --key-file private.key
pv vault batch app.pve --key-file private.key commands.json
pv vault query app.pve --key-file private.key "SELECT * FROM articles"
pv vault backup app.pve --key-file private.key backup.pve
pv crypto verify backup.pve --key-file private.key
pv crypto restore backup.pve restored.pve --key-file private.key
```

`commands.json` is a JSON array of `{sql, params?}` objects:

```json
[
  {"sql":"CREATE TABLE articles(id PRIMARY KEY, title, embedding)"},
  {"sql":"INSERT INTO articles VALUES(?,?,?)", "params":[1,"Verified backups","[1,0]"]}
]
```

Every successful batch is committed to ciphertext before returning. A failing
batch publishes nothing. SELECT belongs to `query`; use `retrieve` with the
[hybrid search request](HYBRID_2_2.md) for full-text/vector search.

To encrypt an existing baked snapshot, use
`pv crypto seal original.pvdb protected.pve --key-file private.key`. **The original
plaintext remains.** Encryption does not erase existing files, old backups,
filesystem snapshots or logs. Verify and retain recovery copies before planning
any separate cleanup of a migrated source.

## Rust, Python, Go and C

```rust
use picovolt::encryption::{Secret, Vault};
let mut vault = Vault::create("app.pve", Secret::from_key_file("private.key")?)?;
vault.transaction(|db| {
    db.query("CREATE TABLE notes(id, body)")?;
    db.query("INSERT INTO notes VALUES(1, 'keep private')")?;
    Ok(())
})?;
let rows = vault.query("SELECT * FROM notes", &[])?;
# Ok::<(), picovolt::PvError>(())
```

```python
from pathlib import Path
from picovolt.vault import Vault

key = Path('private.key').read_bytes()
with Vault('app.pve', key=key) as vault:
    print(vault.query('SELECT * FROM articles WHERE id=?', [1]))
    vault.backup('another-backup.pve')
```

Go: `OpenVault(path, keyBytes, false, false)` returns a handle with `Request`,
`RotateKey` and `Close`. C: `pv_vault_open`, `pv_vault_request`, `pv_vault_rotate`,
`pv_vault_close`; free returned JSON using `pv_string_free`. The request actions
are `query` (`sql`, optional `params`), `batch` (`commands`), `inspect`, `backup`
(`path`), and `retrieve` (`request`, containing a normal retrieval request).
Handles must not be used concurrently or after close. Python and Go keep
caller-owned secret/result objects subject to their respective runtime behavior.

The Rust `seal`, `open`, `save_new`, `read_file` and `inspect` functions also
support encrypted snapshots independently of the managed `Vault` lifecycle.
Opening an encrypted snapshot returns a writable in-memory `Database`; that
standalone handle does not autosave. `seal` rejects an active transaction.

## Keys, passwords and rotation

Raw keys contain exactly 32 cryptographically random bytes. The CLI reads a key
file; it never accepts the secret itself in argv. `keygen` refuses to overwrite
existing files. Store the key separately from the vault and backups, with a
protected recovery copy. There is no key recovery service or hidden master key.

Alternatively use `--password-file file` or `Vault(..., password=bytes)`.
Passwords are 12–1024 **exact bytes**; trailing newlines are significant. Length
validation is not an entropy guarantee: choose a strong, unique passphrase.
Argon2id v19 derives a 32-byte key using 64 MiB, three passes, one lane and a
fresh 16-byte salt. This fixed profile prevents untrusted headers from requesting
arbitrary KDF costs. Password operation takes additional CPU and memory.

`pv vault rotate app.pve --key-file old.key --new-key-file new.key` authenticates
the vault and publishes a freshly encrypted snapshot. Rust/Python/Go rotation
can also switch between key and password modes. **Old backups retain their old
keys.** Rotation cannot revoke ciphertext copies already held with the old key.

On Unix, generated keys, ciphertext staging files and lock files use mode 0600;
key/password loading rejects group/other access. On Windows, access follows the
containing directory ACL: use an owner-restricted directory. Keep vault and lock
files in a private directory on a local filesystem with reliable atomic rename
and locks. Do not use network filesystem locking as an untested substitute.

## Persistence and recovery contract

Vaults keep plaintext database pages in RAM. A write transaction works on a
private candidate; success bakes its complete state, encrypts it, syncs a
ciphertext-only temporary file, then atomically replaces the vault. Unix also
syncs the parent directory. If that final sync fails, the outcome is uncertain
and the handle must be closed/reopened before further operations. Windows does
not provide the same directory-fsync path; no hardware power-loss guarantee is
claimed. A kill before publication leaves the previous image; after a successful
commit the published snapshot survives process termination.

An advisory `.pvlock` file prevents cooperating processes from opening a second
writer. It remains after close; do not remove it while a process is using the
vault. A changed ciphertext digest prevents a handle from overwriting a replaced
snapshot. This is not distributed coordination or protection against an attacker
who controls the directory and can replace its lock files.

Each image is capped at 256 MiB. Transactions copy/rebuild the full image and
need substantially more RAM than the image size; this is not page-level encrypted
I/O or a memory SLA. Batches accept at most 256 bounded statements; JSON requests
are capped at 1 MiB. Queries use 100,000 scanned-version, 10,000 returned-row and
16 MiB materialization budgets. SQL/parameter limits do not replace host quotas.

## Envelope and threat model

`PVENC-1` uses XChaCha20-Poly1305 with a fresh random 24-byte nonce per encryption.
The complete 64-byte header is authenticated as associated data. It contains an
8-byte magic/version, a key-mode byte, seven reserved zero bytes, a 16-byte salt,
a 24-byte nonce and a little-endian 64-bit plaintext length. Ciphertext and a
16-byte authentication tag follow. Unknown modes/versions, reserved bits, lengths,
truncation and trailing bytes are rejected. All ciphertext is authenticated
before database parsing. Schema, indexes, blobs and MVCC history are encrypted.

`crypto inspect` exposes public format/length/KDF facts with `authenticated:false`.
Only `crypto verify` with the correct secret verifies the tag and database.
Encryption hides file contents and detects alteration. It reveals size and
access patterns, does not prevent rollback to an old valid snapshot after reopen,
and does not protect an unlocked process, results/exports, swap, crash dumps or
host-controlled telemetry. Pin an expected snapshot digest/version externally
when rollback detection is required. Owned secret and temporary decrypted-image
buffers are zeroized; comprehensive database-page or language-runtime erasure is
not claimed.

The implementation uses [RustCrypto XChaCha20-Poly1305](https://docs.rs/chacha20poly1305/0.10.1/chacha20poly1305/)
and [RustCrypto Argon2](https://docs.rs/argon2/0.5.3/argon2/). Argon2id is specified
in [RFC 9106](https://www.rfc-editor.org/rfc/rfc9106.html). PicoVolt uses the
64 MiB/three-pass costs with one lane; this differs from the RFC's recommended
64 MiB profile, which uses four lanes.
PicoVolt's integration is new and has not received an independent security audit.
