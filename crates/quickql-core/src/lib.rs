use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use chrono::{Duration, Local, NaiveDate};
use rand::{distributions::Alphanumeric, Rng};
use serde::Serialize;
use serde_json::Value;

mod csv;
mod execution;
mod json;
mod parsing;
mod umap;

pub use execution::{
    fields_from_source_sample, json_fields_for_query, json_fields_from_source_sample,
    source_path_for_query, stream_query_jsonl,
};
pub use parsing::parse_query;

use crate::execution::parse_date_key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubQuery {
    Source(Vec<CaluculatedValue>),
    Map(MapStep),
    Filter(CaluculatedValue),
    MapMany(MapMany),
    GroupBy {
        keys: Vec<String>,
        mapping: Vec<MapExpr>,
    },
    SortBy(Vec<SortKey>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub steps: Vec<SubQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapStep {
    pub config: Value,
    pub mapping: Vec<MapExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapMany {
    pub field: String,
    pub include: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapExpr {
    All,
    Specific {
        column: Vec<String>,
        value: CaluculatedValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaluculatedValue {
    Reference(Vec<String>),
    Static(Value),
    Object(Vec<(String, CaluculatedValue)>),
    Array(Vec<CaluculatedValue>),
    FunctionCall {
        function: String,
        parameters: Vec<CaluculatedValue>,
    },
}

impl CaluculatedValue {
    pub(crate) fn caluculate(
        &self,
        value: &Value,
        query_path: &Path,
        ql_stack: &mut Vec<PathBuf>,
    ) -> Value {
        match self {
            CaluculatedValue::Reference(path) => path
                .iter()
                .try_fold(value, |current, part| {
                    if part == "$" {
                        return Some(current);
                    }

                    match current {
                        Value::Object(map) => map.get(part),
                        _ => None,
                    }
                })
                .cloned()
                .unwrap_or(Value::Null),
            CaluculatedValue::Static(value) => value.clone(),
            CaluculatedValue::Object(entries) => {
                let mut output = serde_json::Map::new();
                for (key, entry) in entries {
                    output.insert(key.clone(), entry.caluculate(value, query_path, ql_stack));
                }
                Value::Object(output)
            }
            CaluculatedValue::Array(entries) => Value::Array(
                entries
                    .iter()
                    .map(|entry| entry.caluculate(value, query_path, ql_stack))
                    .collect(),
            ),
            CaluculatedValue::FunctionCall {
                function,
                parameters,
            } => {
                let values: Vec<_> = parameters
                    .iter()
                    .map(|x| x.caluculate(value, query_path, ql_stack))
                    .collect();
                match function.to_ascii_uppercase().as_str() {
                    "SUM" => {
                        let sum: f64 = values
                            .iter()
                            .flat_map(flatten_value)
                            .map(|value| match value {
                                Value::Number(number) => number.as_f64().unwrap_or(0.0),
                                Value::String(text) => text.parse::<f64>().unwrap_or(0.0),
                                _ => 0.0,
                            })
                            .sum();
                        if sum.fract() == 0.0 && sum >= i64::MIN as f64 && sum <= i64::MAX as f64 {
                            serde_json::json!(sum as i64)
                        } else {
                            serde_json::json!(sum)
                        }
                    }
                    "ARRAY" => {
                        Value::Array(values.iter().flat_map(flatten_value).cloned().collect())
                    }
                    "COUNT" => serde_json::json!(values.iter().flat_map(flatten_value).count()),
                    "LEN" => len_value(values.first()),
                    "RAND" => random_string_value(values.first()),
                    "RANGE" => range_value(values.first(), values.get(1)),
                    "AT" => at_value(values.first(), values.get(1)),
                    "CROSSJOIN" => cross_join_value(values.first()),
                    "ZIPROWS" => zip_rows_value(values.first()),
                    "GET" => CaluculatedValue::Reference(values.iter().skip(1).filter_map(|x|x.as_str()).map(|x|x.to_string()).collect()).caluculate(values.first().unwrap_or_default(), query_path, ql_stack),
                    "EQ" => Value::Bool(values.first() == values.get(1)),
                    "OR" => Value::Bool(values.iter().any(value_truthy)),
                    "AND" => Value::Bool(values.iter().all(value_truthy)),
                    "TODAY" => Value::String(Local::now().date_naive().to_string()),
                    "ADDDATE" => add_date_value(values.first(), values.get(1)),
                    "GETDATE" => values
                        .first()
                        .and_then(|value| value.as_str())
                        .and_then(|text| text.split('T').next())
                        .filter(|date| !date.trim().is_empty())
                        .map(|date| Value::String(date.trim().to_string()))
                        .unwrap_or(Value::Null),
                    "MINDATE" => aggregate_date(
                        &values
                            .iter()
                            .flat_map(flatten_value)
                            .cloned()
                            .collect::<Vec<_>>(),
                        DateAggregate::Min,
                    ),
                    "MAXDATE" => aggregate_date(
                        &values
                            .iter()
                            .flat_map(flatten_value)
                            .cloned()
                            .collect::<Vec<_>>(),
                        DateAggregate::Max,
                    ),
                    "OPEN" => Self::open_source(
                        values.first(),
                        query_path,
                        reqwest::Method::GET,
                        ql_stack,
                    ),
                    "POST" => Self::open_source(
                        values.first(),
                        query_path,
                        reqwest::Method::POST,
                        ql_stack,
                    ),
                    "PUT" => Self::open_source(
                        values.first(),
                        query_path,
                        reqwest::Method::PUT,
                        ql_stack,
                    ),
                    "CONCAT" => Value::String(
                        values
                            .iter()
                            .flat_map(flatten_value)
                            .map(value_to_string)
                            .collect(),
                    ),
                    "BASE64" => values
                        .first()
                        .map(value_to_string)
                        .map(|value| Value::String(BASE64_STANDARD.encode(value)))
                        .unwrap_or(Value::Null),
                    "UMAP" => umap::umap_value(values.first(), values.get(1)),
                    _ => Value::Null,
                }
            }
        }
    }

    fn open_source(
        value: Option<&Value>,
        query_path: &Path,
        method: reqwest::Method,
        ql_stack: &mut Vec<PathBuf>,
    ) -> Value {
        if let Some(Value::String(source)) = value {
            return execution::load_query_source(
                query_path,
                ql_stack,
                source,
                method,
                Default::default(),
                None,
                None,
            )
            .unwrap_or_default();
        }

        if let Some(Value::Object(obj)) = value {
            let src = obj.get("src").and_then(|x| x.as_str()).unwrap_or_default();
            let mut headers: HashMap<&str, &str> = Default::default();
            if let Some(source_headers) = obj.get("headers").and_then(|x| x.as_object()) {
                for (key, value) in source_headers.iter() {
                    if let Some(value) = value.as_str() {
                        headers.insert(key.as_str(), value);
                    }
                }
            }
            return execution::load_query_source(
                query_path,
                ql_stack,
                src,
                method,
                headers,
                obj.get("body"),
                obj.get("paging"),
            )
            .unwrap_or_default();
        }

        Value::Null
    }
}
enum DateAggregate {
    Min,
    Max,
}

fn aggregate_date(rows: &[Value], aggregate: DateAggregate) -> Value {
    let mut selected: Option<(i32, String)> = None;

    for value in rows {
        let Value::String(date_text) = value else {
            continue;
        };
        let Ok(Some(date_key)) = parse_date_key(&date_text) else {
            continue;
        };

        let should_replace = match (&selected, &aggregate) {
            (None, _) => true,
            (Some((current, _)), DateAggregate::Min) => date_key < *current,
            (Some((current, _)), DateAggregate::Max) => date_key > *current,
        };

        if should_replace {
            selected = Some((date_key, date_text.to_string()));
        }
    }

    selected
        .map(|(_, date_text)| Value::String(date_text))
        .unwrap_or(Value::Null)
}

fn flatten_value(value: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match value {
        Value::Array(values) => Box::new(values.iter()),
        value => Box::new(std::iter::once(value)),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        value => value.to_string(),
    }
}

fn len_value(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .map(|value| serde_json::json!(value.chars().count()))
        .unwrap_or(Value::Null)
}

fn random_string_value(length: Option<&Value>) -> Value {
    let Some(length) = length
        .and_then(value_to_i64)
        .and_then(|length| usize::try_from(length).ok())
    else {
        return Value::Null;
    };

    let value: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect();
    Value::String(value)
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

fn range_value(start: Option<&Value>, end: Option<&Value>) -> Value {
    let (Some(start), Some(end)) = (start.and_then(value_to_i64), end.and_then(value_to_i64))
    else {
        return Value::Null;
    };

    let step = if start <= end { 1 } else { -1 };
    let mut current = start;
    let mut values = Vec::new();

    loop {
        values.push(serde_json::json!(current));
        if current == end {
            break;
        }
        current += step;
    }

    Value::Array(values)
}

fn at_value(input: Option<&Value>, index: Option<&Value>) -> Value {
    let (Some(values), Some(index)) = (
        input.and_then(Value::as_array),
        index
            .and_then(value_to_i64)
            .and_then(|index| usize::try_from(index).ok()),
    ) else {
        return Value::Null;
    };

    values.get(index).cloned().unwrap_or(Value::Null)
}

fn add_date_value(date: Option<&Value>, days: Option<&Value>) -> Value {
    let (Some(date), Some(days)) = (
        date.and_then(Value::as_str)
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()),
        days.and_then(value_to_i64),
    ) else {
        return Value::Null;
    };

    date.checked_add_signed(Duration::days(days))
        .map(|date| Value::String(date.to_string()))
        .unwrap_or(Value::Null)
}

fn cross_join_value(input: Option<&Value>) -> Value {
    let Some(input) = input.and_then(Value::as_object) else {
        return Value::Null;
    };

    let arrays: Option<Vec<_>> = input
        .iter()
        .map(|(key, value)| value.as_array().map(|values| (key.as_str(), values)))
        .collect();
    let Some(arrays) = arrays else {
        return Value::Null;
    };

    if arrays.is_empty() {
        return Value::Array(Vec::new());
    }

    let mut rows = Vec::new();
    cross_join_rows(
        &arrays,
        arrays.len() - 1,
        &mut serde_json::Map::new(),
        &mut rows,
    );
    Value::Array(rows)
}

fn cross_join_rows(
    arrays: &[(&str, &Vec<Value>)],
    index: usize,
    current: &mut serde_json::Map<String, Value>,
    rows: &mut Vec<Value>,
) {
    let (key, values) = arrays[index];
    for value in values {
        current.insert(key.to_string(), value.clone());
        if index == 0 {
            rows.push(Value::Object(current.clone()));
        } else {
            cross_join_rows(arrays, index - 1, current, rows);
        }
    }
    current.remove(key);
}

fn zip_rows_value(input: Option<&Value>) -> Value {
    let Some(input) = input.and_then(Value::as_object) else {
        return Value::Null;
    };

    let arrays: Option<Vec<_>> = input
        .iter()
        .map(|(key, value)| value.as_array().map(|values| (key.as_str(), values)))
        .collect();
    let Some(arrays) = arrays else {
        return Value::Null;
    };

    let Some((_, first_array)) = arrays.first() else {
        return Value::Array(Vec::new());
    };
    let row_count = first_array.len();
    if arrays.iter().any(|(_, values)| values.len() != row_count) {
        return Value::Null;
    }

    let rows = (0..row_count)
        .map(|index| {
            let row = arrays
                .iter()
                .map(|(key, values)| (key.to_string(), values[index].clone()))
                .collect();
            Value::Object(row)
        })
        .collect();

    Value::Array(rows)
}

fn value_to_i64(value: &Value) -> Option<i64> {
    value.as_i64()
}

pub struct QueryResult {
    pub columns: KeyDescriptor,
    pub rows: Vec<Value>,
}

impl QueryResult {
    pub fn new(rows: Vec<Value>) -> Self {
        Self {
            columns: KeyDescriptor::from_values(&rows),
            rows,
        }
    }
}

impl Default for QueryResult {
    fn default() -> Self {
        Self {
            columns: KeyDescriptor::Value,
            rows: vec![],
        }
    }
}

pub enum KeyDescriptor {
    Value,
    Object(HashMap<String, KeyDescriptor>),
}

impl KeyDescriptor {
    fn from_values(values: &[Value]) -> Self {
        let mut fields = HashMap::new();

        for value in values {
            Self::merge_value(&mut fields, value);
        }

        if fields.is_empty() {
            Self::Value
        } else {
            Self::Object(fields)
        }
    }

    fn from_value(value: &Value) -> Self {
        match value {
            Value::Object(map) => {
                let mut fields = HashMap::new();
                for (key, value) in map {
                    fields.insert(key.clone(), Self::from_value(value));
                }
                Self::Object(fields)
            }
            _ => Self::Value,
        }
    }

    fn merge_value(fields: &mut HashMap<String, KeyDescriptor>, value: &Value) {
        let Value::Object(map) = value else {
            return;
        };

        for (key, value) in map {
            match fields.get_mut(key) {
                Some(existing) => existing.merge(value),
                None => {
                    fields.insert(key.clone(), Self::from_value(value));
                }
            }
        }
    }

    fn merge(&mut self, value: &Value) {
        match (self, value) {
            (Self::Object(fields), Value::Object(_)) => Self::merge_value(fields, value),
            (descriptor, _) => *descriptor = Self::Value,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum StreamMessage<'a> {
    Meta {
        columns: &'a [String],
        source: String,
    },
    Batch {
        start: usize,
        rows: &'a [Value],
    },
    Done {
        #[serde(rename = "rowCount")]
        row_count: usize,
        #[serde(rename = "elapsedMs")]
        elapsed_ms: f64,
    },
}

pub const DEFAULT_BLOCK_SIZE: usize = 1000;
pub const DEFAULT_STREAM_BATCH_SIZE: usize = 1000;

pub(crate) const ALL_COLUMNS: &str = "*";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_rows_transposes_object_arrays_to_rows() {
        let input = serde_json::json!({
            "name": ["a", "b", "c"],
            "value": [1, 2, 3],
        });

        assert_eq!(
            zip_rows_value(Some(&input)),
            serde_json::json!([
                {"name": "a", "value": 1},
                {"name": "b", "value": 2},
                {"name": "c", "value": 3},
            ])
        );
    }

    #[test]
    fn zip_rows_returns_null_for_non_array_fields() {
        let input = serde_json::json!({
            "name": ["a"],
            "value": 1,
        });

        assert_eq!(zip_rows_value(Some(&input)), Value::Null);
    }

    #[test]
    fn zip_rows_returns_null_for_mismatched_array_lengths() {
        let input = serde_json::json!({
            "name": ["a", "b"],
            "value": [1],
        });

        assert_eq!(zip_rows_value(Some(&input)), Value::Null);
    }
}
