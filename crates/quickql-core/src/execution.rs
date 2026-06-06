use crate::csv::{csv_fields_from_source, load_csv_source};
use crate::json::{load_json_http_source, load_json_source};
use crate::parsing::{parse_query, parse_query_lenient};
use crate::{
    CaluculatedValue, KeyDescriptor, MapExpr, Query, QueryResult, SortDirection, SortKey,
    StreamMessage, SubQuery, ALL_COLUMNS,
};
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub fn stream_query_jsonl<W: Write>(
    query_path: &Path,
    writer: &mut W,
    batch_size: usize,
) -> Result<()> {
    let start = Instant::now();
    let query_text = fs::read_to_string(query_path)
        .with_context(|| format!("Reading query file {}", query_path.display()))?;
    let query = parse_query(&query_text)?;
    let result = execute_pipeline(&query, query_path)
        .with_context(|| format!("Executing pipeline {}", query_path.display()))?;
    let columns = columns_from_descriptor(&result.columns);

    write_stream_message(
        writer,
        &StreamMessage::Meta {
            columns: &columns,
            source: query_path.display().to_string(),
        },
    )?;

    let mut row_count = 0usize;
    let mut batch_start = 0usize;
    let mut batch: Vec<Value> = Vec::with_capacity(batch_size.max(1));

    for row in result.rows {
        if batch.is_empty() {
            batch_start = row_count;
        }
        batch.push(row);
        row_count += 1;
        if batch.len() >= batch_size.max(1) {
            write_stream_message(
                writer,
                &StreamMessage::Batch {
                    start: batch_start,
                    rows: &batch,
                },
            )?;
            batch.clear();
        }
    }

    if !batch.is_empty() {
        write_stream_message(
            writer,
            &StreamMessage::Batch {
                start: batch_start,
                rows: &batch,
            },
        )?;
    }

    write_stream_message(
        writer,
        &StreamMessage::Done {
            row_count,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        },
    )?;
    writer.flush()?;
    Ok(())
}

pub fn json_fields_for_query(query_path: &Path, query_text: &str) -> Result<Vec<String>> {
    let query = parse_query_lenient(query_text)?;
    if query.sources.is_empty() {
        return Ok(Vec::new());
    }

    fields_from_sources(query_path, &query.sources)
}

pub fn source_path_for_query(query_path: &Path, query_text: &str) -> Result<Option<PathBuf>> {
    let query = parse_query_lenient(query_text)?;
    Ok(query
        .sources
        .first()
        .map(String::as_str)
        .map(|source| resolve_source(query_path, source)))
}

pub fn json_fields_from_source_sample(source_path: &Path, max_rows: usize) -> Result<Vec<String>> {
    if is_csv_path(source_path) {
        return csv_fields_from_source(source_path);
    }
    if is_ql_path(source_path) {
        return fields_from_ql_source(source_path);
    }

    let mut file = File::open(source_path)
        .with_context(|| format!("Opening JSON source {}", source_path.display()))?;
    let sample_bytes = (max_rows.max(1) * 4096).clamp(64 * 1024, 1024 * 1024);
    let mut buffer = vec![0u8; sample_bytes];
    let read = file.read(&mut buffer)?;
    buffer.truncate(read);
    Ok(fields_from_json_prefix(&String::from_utf8_lossy(&buffer)))
}

pub fn fields_from_source_sample(source_path: &Path, max_rows: usize) -> Result<Vec<String>> {
    if is_csv_path(source_path) {
        csv_fields_from_source(source_path)
    } else if is_ql_path(source_path) {
        fields_from_ql_source(source_path)
    } else {
        json_fields_from_source_sample(source_path, max_rows)
    }
}

fn fields_from_sources(query_path: &Path, sources: &[String]) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    for source in sources {
        let source_path = resolve_source(query_path, source);
        let source_fields = fields_from_source_sample(&source_path, 100)?;
        for field in source_fields {
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    Ok(fields)
}

pub(crate) fn execute_pipeline(query: &Query, query_path: &Path) -> Result<QueryResult> {
    let mut ql_stack = Vec::new();
    if let Ok(canonical) = fs::canonicalize(query_path) {
        ql_stack.push(canonical);
    }
    execute_pipeline_with_stack(query, query_path, &mut ql_stack)
}

fn execute_pipeline_with_stack(
    query: &Query,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
) -> Result<QueryResult> {
    let mut result = QueryResult::default();

    for step in &query.steps {
        result = match step {
            SubQuery::Source(sources) => load_sources(query_path, sources, ql_stack)?,
            SubQuery::Map(mapping) => apply_map(result, mapping),
            SubQuery::Filter(filter) => apply_filter(result, filter),
            SubQuery::MapMany(field) => apply_map_many(result, field)?,
            SubQuery::GroupBy { keys, mapping } => apply_group_by(result, keys, mapping)?,
            SubQuery::OrderBy(sort_keys) => apply_order_by(result, sort_keys),
        };
    }

    Ok(result)
}

fn apply_map(result: QueryResult, mapping: &[MapExpr]) -> QueryResult {
    if mapping.len() == 1 && matches!(mapping[0], MapExpr::All) {
        return result;
    }

    let rows = result
        .rows
        .iter()
        .map(|row| {
            let mut output = Map::new();
            for expr in mapping {
                match expr {
                    MapExpr::All => {
                        if let Value::Object(map) = row {
                            output.extend(map.clone());
                        }
                    }
                    MapExpr::Specific { column, value } => {
                        set_path(&mut output, column, value.caluculate(row));
                    }
                }
            }
            Value::Object(output)
        })
        .collect();

    QueryResult::new(rows)
}

fn apply_filter(result: QueryResult, filter: &CaluculatedValue) -> QueryResult {
    let rows = result
        .rows
        .into_iter()
        .filter(|row| value_truthy(&filter.caluculate(row)))
        .collect();
    QueryResult::new(rows)
}

fn apply_map_many(result: QueryResult, field: &str) -> Result<QueryResult> {
    let path = path_parts(field);
    let mut rows = Vec::new();

    for row in result.rows {
        match get_path(&row, &path) {
            Value::Array(values) => rows.extend(values),
            Value::Null => {}
            value => bail!("MAP_MANY column '{field}' must be an array, got {value}"),
        }
    }

    Ok(QueryResult::new(rows))
}

fn load_sources(
    query_path: &Path,
    sources: &[CaluculatedValue],
    ql_stack: &mut Vec<PathBuf>,
) -> Result<QueryResult> {
    let mut rows = Vec::new();
    for source in sources {
        let source = source.caluculate(&Value::Null);
        let source = source
            .as_str()
            .with_context(|| format!("SOURCE value must be a string, got {source}"))?;
        rows.extend(load_query_source(query_path, source, ql_stack)?.rows);
    }
    Ok(QueryResult::new(rows))
}

fn load_query_source(
    query_path: &Path,
    source: &str,
    ql_stack: &mut Vec<PathBuf>,
) -> Result<QueryResult> {
    if is_http_uri(source) {
        return load_json_http_source(source);
    }

    let source_path = resolve_source(query_path, source);
    if is_csv_path(&source_path) {
        load_csv_source(&source_path)
    } else if is_ql_path(&source_path) {
        load_ql_source(&source_path, ql_stack)
    } else {
        load_json_source(&source_path)
    }
}

fn load_ql_source(path: &Path, ql_stack: &mut Vec<PathBuf>) -> Result<QueryResult> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("Resolving QL source {}", path.display()))?;
    if ql_stack.contains(&canonical) {
        bail!("Recursive QL source reference: {}", path.display());
    }

    let query_text = fs::read_to_string(path)
        .with_context(|| format!("Reading QL source {}", path.display()))?;
    let query = parse_query(&query_text)
        .with_context(|| format!("Parsing QL source {}", path.display()))?;

    ql_stack.push(canonical);
    let result = execute_pipeline_with_stack(&query, path, ql_stack)
        .with_context(|| format!("Executing QL source {}", path.display()));
    ql_stack.pop();
    result
}

fn fields_from_ql_source(path: &Path) -> Result<Vec<String>> {
    let mut ql_stack = Vec::new();
    let result = load_ql_source(path, &mut ql_stack)?;
    Ok(columns_from_descriptor(&result.columns))
}

fn apply_group_by(
    result: QueryResult,
    keys: &[String],
    mapping: &[MapExpr],
) -> Result<QueryResult> {
    let group_all = keys.len() == 1 && keys[0] == ALL_COLUMNS;
    let key_paths: Vec<Vec<String>> = if group_all {
        Vec::new()
    } else {
        keys.iter().map(|key| path_parts(key)).collect()
    };

    let mut group_order = Vec::new();
    let mut groups: HashMap<String, (Vec<Value>, Vec<Value>)> = HashMap::new();

    for row in result.rows {
        let key_values = if group_all {
            Vec::new()
        } else {
            key_paths
                .iter()
                .map(|path| get_path(&row, path))
                .collect::<Vec<_>>()
        };
        let key = if group_all {
            ALL_COLUMNS.to_string()
        } else {
            group_key(&key_values)
        };
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
            groups.insert(key.clone(), (key_values, Vec::new()));
        }
        groups.get_mut(&key).unwrap().1.push(row);
    }

    let mut output_rows = Vec::new();
    for key in group_order {
        let (key_values, rows) = groups.remove(&key).unwrap();
        let mut output = Map::new();

        if !group_all {
            for (key, value) in keys.iter().zip(key_values) {
                set_path(&mut output, &path_parts(key), value);
            }
        }

        let group_value = grouped_rows_value(&rows);
        for expr in mapping {
            match expr {
                MapExpr::All => {}
                MapExpr::Specific { column, value } => {
                    set_path(&mut output, column, value.caluculate(&group_value));
                }
            }
        }

        output_rows.push(Value::Object(output));
    }

    Ok(QueryResult::new(output_rows))
}

fn apply_order_by(result: QueryResult, sort_keys: &[SortKey]) -> QueryResult {
    let mut rows = result.rows;
    rows.sort_by(|a, b| {
        for key in sort_keys {
            let path = path_parts(&key.column);
            let ord = compare_values(&get_path(a, &path), &get_path(b, &path));
            let ord = if matches!(key.direction, SortDirection::Desc) {
                ord.reverse()
            } else {
                ord
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    QueryResult::new(rows)
}

fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&b.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (a, b) => type_rank(a).cmp(&type_rank(b)),
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}



pub fn parse_date_key(input: &str) -> Result<Option<i32>> {
    let date = input.split('T').next().unwrap_or("").trim();
    if date.is_empty() {
        return Ok(None);
    }

    let separator = if date.contains('.') {
        '.'
    } else if date.contains('-') {
        '-'
    } else {
        return Ok(None);
    };
    let parts: Vec<&str> = date.split(separator).collect();
    if parts.len() != 3 {
        return Ok(None);
    }

    let (year_part, month_part, day_part) = if separator == '-' && parts[0].len() == 4 {
        (parts[0], parts[1], parts[2])
    } else {
        (parts[2], parts[1], parts[0])
    };

    let day: u32 = day_part
        .parse()
        .with_context(|| format!("Parsing date day in '{input}'"))?;
    let month: u32 = month_part
        .parse()
        .with_context(|| format!("Parsing date month in '{input}'"))?;
    let year: i32 = year_part
        .parse()
        .with_context(|| format!("Parsing date year in '{input}'"))?;

    if !is_valid_date(year, month, day) {
        bail!("Invalid date '{input}'");
    }

    Ok(Some(year * 10_000 + month as i32 * 100 + day as i32))
}

fn is_valid_date(year: i32, month: u32, day: u32) -> bool {
    if month == 0 || month > 12 || day == 0 {
        return false;
    }
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return false,
    };
    day <= max_day
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn get_path(value: &Value, path: &[String]) -> Value {
    if path.is_empty() {
        return value.clone();
    }

    path.iter()
        .try_fold(value, |current, part| match current {
            Value::Object(map) => map.get(part),
            _ => None,
        })
        .cloned()
        .unwrap_or(Value::Null)
}

fn set_path(output: &mut Map<String, Value>, path: &[String], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };

    let mut current = output;
    for part in parents {
        let value = current
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !value.is_object() {
            *value = Value::Object(Map::new());
        }
        current = value.as_object_mut().unwrap();
    }
    current.insert(last.clone(), value);
}

fn grouped_rows_value(rows: &[Value]) -> Value {
    if rows.iter().any(|value| matches!(value, Value::Object(_)))
        && rows
            .iter()
            .all(|value| matches!(value, Value::Object(_) | Value::Null))
    {
        return Value::Object(grouped_object_rows(rows));
    }

    Value::Array(rows.to_vec())
}

fn grouped_object_rows(rows: &[Value]) -> Map<String, Value> {
    let keys = rows
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|object| object.keys().cloned())
        .collect::<BTreeSet<_>>();

    keys.into_iter()
        .map(|key| {
            let values = rows
                .iter()
                .map(|row| match row {
                    Value::Object(object) => object.get(&key).cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                })
                .collect::<Vec<_>>();
            (key, grouped_rows_value(&values))
        })
        .collect()
}

fn path_parts(input: &str) -> Vec<String> {
    input
        .split('.')
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Null => false,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn group_key(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| match value {
            Value::String(value) => format!("s\x01{value}\x02"),
            Value::Number(value) => format!("n\x01{value}\x02"),
            Value::Bool(value) => format!("b\x01{value}\x02"),
            Value::Null => "N\x01\x02".to_string(),
            value => format!("j\x01{value}\x02"),
        })
        .collect::<Vec<_>>()
        .concat()
}

fn columns_from_descriptor(descriptor: &KeyDescriptor) -> Vec<String> {
    fn collect(prefix: &str, descriptor: &KeyDescriptor, columns: &mut Vec<String>) {
        match descriptor {
            KeyDescriptor::Value => {
                if !prefix.is_empty() {
                    columns.push(prefix.to_string());
                }
            }
            KeyDescriptor::Object(fields) => {
                let mut keys = fields.keys().collect::<Vec<_>>();
                keys.sort();
                for key in keys {
                    let path = if prefix.is_empty() {
                        key.to_string()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    collect(&path, &fields[key], columns);
                }
            }
        }
    }

    let mut columns = Vec::new();
    collect("", descriptor, &mut columns);
    columns
}

fn fields_from_json_prefix(input: &str) -> Vec<String> {
    let bytes = input.as_bytes();
    let mut fields = BTreeSet::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }

        let start = i;
        i += 1;
        let mut escaped = false;
        while i < bytes.len() {
            if escaped {
                escaped = false;
            } else if bytes[i] == b'\\' {
                escaped = true;
            } else if bytes[i] == b'"' {
                break;
            }
            i += 1;
        }

        if i >= bytes.len() {
            break;
        }

        let end = i;
        i += 1;
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }

        if j < bytes.len() && bytes[j] == b':' {
            if let Ok(field) = serde_json::from_str::<String>(&input[start..=end]) {
                fields.insert(field);
            }
        }
    }

    fields.into_iter().collect()
}

fn resolve_source(query_path: &Path, source: &str) -> PathBuf {
    let source_path = PathBuf::from(source);
    if source_path.is_absolute() {
        source_path
    } else {
        query_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(source_path)
    }
}

fn write_stream_message<W: Write>(writer: &mut W, message: &StreamMessage<'_>) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn is_csv_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
}

fn is_ql_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ql"))
}

fn is_http_uri(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn group_by_maps_nested_fields_to_arrays() {
        let result = QueryResult::new(vec![
            json!({
                "text": "test",
                "object": {
                    "number": 42
                }
            }),
            json!({
                "text": "test2",
                "object": {
                    "number": 43
                }
            }),
        ]);
        let mapping = vec![
            MapExpr::Specific {
                column: vec!["text".to_string()],
                value: CaluculatedValue::Reference(vec!["text".to_string()]),
            },
            MapExpr::Specific {
                column: vec!["object".to_string()],
                value: CaluculatedValue::Reference(vec!["object".to_string()]),
            },
        ];

        let grouped = apply_group_by(result, &[ALL_COLUMNS.to_string()], &mapping).unwrap();

        assert_eq!(
            grouped.rows,
            vec![json!({
                "text": ["test", "test2"],
                "object": {
                    "number": [42, 43]
                }
            })]
        );
    }

    #[test]
    fn group_by_aggregates_use_grouped_field_arrays() {
        let result = QueryResult::new(vec![
            json!({
                "id": 1,
                "number": 42,
                "date": "2026-01-01"
            }),
            json!({
                "id": 2,
                "number": 43,
                "date": "2026-01-03"
            }),
        ]);
        let mapping = vec![
            MapExpr::Specific {
                column: vec!["ids".to_string()],
                value: CaluculatedValue::FunctionCall {
                    function: "ARRAY".to_string(),
                    parameters: vec![CaluculatedValue::Reference(vec!["id".to_string()])],
                },
            },
            MapExpr::Specific {
                column: vec!["total".to_string()],
                value: CaluculatedValue::FunctionCall {
                    function: "SUM".to_string(),
                    parameters: vec![CaluculatedValue::Reference(vec!["number".to_string()])],
                },
            },
            MapExpr::Specific {
                column: vec!["count".to_string()],
                value: CaluculatedValue::FunctionCall {
                    function: "COUNT".to_string(),
                    parameters: vec![CaluculatedValue::Reference(vec!["id".to_string()])],
                },
            },
            MapExpr::Specific {
                column: vec!["last_date".to_string()],
                value: CaluculatedValue::FunctionCall {
                    function: "MAXDATE".to_string(),
                    parameters: vec![CaluculatedValue::Reference(vec!["date".to_string()])],
                },
            },
        ];

        let grouped = apply_group_by(result, &[ALL_COLUMNS.to_string()], &mapping).unwrap();

        assert_eq!(
            grouped.rows,
            vec![json!({
                "ids": [1, 2],
                "total": 85,
                "count": 2,
                "last_date": "2026-01-03"
            })]
        );
    }
}
