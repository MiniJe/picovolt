// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
//! Encrypted vault C ABI. Secrets are byte slices, never included in errors.
use super::*;
use crate::encryption::{Secret, Vault};
use crate::{PvError, Result, Value};
use serde::Deserialize;

pub struct PvVault {
    inner: Vault,
}

unsafe fn secret(bytes: *const u8, len: usize, password: i32) -> Result<Secret> {
    if bytes.is_null()
        || !matches!(password, 0 | 1)
        || (password == 0 && len != 32)
        || (password == 1 && !(12..=1024).contains(&len))
    {
        return Err(PvError::Query("invalid secret kind or length".into()));
    }
    let input = unsafe { std::slice::from_raw_parts(bytes, len) };
    if password == 1 {
        Secret::password(input)
    } else {
        let mut key = zeroize::Zeroizing::new([0; 32]);
        key.copy_from_slice(input);
        Ok(Secret::key(*key))
    }
}

/// Open/create a locked encrypted vault. password and create must be 0 or 1.
/// # Safety
/// path must be NULL or NUL-terminated UTF-8; secret must cover secret_len bytes.
#[no_mangle]
pub unsafe extern "C" fn pv_vault_open(
    path: *const c_char,
    bytes: *const u8,
    secret_len: usize,
    password: i32,
    create: i32,
) -> *mut PvVault {
    guard(ptr::null_mut(), || {
        clear_last_error();
        let result = (|| -> Result<Vault> {
            let path = unsafe { cstr_to_str(path) }
                .ok_or_else(|| PvError::Query("invalid vault path".into()))?;
            let secret = unsafe { secret(bytes, secret_len, password) }?;
            match create {
                0 => Vault::open(path, secret),
                1 => Vault::create(path, secret),
                _ => Err(PvError::Query("create must be 0 or 1".into())),
            }
        })();
        match result {
            Ok(inner) => Box::into_raw(Box::new(PvVault { inner })),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    sql: String,
    #[serde(default)]
    params: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Query {
        sql: String,
        #[serde(default)]
        params: Vec<serde_json::Value>,
    },
    Batch {
        commands: Vec<Command>,
    },
    Inspect,
    Backup {
        path: String,
    },
    #[cfg(any(feature = "full-text", feature = "vector-search"))]
    Retrieve {
        request: serde_json::Value,
    },
}
fn values(params: Vec<serde_json::Value>) -> Result<Vec<Value>> {
    if params.len() > 256 {
        return Err(PvError::ResourceLimit("at most 256 parameters".into()));
    }
    params
        .into_iter()
        .map(|v| super::json_to_value(v).map_err(PvError::Query))
        .collect()
}

/// Execute a bounded JSON query/batch/inspect/backup/retrieve request.
/// Caller frees the returned JSON with pv_string_free.
/// # Safety
/// vault must be NULL or live; request must be NULL or valid NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn pv_vault_request(
    vault: *mut PvVault,
    request: *const c_char,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        clear_last_error();
        let result = (|| -> Result<String> {
            let vault = unsafe { vault.as_mut() }
                .ok_or_else(|| PvError::Query("vault is closed".into()))?;
            let request = unsafe { cstr_to_str(request) }
                .ok_or_else(|| PvError::Query("invalid request".into()))?;
            if request.len() > 1024 * 1024 {
                return Err(PvError::ResourceLimit("vault request exceeds 1 MiB".into()));
            }
            match serde_json::from_str::<Request>(request)? {
                Request::Query { sql, params } => Ok(crate::json::result_to_string(
                    &vault.inner.query(&sql, &values(params)?)?,
                )?),
                Request::Batch { commands } => {
                    let commands = commands
                        .into_iter()
                        .map(|c| Ok((c.sql, values(c.params)?)))
                        .collect::<Result<Vec<_>>>()?;
                    let result = vault.inner.execute_batch(&commands)?;
                    Ok(serde_json::json!({"committed":true,"statements":result.len()}).to_string())
                }
                Request::Inspect => Ok(serde_json::to_string(&vault.inner.inspect()?)?),
                Request::Backup { path } => {
                    vault.inner.backup(path)?;
                    Ok("{\"verified\":true}".into())
                }
                #[cfg(any(feature = "full-text", feature = "vector-search"))]
                Request::Retrieve { request } => vault.inner.retrieve_json(&request.to_string()),
            }
        })();
        match result {
            Ok(json) => string_to_c(json),
            Err(e) => {
                set_last_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// Re-encrypt with new key material; old backups retain their previous keys.
/// # Safety
/// vault must be NULL or live; bytes must cover secret_len readable bytes.
#[no_mangle]
pub unsafe extern "C" fn pv_vault_rotate(
    vault: *mut PvVault,
    bytes: *const u8,
    secret_len: usize,
    password: i32,
) -> i32 {
    guard(0, || {
        clear_last_error();
        let result = (|| -> Result<()> {
            let vault = unsafe { vault.as_mut() }
                .ok_or_else(|| PvError::Query("vault is closed".into()))?;
            vault
                .inner
                .rotate_key(unsafe { secret(bytes, secret_len, password) }?)
        })();
        match result {
            Ok(()) => 1,
            Err(e) => {
                set_last_error(e.to_string());
                0
            }
        }
    })
}
/// Release the vault lock and owned secrets. Does not implicitly save changes.
/// # Safety
/// vault must be NULL or a live handle, and must not be used after this call.
#[no_mangle]
pub unsafe extern "C" fn pv_vault_close(vault: *mut PvVault) {
    guard((), || {
        if !vault.is_null() {
            drop(unsafe { Box::from_raw(vault) });
        }
    })
}
