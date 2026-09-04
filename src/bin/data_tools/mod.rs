//! Native data movement. Imports create new tables in one transaction, and
//! exports atomically replace their destination only after successful encoding.
//! SQLite import copies ordinary table data, not indexes, triggers, constraints,
//! views, or application behavior. See `docs/DATA_TOOLS.md` for type mappings.

use arrow_array::*;
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::{arrow_reader::ParquetRecordBatchReaderBuilder, ArrowWriter};
use parquet::file::properties::WriterProperties;
use picovolt::{Database, PvError, Result, Row, Value};
use std::{fs::File, path::Path, sync::Arc};

pub mod dataset;

fn external(error: impl std::fmt::Display) -> PvError {
    PvError::Schema(format!("data conversion: {error}"))
}

/// Import an ordinary table from a binary SQLite database, opened read-only.
/// Source and destination names are independent. The destination must be new.
pub fn import_sqlite(db: &mut Database, input: &Path, source: &str, target: &str) -> Result<u64> {
    use rusqlite::{types::ValueRef, Connection, OpenFlags};
    let connection = Connection::open_with_flags(
        input,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(external)?;
    connection
        .execute_batch("PRAGMA trusted_schema=OFF; BEGIN")
        .map_err(external)?;
    let ordinary: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main' AND name=?1 AND type='table')", [source], |r| r.get(0)).map_err(external)?;
    if !ordinary {
        return Err(external("source must be an ordinary SQLite table"));
    }
    let quoted = source.replace('"', "\"\"");
    let mut statement = connection
        .prepare(&format!("SELECT * FROM main.\"{quoted}\""))
        .map_err(external)?;
    let columns = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let width = columns.len();
    db.transaction(|db| {
        db.create_table(target, columns)?;
        let mut count = 0;
        let mut rows = statement.query([]).map_err(external)?;
        while let Some(row) = rows.next().map_err(external)? {
            let values = (0..width)
                .map(|i| {
                    Ok(match row.get_ref(i).map_err(external)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(v) => Value::Int(v),
                        ValueRef::Real(v) => float_value(v)?,
                        ValueRef::Text(v) => {
                            Value::Text(std::str::from_utf8(v).map_err(external)?.into())
                        }
                        ValueRef::Blob(v) => Value::Blob(v.to_vec()),
                    })
                })
                .collect::<Result<Row>>()?;
            db.insert(target, values)?;
            count += 1;
        }
        Ok(count)
    })
}

fn float_value(v: f64) -> Result<Value> {
    if !v.is_finite() {
        return Err(external("NaN and infinity are not supported"));
    }
    // Text conversion avoids an unchecked saturating float-to-i128 cast.
    let text = format!("{v:.6}");
    let mantissa = text.replace('.', "").parse::<i128>().map_err(external)?;
    Ok(Value::Decimal(mantissa))
}

fn scaled_decimal(value: i128, scale: i8) -> Result<Value> {
    let shift = 6i16 - i16::from(scale);
    let mantissa = if shift >= 0 {
        value
            .checked_mul(
                10i128
                    .checked_pow(shift as u32)
                    .ok_or_else(|| external("decimal scale overflow"))?,
            )
            .ok_or_else(|| external("decimal overflow"))?
    } else {
        let divisor = 10i128
            .checked_pow((-shift) as u32)
            .ok_or_else(|| external("decimal scale overflow"))?;
        if value % divisor != 0 {
            return Err(external(
                "decimal has precision beyond six fractional digits",
            ));
        }
        value / divisor
    };
    Ok(Value::Decimal(mantissa))
}

fn arrow_value(array: &dyn Array, i: usize) -> Result<Value> {
    if array.is_null(i) {
        return Ok(Value::Null);
    }
    macro_rules! primitive {
        ($ty:ty, $convert:expr) => {{
            let a = array
                .as_any()
                .downcast_ref::<$ty>()
                .ok_or_else(|| external("Arrow type mismatch"))?;
            $convert(a.value(i))
        }};
    }
    Ok(match array.data_type() {
        DataType::Null => Value::Null,
        DataType::Boolean => primitive!(BooleanArray, |v| Value::Int(i64::from(v))),
        DataType::Int8 => primitive!(Int8Array, |v| Value::Int(i64::from(v))),
        DataType::Int16 => primitive!(Int16Array, |v| Value::Int(i64::from(v))),
        DataType::Int32 => primitive!(Int32Array, |v| Value::Int(i64::from(v))),
        DataType::Int64 => primitive!(Int64Array, Value::Int),
        DataType::UInt8 => primitive!(UInt8Array, |v| Value::Int(i64::from(v))),
        DataType::UInt16 => primitive!(UInt16Array, |v| Value::Int(i64::from(v))),
        DataType::UInt32 => primitive!(UInt32Array, |v| Value::Int(i64::from(v))),
        DataType::UInt64 => primitive!(UInt64Array, |v| i64::try_from(v)
            .map(Value::Int)
            .map_err(external))?,
        DataType::Float32 => primitive!(Float32Array, |v| float_value(f64::from(v)))?,
        DataType::Float64 => primitive!(Float64Array, float_value)?,
        DataType::Decimal128(_, scale) => {
            primitive!(Decimal128Array, |v| scaled_decimal(v, *scale))?
        }
        DataType::Utf8 => primitive!(StringArray, |v: &str| Value::Text(v.into())),
        DataType::LargeUtf8 => primitive!(LargeStringArray, |v: &str| Value::Text(v.into())),
        DataType::Binary => primitive!(BinaryArray, |v: &[u8]| Value::Blob(v.into())),
        DataType::LargeBinary => primitive!(LargeBinaryArray, |v: &[u8]| Value::Blob(v.into())),
        DataType::FixedSizeBinary(_) => {
            primitive!(FixedSizeBinaryArray, |v: &[u8]| Value::Blob(v.into()))
        }
        other => {
            return Err(external(format!(
                "unsupported Parquet/Arrow type {other:?}"
            )))
        }
    })
}

/// Import a flat Parquet file in batches of 1024 rows into a new table.
/// Unsupported logical types fail explicitly and roll back the entire import.
pub fn import_parquet(db: &mut Database, input: &Path, table: &str) -> Result<u64> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(input)?).map_err(external)?;
    let columns = builder
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect::<Vec<_>>();
    // Validate even empty files, for which no row conversion will run.
    for field in builder.schema().fields() {
        match field.data_type() {
            DataType::Null
            | DataType::Boolean
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::Decimal128(_, _)
            | DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Binary
            | DataType::LargeBinary
            | DataType::FixedSizeBinary(_) => (),
            other => {
                return Err(external(format!(
                    "unsupported Parquet/Arrow type {other:?}"
                )))
            }
        }
    }
    let reader = builder.with_batch_size(1024).build().map_err(external)?;
    db.transaction(|db| {
        db.create_table(table, columns)?;
        let mut count = 0;
        for batch in reader {
            let batch = batch.map_err(external)?;
            for i in 0..batch.num_rows() {
                let row = batch
                    .columns()
                    .iter()
                    .map(|a| arrow_value(a.as_ref(), i))
                    .collect::<Result<Row>>()?;
                db.insert(table, row)?;
                count += 1;
            }
        }
        Ok(count)
    })
}

fn value_type(value: &Value) -> DataType {
    match value {
        Value::Null => DataType::Null,
        Value::Int(_) => DataType::Int64,
        Value::Decimal(_) => DataType::Decimal128(38, 6),
        Value::Text(_) => DataType::Utf8,
        Value::Blob(_) => DataType::Binary,
    }
}

fn batch_from_rows(schema: Arc<Schema>, rows: &[Row]) -> Result<RecordBatch> {
    let mut arrays: Vec<ArrayRef> = Vec::new();
    for (i, field) in schema.fields().iter().enumerate() {
        arrays.push(match field.data_type() {
            DataType::Int64 => Arc::new(Int64Array::from(
                rows.iter()
                    .map(|r| {
                        if let Value::Int(v) = r[i] {
                            Some(v)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>(),
            )),
            DataType::Decimal128(_, _) => {
                let values = rows
                    .iter()
                    .map(|r| match r[i] {
                        Value::Decimal(v) => Some(v),
                        Value::Int(v) => Some(i128::from(v) * 1_000_000),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let array = Decimal128Array::from(values)
                    .with_precision_and_scale(38, 6)
                    .map_err(external)?;
                array.validate_decimal_precision(38).map_err(external)?;
                Arc::new(array)
            }
            DataType::Binary => Arc::new(BinaryArray::from_iter(rows.iter().map(|r| {
                if let Value::Blob(v) = &r[i] {
                    Some(v.as_slice())
                } else {
                    None
                }
            }))),
            _ => Arc::new(StringArray::from_iter(rows.iter().map(|r| {
                if let Value::Text(v) = &r[i] {
                    Some(v.as_str())
                } else {
                    None
                }
            }))),
        });
    }
    RecordBatch::try_new(schema, arrays).map_err(external)
}

/// Export one snapshot to interoperable Parquet. A first streaming pass infers
/// column types; a second writes bounded batches. Mixed nonnumeric types in one
/// column are rejected rather than silently coerced. Destination replacement is
/// atomic. All-null columns are exported as nullable UTF-8.
pub fn export_parquet(
    db: &Database,
    table: &str,
    output: &Path,
    before: Option<u64>,
) -> Result<u64> {
    let columns = db.column_names(table)?;
    if before.is_some_and(|tx| tx > db.current_tx()) {
        return Err(external("snapshot is in the future"));
    }
    let mut types = vec![DataType::Null; columns.len()];
    db.for_each_row(table, before, |row| {
        for (i, value) in row.iter().enumerate() {
            let incoming = value_type(value);
            if incoming == DataType::Null {
                continue;
            }
            if types[i] == DataType::Null {
                types[i] = incoming;
            } else if types[i] != incoming {
                if matches!(
                    (&types[i], &incoming),
                    (DataType::Int64, DataType::Decimal128(_, _))
                        | (DataType::Decimal128(_, _), DataType::Int64)
                ) {
                    types[i] = DataType::Decimal128(38, 6);
                } else {
                    return Err(external(format!(
                        "column `{}` mixes incompatible types",
                        columns[i]
                    )));
                }
            }
        }
        Ok(())
    })?;
    let schema = Arc::new(Schema::new(
        columns
            .into_iter()
            .zip(types)
            .map(|(name, ty)| {
                Field::new(
                    name,
                    if ty == DataType::Null {
                        DataType::Utf8
                    } else {
                        ty
                    },
                    true,
                )
            })
            .collect::<Vec<_>>(),
    ));
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let properties = WriterProperties::builder()
        .set_max_row_group_row_count(Some(1024))
        .build();
    let mut writer =
        ArrowWriter::try_new(temporary.as_file_mut(), schema.clone(), Some(properties))
            .map_err(external)?;
    let mut rows = Vec::with_capacity(1024);
    let mut count = 0;
    db.for_each_row(table, before, |row| {
        rows.push(row.clone());
        count += 1;
        if rows.len() == 1024 {
            writer
                .write(&batch_from_rows(schema.clone(), &rows)?)
                .map_err(external)?;
            rows.clear();
        }
        Ok(())
    })?;
    if !rows.is_empty() {
        writer
            .write(&batch_from_rows(schema, &rows)?)
            .map_err(external)?;
    }
    writer.close().map_err(external)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(output)
        .map_err(|e| PvError::Io(e.error))?;
    Ok(count)
}
