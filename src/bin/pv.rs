//! `pv` is the batteries-included PicoVolt command-line interface.

use picovolt::{Database, PvError, QueryResult, Value};
use serde_json::{Map, Value as JsonValue};
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

type CliResult<T> = Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = run(std::env::args().skip(1).collect()) {
        eprintln!("pv: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> CliResult<()> {
    match args.first().map(String::as_str) {
        Some("query") if args.len() >= 3 => {
            let mut db = open_database(&args[1])?;
            print_result(db.query(&args[2..].join(" "))?)
        }
        Some("inspect") if args.len() == 2 => {
            let db = open_database(&args[1])?;
            println!("transaction: {}", db.current_tx());
            for table in db.table_names() {
                println!("table: {table}");
            }
            Ok(())
        }
        Some("history") if args.len() >= 2 => history_command(&args[1..]),
        Some("bake") if args.len() == 3 => {
            let mut db = Database::open_dev(&args[1])?;
            db.bake(&args[2])?;
            println!("wrote {}", args[2]);
            Ok(())
        }
        Some("import") => import_command(&args[1..]),
        Some("export") => export_command(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            Ok(())
        }
        Some(command) => {
            Err(format!("unknown or incomplete command `{command}`; run `pv help`").into())
        }
    }
}

fn history_command(args: &[String]) -> CliResult<()> {
    let db = open_database(&args[0])?;
    let tables = match option(args, "--table") {
        Some(table) => vec![table.to_owned()],
        None => db.table_names(),
    };
    let limit = option(args, "--limit")
        .map(str::parse::<u64>)
        .transpose()?
        .unwrap_or(20);
    println!("transaction,table,rows");
    for (tx, table, rows) in history_rows(&db, &tables, limit)? {
        println!("{tx},{},{}", csv_escape(&table), rows);
    }
    Ok(())
}

fn history_rows(
    db: &Database,
    tables: &[String],
    limit: u64,
) -> Result<Vec<(u64, String, usize)>, PvError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let current = db.current_tx();
    let first = current.saturating_sub(limit.saturating_sub(1));
    let mut history = Vec::new();
    for tx in first..=current {
        for table in tables {
            let (_, rows) = db.select(table, Some(tx))?;
            history.push((tx, table.clone(), rows.len()));
        }
    }
    Ok(history)
}

fn open_database(path: &str) -> Result<Database, PvError> {
    if Path::new(path).is_file() {
        Database::open_prod(path)
    } else {
        Database::open_dev(path)
    }
}

fn import_command(args: &[String]) -> CliResult<()> {
    if args.len() < 2 {
        return Err(
            "usage: pv import <workspace> <input> [--table name] [--format csv|jsonl|sql]".into(),
        );
    }
    let workspace = &args[0];
    let input = PathBuf::from(&args[1]);
    let format = option(args, "--format")
        .map(str::to_owned)
        .or_else(|| {
            input
                .extension()
                .and_then(|v| v.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "sql".into())
        .to_ascii_lowercase();
    let mut db = Database::open_dev(workspace)?;
    let imported = match format.as_str() {
        "sql" => {
            let report = db.import_sql(&fs::read_to_string(&input)?);
            for skipped in &report.skipped {
                eprintln!("skipped: {skipped}");
            }
            for failure in &report.errors {
                eprintln!("error: {failure}");
            }
            if !report.errors.is_empty() {
                return Err(format!("{} SQL statement(s) failed", report.errors.len()).into());
            }
            report.executed
        }
        "csv" => import_csv(&mut db, required_option(args, "--table")?, &input)?,
        "json" | "jsonl" | "ndjson" => {
            import_jsonl(&mut db, required_option(args, "--table")?, &input)?
        }
        _ => return Err(format!("unsupported import format `{format}`").into()),
    };
    println!("imported {imported} row(s)/statement(s)");
    Ok(())
}

fn export_command(args: &[String]) -> CliResult<()> {
    if args.len() < 3 {
        return Err("usage: pv export <database> <table> <output> [--format csv|jsonl]".into());
    }
    let mut db = open_database(&args[0])?;
    let output = PathBuf::from(&args[2]);
    let format = option(args, "--format")
        .map(str::to_owned)
        .or_else(|| {
            output
                .extension()
                .and_then(|v| v.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "csv".into())
        .to_ascii_lowercase();
    let QueryResult::Rows { columns, rows } = db.query(&format!("SELECT * FROM {}", args[1]))?
    else {
        unreachable!("SELECT always returns rows")
    };
    let mut out = BufWriter::new(File::create(&output)?);
    match format.as_str() {
        "csv" => write_csv(&mut out, &columns, &rows)?,
        "json" | "jsonl" | "ndjson" => write_jsonl(&mut out, &columns, &rows)?,
        _ => return Err(format!("unsupported export format `{format}`").into()),
    }
    out.flush()?;
    println!("exported {} row(s) to {}", rows.len(), output.display());
    Ok(())
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn required_option<'a>(args: &'a [String], name: &str) -> CliResult<&'a str> {
    option(args, name).ok_or_else(|| format!("missing required option `{name}`").into())
}

fn import_csv(db: &mut Database, table: &str, path: &Path) -> CliResult<usize> {
    let mut lines = BufReader::new(File::open(path)?).lines();
    let header = lines.next().ok_or("CSV input is empty")??;
    let columns = parse_csv_record(&header)?;
    create_table_if_missing(db, table, &columns)?;
    let placeholders = vec!["?"; columns.len()].join(", ");
    let sql = format!("INSERT INTO {table} VALUES ({placeholders})");
    let mut count = 0;
    for (line_number, line) in lines.enumerate() {
        let cells = parse_csv_record(&line?)?;
        if cells.len() != columns.len() {
            return Err(format!(
                "CSV line {} has {} fields; expected {}",
                line_number + 2,
                cells.len(),
                columns.len()
            )
            .into());
        }
        let values: Vec<Value> = cells.into_iter().map(|cell| parse_scalar(&cell)).collect();
        db.query_with(&sql, &values)?;
        count += 1;
    }
    Ok(count)
}

fn parse_csv_record(line: &str) -> CliResult<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(ch),
        }
    }
    if quoted {
        return Err("unterminated CSV quote (multiline fields are not supported)".into());
    }
    fields.push(field);
    Ok(fields)
}

fn import_jsonl(db: &mut Database, table: &str, path: &Path) -> CliResult<usize> {
    let reader = BufReader::new(File::open(path)?);
    let mut columns: Option<Vec<String>> = None;
    let mut count = 0;
    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let JsonValue::Object(object) = serde_json::from_str(&line)? else {
            return Err(format!("JSONL line {} is not an object", line_number + 1).into());
        };
        let names = columns.get_or_insert_with(|| object.keys().cloned().collect());
        if count == 0 {
            create_table_if_missing(db, table, names)?;
        }
        let values: Vec<Value> = names
            .iter()
            .map(|name| json_to_value(object.get(name).unwrap_or(&JsonValue::Null)))
            .collect::<CliResult<_>>()?;
        let placeholders = vec!["?"; names.len()].join(", ");
        db.query_with(
            &format!("INSERT INTO {table} VALUES ({placeholders})"),
            &values,
        )?;
        count += 1;
    }
    Ok(count)
}

fn create_table_if_missing(db: &mut Database, table: &str, columns: &[String]) -> CliResult<()> {
    if !db.table_names().iter().any(|name| name == table) {
        db.query(&format!("CREATE TABLE {table} ({})", columns.join(", ")))?;
    }
    Ok(())
}

fn parse_scalar(value: &str) -> Value {
    if value.is_empty() || value.eq_ignore_ascii_case("null") {
        Value::Null
    } else if let Ok(value) = value.parse::<i64>() {
        Value::Int(value)
    } else if let Ok(value) = value.parse::<f64>() {
        Value::Decimal((value * 1_000_000.0).round() as i128)
    } else {
        Value::Text(value.to_owned())
    }
}

fn json_to_value(value: &JsonValue) -> CliResult<Value> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Int(i64::from(*value)),
        JsonValue::Number(value) if value.is_i64() => Value::Int(value.as_i64().unwrap()),
        JsonValue::Number(value) => Value::Decimal(
            (value.as_f64().ok_or("JSON number cannot be represented")? * 1_000_000.0).round()
                as i128,
        ),
        JsonValue::String(value) => Value::Text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => Value::Text(serde_json::to_string(value)?),
    })
}

fn write_csv(out: &mut impl Write, columns: &[String], rows: &[Vec<Value>]) -> CliResult<()> {
    writeln!(
        out,
        "{}",
        columns
            .iter()
            .map(|v| csv_escape(v))
            .collect::<Vec<_>>()
            .join(",")
    )?;
    for row in rows {
        writeln!(
            out,
            "{}",
            row.iter().map(value_to_csv).collect::<Vec<_>>().join(",")
        )?;
    }
    Ok(())
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn value_to_csv(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Text(value) => csv_escape(value),
        _ => csv_escape(&value.to_string()),
    }
}

fn write_jsonl(out: &mut impl Write, columns: &[String], rows: &[Vec<Value>]) -> CliResult<()> {
    for row in rows {
        let object: Map<String, JsonValue> = columns
            .iter()
            .cloned()
            .zip(row.iter().map(value_to_json))
            .collect();
        serde_json::to_writer(&mut *out, &JsonValue::Object(object))?;
        writeln!(out)?;
    }
    Ok(())
}

fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Int(value) => JsonValue::from(*value),
        Value::Decimal(value) => JsonValue::from(*value as f64 / 1_000_000.0),
        Value::Text(value) => JsonValue::from(value.clone()),
        Value::Blob(value) => JsonValue::from(
            value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
    }
}

fn print_result(result: QueryResult) -> CliResult<()> {
    match result {
        QueryResult::Rows { columns, rows } => write_csv(&mut std::io::stdout(), &columns, &rows),
        QueryResult::Mutated(count) => {
            println!("{count} row(s) affected");
            Ok(())
        }
        QueryResult::Done => {
            println!("ok");
            Ok(())
        }
    }
}

fn print_help() {
    println!("PicoVolt command-line interface\n");
    println!("  pv query <database> <sql>");
    println!("  pv inspect <database>");
    println!("  pv history <database> [--table name] [--limit transactions]");
    println!("  pv bake <workspace> <output.pvdb>");
    println!("  pv import <workspace> <input> [--table name] [--format csv|jsonl|sql]");
    println!("  pv export <database> <table> <output> [--format csv|jsonl]");
    println!("\nA database path ending in a file is opened read-only; a directory is a writable workspace.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_and_scalars() {
        assert_eq!(
            parse_csv_record("1,\"a,b\",\"c\"\"d\"").unwrap(),
            ["1", "a,b", "c\"d"]
        );
        assert_eq!(parse_scalar("42"), Value::Int(42));
        assert_eq!(parse_scalar(""), Value::Null);
    }

    #[test]
    fn csv_and_jsonl_import_export_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let csv = temp.path().join("users.csv");
        std::fs::write(&csv, "id,name\n1,Ada\n2,\"Lin, Jr\"\n").unwrap();
        let mut db = Database::open_memory();
        assert_eq!(import_csv(&mut db, "users", &csv).unwrap(), 2);

        let mut exported = Vec::new();
        let QueryResult::Rows { columns, rows } =
            db.query("SELECT * FROM users ORDER BY id").unwrap()
        else {
            panic!("expected rows")
        };
        write_jsonl(&mut exported, &columns, &rows).unwrap();
        let text = String::from_utf8(exported).unwrap();
        assert!(text.contains(r#"{"id":1,"name":"Ada"}"#));
        assert!(text.contains(r#"{"id":2,"name":"Lin, Jr"}"#));
    }

    #[test]
    fn history_reports_recent_snapshot_counts() {
        let mut db = Database::open_memory();
        db.query("CREATE TABLE events (id)").unwrap();
        db.query("INSERT INTO events VALUES (1), (2)").unwrap();
        let history = history_rows(&db, &["events".into()], 2).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history.last().unwrap().2, 2);
        assert!(history[0].2 <= history[1].2);
    }
}
