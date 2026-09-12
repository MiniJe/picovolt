// SPDX-License-Identifier: LicenseRef-PicoVolt-Proprietary-1.0
//! Native encryption CLI. Secrets are read from files, never command arguments.
use super::{json_to_value, open_existing_database, print_result, CliResult};
use picovolt::encryption::{self, Secret, Vault};
use std::fs::File;
use std::io::Read;

pub const HELP: &str = "Encryption (native, default feature):
  pv crypto keygen <new-key-file>
  pv crypto inspect <encrypted-file>
  pv crypto seal <database> <new-encrypted-file> <secret-option>
  pv crypto verify <encrypted-file> <secret-option>
  pv crypto restore <backup> <new-encrypted-file> <secret-option>
  pv vault create <new-vault> <secret-option>
  pv vault query <vault> <secret-option> <SELECT SQL>
  pv vault batch <vault> <secret-option> <commands.json>
  pv vault retrieve <vault> <secret-option> <request.json>
  pv vault inspect <vault> <secret-option>
  pv vault backup <vault> <secret-option> <new-backup>
  pv vault rotate <vault> <secret-option> --new-key-file <key>
  Secret option: --key-file <raw-32-byte-file> OR --password-file <file>.
  Password bytes are exact, including newlines. New output files never overwrite.
  Keep keys outside vault/backup directories. Rotation does not re-key old backups.";

fn secret(args: &mut Vec<String>) -> CliResult<Secret> {
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, s)| s.as_str() == "--key-file" || s.as_str() == "--password-file")
        .map(|(i, _)| i)
        .collect();
    if positions.len() != 1 {
        return Err("supply exactly one --key-file or --password-file".into());
    }
    let pos = positions[0];
    if pos + 1 >= args.len() {
        return Err("secret file path is missing".into());
    }
    let flag = args.remove(pos);
    let path = args.remove(pos);
    Ok(if flag == "--key-file" {
        Secret::from_key_file(path)?
    } else {
        Secret::from_password_file(path)?
    })
}
fn read_json(path: &str) -> CliResult<String> {
    let mut text = String::new();
    File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 1024 * 1024 {
        return Err("request exceeds 1 MiB".into());
    }
    Ok(text)
}
pub fn crypto(args: &[String]) -> CliResult<()> {
    if args.first().is_some_and(|s| s == "keygen") && args.len() == 2 {
        Secret::generate()?.write_key_file(&args[1])?;
        println!("Created key file; store a separate protected recovery copy.");
        return Ok(());
    }
    if args.first().is_some_and(|s| s == "inspect") && args.len() == 2 {
        println!(
            "{}",
            serde_json::to_string_pretty(&encryption::inspect(&encryption::read_file(&args[1])?)?)?
        );
        return Ok(());
    }
    let mut args = args.to_vec();
    let secret = secret(&mut args)?;
    match args.first().map(String::as_str) {
        Some("seal") if args.len() == 3 => {
            let mut db = open_existing_database(&args[1])?;
            encryption::save_new(&mut db, &args[2], &secret)?;
            println!("Encrypted snapshot written.");
        }
        Some("verify") if args.len() == 2 => {
            let bytes = encryption::read_file(&args[1])?;
            let db = encryption::open(&bytes, &secret)?;
            let stats = db.inspect_stats()?;
            println!(
                "{}",
                serde_json::json!({"authenticated":true,"database_valid":true,"transaction":stats.current_transaction})
            );
        }
        Some("restore") if args.len() == 3 => {
            let mut db = encryption::open(&encryption::read_file(&args[1])?, &secret)?;
            db.inspect_stats()?;
            encryption::save_new(&mut db, &args[2], &secret)?;
            println!("Restored to a new encrypted vault file.");
        }
        _ => return Err(HELP.into()),
    }
    Ok(())
}
pub fn vault(args: &[String]) -> CliResult<()> {
    let mut args = args.to_vec();
    let secret = secret(&mut args)?;
    if args.len() < 2 {
        return Err(HELP.into());
    }
    if args[0] == "create" && args.len() == 2 {
        Vault::create(&args[1], secret)?;
        println!("Encrypted vault created.");
        return Ok(());
    }
    let mut db = Vault::open(&args[1], secret)?;
    match args[0].as_str() {
        "query" if args.len() >= 3 => print_result(db.query(&args[2..].join(" "), &[])?)?,
        "batch" if args.len() == 3 => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Command {
                sql: String,
                #[serde(default)]
                params: Vec<serde_json::Value>,
            }
            let commands: Vec<Command> = serde_json::from_str(&read_json(&args[2])?)?;
            let statements = commands
                .into_iter()
                .map(|c| {
                    Ok((
                        c.sql,
                        c.params
                            .iter()
                            .map(json_to_value)
                            .collect::<CliResult<Vec<_>>>()?,
                    ))
                })
                .collect::<CliResult<Vec<_>>>()?;
            let results = db.execute_batch(&statements)?;
            println!(
                "{}",
                serde_json::json!({"committed":true,"statements":results.len()})
            );
        }
        #[cfg(any(feature = "full-text", feature = "vector-search"))]
        "retrieve" if args.len() == 3 => println!("{}", db.retrieve_json(&read_json(&args[2])?)?),
        "inspect" if args.len() == 2 => {
            println!("{}", serde_json::to_string_pretty(&db.inspect()?)?)
        }
        "backup" if args.len() == 3 => {
            db.backup(&args[2])?;
            println!("Verified encrypted backup written.");
        }
        "rotate" if args.len() == 4 && args[2] == "--new-key-file" => {
            db.rotate_key(Secret::from_key_file(&args[3])?)?;
            println!("Vault re-encrypted with new key. Existing backups retain their old key.");
        }
        _ => return Err(HELP.into()),
    }
    Ok(())
}
