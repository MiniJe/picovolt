//! Ed25519-signed dataset manifests. Verification requires a public key supplied
//! independently by the caller; the embedded key is never a trust anchor.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use picovolt::{FileHeader, PvError, Result, FILE_HEADER_SIZE};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

const DOMAIN: &[u8] = b"PicoVolt dataset manifest v1\0";
const MAX_MANIFEST: u64 = 64 * 1024;

/// Authenticated dataset metadata. No paths or URLs are automatically opened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifest {
    /// Manifest schema version (currently 1).
    pub schema_version: u32,
    /// Human-selected dataset identity, independent of its local filename.
    pub name: String,
    /// Exact artifact length.
    pub size_bytes: u64,
    /// BLAKE3 digest of the entire artifact.
    pub blake3: String,
    /// On-disk PicoVolt format version.
    pub format_version: u16,
}

/// Envelope containing authenticated metadata and an Ed25519 signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedDatasetManifest {
    /// Signed metadata.
    pub manifest: DatasetManifest,
    /// Informational signer public key, lower-case hexadecimal.
    pub public_key: String,
    /// Ed25519 signature, lower-case hexadecimal.
    pub signature: String,
}

fn error(message: impl std::fmt::Display) -> PvError {
    PvError::Corruption(format!("dataset manifest: {message}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("invalid hex length or character"));
    }
    let mut bytes = [0u8; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(error)?;
    }
    Ok(bytes)
}

fn message(manifest: &DatasetManifest) -> Result<Vec<u8>> {
    if manifest.schema_version != 1 || manifest.name.is_empty() || manifest.name.len() > 1024 {
        return Err(error("unsupported schema version or invalid dataset name"));
    }
    let mut bytes = DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(manifest)?);
    Ok(bytes)
}

fn describe(path: &Path, name: &str) -> Result<DatasetManifest> {
    let mut file = File::open(path)?;
    let mut header = [0; FILE_HEADER_SIZE];
    file.read_exact(&mut header)?;
    let header_value = FileHeader::decode(&header)?;
    let mut hash = blake3::Hasher::new();
    hash.update(&header);
    let mut count = header.len() as u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        count += n as u64;
    }
    Ok(DatasetManifest {
        schema_version: 1,
        name: name.into(),
        size_bytes: count,
        blake3: hash.finalize().to_hex().to_string(),
        format_version: header_value.format_version,
    })
}

/// Generate a new 32-byte raw private seed without overwriting an existing
/// file. Returns the public key as hex. On Unix the key is created with mode
/// 0600; on Windows secure the containing directory using its access controls.
pub fn generate_key(path: &Path) -> Result<String> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).map_err(error)?;
    let key = SigningKey::from_bytes(&seed);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    (|| {
        let mut file = options.open(path)?;
        file.write_all(seed.as_ref())?;
        file.sync_all()?;
        Ok(hex(key.verifying_key().as_bytes()))
    })()
}

/// Sign an artifact with a raw 32-byte seed stored in `key_path`. The output is
/// a manifest envelope; callers choose where it is stored.
pub fn sign(path: &Path, name: &str, key_path: &Path) -> Result<SignedDatasetManifest> {
    let mut file = File::open(key_path)?;
    if file.metadata()?.len() != 32 {
        return Err(error("private key must contain exactly 32 raw bytes"));
    }
    let mut seed = Zeroizing::new([0; 32]);
    file.read_exact(seed.as_mut())?;
    let key = SigningKey::from_bytes(&seed);
    let manifest = describe(path, name)?;
    let signature = key.sign(&message(&manifest)?);
    Ok(SignedDatasetManifest {
        manifest,
        public_key: hex(key.verifying_key().as_bytes()),
        signature: hex(&signature.to_bytes()),
    })
}

/// Verify signature and exact artifact bytes against an independently trusted
/// public key. A valid signature authenticates bytes, not SQL compatibility;
/// open/query the verified artifact separately to validate its data.
pub fn verify(path: &Path, manifest_path: &Path, trusted_key_hex: &str) -> Result<DatasetManifest> {
    let mut bytes = Vec::new();
    File::open(manifest_path)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(error("manifest exceeds 64 KiB"));
    }
    let envelope: SignedDatasetManifest = serde_json::from_slice(&bytes)?;
    let trusted = decode::<32>(trusted_key_hex)?;
    if decode::<32>(&envelope.public_key)? != trusted {
        return Err(error("signer does not match trusted public key"));
    }
    let key = VerifyingKey::from_bytes(&trusted).map_err(error)?;
    key.verify_strict(
        &message(&envelope.manifest)?,
        &Signature::from_bytes(&decode::<64>(&envelope.signature)?),
    )
    .map_err(error)?;
    let actual = describe(path, &envelope.manifest.name)?;
    if actual != envelope.manifest {
        return Err(error("artifact length, hash, or format does not match"));
    }
    Ok(envelope.manifest)
}
