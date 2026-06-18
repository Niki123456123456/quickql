use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use chrono::{Duration, Local, NaiveDate};
use rand::{distributions::Alphanumeric, Rng};
use serde::Serialize;
use serde_json::Value;

mod color;
mod csv;
mod execution;
mod json;
mod optics;
mod parsing;
mod tsne;
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
                    "ASSIGN" => assign_value(&values),
                    "PARSE" => parse_json_value(values.first()),
                    "NUMBER" | "TONUMBER" => number_value(values.first()),
                    "CEIL" => ceil_value(values.first()),
                    "MIN" => min_value(&values),
                    "DISTINCT" => distinct_value(&values),
                    "SORT" => sort_value(&values),
                    "COUNT" => serde_json::json!(values.iter().flat_map(flatten_value).count()),
                    "LEN" => len_value(values.first()),
                    "SPLIT" => split_value(values.first(), values.get(1)),
                    "JOINSTRING" => join_string_value(values.first(), values.get(1)),
                    "NEWLINE" => Value::String("\n".into()),
                    "RAND" => random_string_value(values.first()),
                    "RANGE" => range_value(values.first(), values.get(1)),
                    "AT" => at_value(values.first(), values.get(1)),
                    "INDEXOF" => index_of_value(values.first(), values.get(1)),
                    "CROSSJOIN" => cross_join_value(values.first()),
                    "ZIPROWS" => zip_rows_value(values.first()),
                    "GET" => CaluculatedValue::Reference(
                        values
                            .iter()
                            .skip(1)
                            .filter_map(|x| x.as_str())
                            .map(|x| x.to_string())
                            .collect(),
                    )
                    .caluculate(
                        values.first().unwrap_or_default(),
                        query_path,
                        ql_stack,
                    ),
                    "EQ" => Value::Bool(values.first() == values.get(1)),
                    "OR" => Value::Bool(values.iter().any(value_truthy)),
                    "AND" => Value::Bool(values.iter().all(value_truthy)),
                    "TODAY" => Value::String(Local::now().date_naive().to_string()),
                    "ADDDATE" => add_date_value(values.first(), values.get(1)),
                    "ISODATE" => iso_date_value(values.first()),
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
                    "COLOR" => color_value(values.first()),
                    "OPTICS" => optics::optics_value(values.first(), values.get(1)),
                    "TSNE" => tsne::tsne_value(values.first(), values.get(1)),
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

fn color_value(index: Option<&Value>) -> Value {
    index
        .and_then(Value::as_u64)
        .map(color::get_color)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn len_value(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .map(|value| serde_json::json!(value.chars().count()))
        .unwrap_or(Value::Null)
}

fn number_value(value: Option<&Value>) -> Value {
    number_from_value(value)
        .map(|value| serde_json::json!(value))
        .unwrap_or(Value::Null)
}

fn ceil_value(value: Option<&Value>) -> Value {
    number_from_value(value)
        .map(|value| serde_json::json!(value.ceil() as i64))
        .unwrap_or(Value::Null)
}

fn min_value(values: &[Value]) -> Value {
    values
        .iter()
        .flat_map(flatten_value)
        .filter_map(|value| number_from_value(Some(value)))
        .reduce(f64::min)
        .map(json_number_value)
        .unwrap_or(Value::Null)
}

fn distinct_value(values: &[Value]) -> Value {
    let mut output = Vec::new();

    for value in values.iter().flat_map(flatten_value) {
        if !output.contains(value) {
            output.push(value.clone());
        }
    }

    Value::Array(output)
}

fn sort_value(values: &[Value]) -> Value {
    let mut output: Vec<_> = values.iter().flat_map(flatten_value).cloned().collect();
    output.sort_by(compare_sort_values);
    Value::Array(output)
}

fn compare_sort_values(left: &Value, right: &Value) -> std::cmp::Ordering {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => value_to_string(left).cmp(&value_to_string(right)),
    }
}

fn number_from_value(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(value)) => value.as_f64().filter(|value| value.is_finite()),
        Some(Value::String(value)) => {
            parse_localised_number(value).filter(|value| value.is_finite())
        }
        _ => None,
    }
}

fn json_number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        serde_json::json!(value as i64)
    } else {
        serde_json::json!(value)
    }
}

fn parse_localised_number(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if value.contains(',') && !value.contains('.') {
        value.replace(',', ".").parse().ok()
    } else {
        value.parse().ok()
    }
}

fn split_value(input: Option<&Value>, max_part_length: Option<&Value>) -> Value {
    let (Some(input), Some(max_part_length)) = (
        input.and_then(Value::as_str),
        max_part_length
            .and_then(value_to_i64)
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length > 0),
    ) else {
        return Value::Null;
    };

    let char_count = input.chars().count();
    if char_count == 0 {
        return Value::Array(Vec::new());
    }

    let part_count = char_count.div_ceil(max_part_length);
    let base_part_length = char_count / part_count;
    let longer_part_count = char_count % part_count;
    let byte_indices: Vec<_> = input
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(input.len()))
        .collect();

    let mut start = 0;
    let parts = (0..part_count)
        .map(|index| {
            let part_length = base_part_length + usize::from(index < longer_part_count);
            let end = start + part_length;
            let part = Value::String(input[byte_indices[start]..byte_indices[end]].to_string());
            start = end;
            part
        })
        .collect();

    Value::Array(parts)
}

fn join_string_value(input: Option<&Value>, separator: Option<&Value>) -> Value {
    let (Some(input), Some(separator)) = (input, separator.and_then(Value::as_str)) else {
        return Value::Null;
    };

    Value::String(
        flatten_value(input)
            .map(value_to_string)
            .collect::<Vec<_>>()
            .join(separator),
    )
}

fn assign_value(values: &[Value]) -> Value {
    let Some(first) = values.first() else {
        return Value::Null;
    };
    if !first.is_object() {
        return Value::Null;
    }

    let mut output = first.clone();
    for value in values.iter().skip(1) {
        let Some(source) = value.as_object() else {
            return Value::Null;
        };

        for (key, value) in source {
            execution::assign_output(&mut output, &[key.clone()], value.clone());
        }
    }

    output
}

fn parse_json_value(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .and_then(|value| serde_json::from_str(value).ok())
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
    let Some(values) = input.and_then(Value::as_array) else {
        return Value::Null;
    };

    if let Some(range) = index.and_then(Value::as_array) {
        let Some(start) = range
            .first()
            .and_then(value_to_i64)
            .and_then(|index| usize::try_from(index).ok())
        else {
            return Value::Null;
        };
        let end = match range.as_slice() {
            [_] => values.len(),
            [_, end] => {
                let Some(end) = value_to_i64(end).and_then(|index| usize::try_from(index).ok())
                else {
                    return Value::Null;
                };
                end
            }
            _ => return Value::Null,
        };

        return values
            .get(start..end)
            .map(|values| Value::Array(values.to_vec()))
            .unwrap_or(Value::Null);
    }

    index
        .and_then(value_to_i64)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| values.get(index))
        .cloned()
        .unwrap_or(Value::Null)
}

fn index_of_value(input: Option<&Value>, needle: Option<&Value>) -> Value {
    let (Some(values), Some(needle)) = (input.and_then(Value::as_array), needle) else {
        return Value::Null;
    };

    values
        .iter()
        .position(|value| value == needle)
        .map(|index| serde_json::json!(index))
        .unwrap_or_else(|| serde_json::json!(-1))
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

fn iso_date_value(date: Option<&Value>) -> Value {
    let Some(date_key) = date
        .and_then(Value::as_str)
        .and_then(|date| parse_date_key(date).ok().flatten())
    else {
        return Value::Null;
    };

    let year = date_key / 10_000;
    let month = (date_key / 100) % 100;
    let day = date_key % 100;
    Value::String(format!("{year:04}-{month:02}-{day:02}"))
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
    Progress {
        substep: usize,
        #[serde(rename = "totalSubsteps")]
        total_substeps: usize,
        #[serde(rename = "substepName")]
        substep_name: &'a str,
        percent: f64,
        #[serde(rename = "elapsedMs")]
        elapsed_ms: f64,
        #[serde(rename = "remainingMs")]
        remaining_ms: Option<f64>,
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
    fn split_value_splits_string_into_equal_parts_under_max_length() {
        assert_eq!(
            split_value(
                Some(&serde_json::json!("abcdefghij")),
                Some(&serde_json::json!(3))
            ),
            serde_json::json!(["abc", "def", "gh", "ij"])
        );
    }

    #[test]
    fn split_value_distributes_remainder_across_first_parts() {
        assert_eq!(
            split_value(
                Some(&serde_json::json!("abcdefghij")),
                Some(&serde_json::json!(4))
            ),
            serde_json::json!(["abcd", "efg", "hij"])
        );
    }

    #[test]
    fn split_value_counts_characters_not_bytes() {
        assert_eq!(
            split_value(
                Some(&serde_json::json!("åßcd")),
                Some(&serde_json::json!(2))
            ),
            serde_json::json!(["åß", "cd"])
        );
    }

    #[test]
    fn split_value_rejects_invalid_input() {
        assert_eq!(
            split_value(Some(&serde_json::json!("abc")), Some(&serde_json::json!(0))),
            Value::Null
        );
        assert_eq!(
            split_value(
                Some(&serde_json::json!(["abc"])),
                Some(&serde_json::json!(2))
            ),
            Value::Null
        );
    }

    #[test]
    fn join_string_value_joins_array_with_separator() {
        assert_eq!(
            join_string_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!("t"))
            ),
            serde_json::json!("atbtc")
        );
    }

    #[test]
    fn join_string_value_rejects_invalid_separator() {
        assert_eq!(
            join_string_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!(1))
            ),
            Value::Null
        );
    }

    #[test]
    fn number_value_parses_decimal_comma_string() {
        assert_eq!(
            number_value(Some(&serde_json::json!("0,3333"))),
            serde_json::json!(0.3333)
        );
    }

    #[test]
    fn number_value_keeps_number_values() {
        assert_eq!(
            number_value(Some(&serde_json::json!(12.5))),
            serde_json::json!(12.5)
        );
    }

    #[test]
    fn number_value_rejects_invalid_input() {
        assert_eq!(number_value(Some(&serde_json::json!("abc"))), Value::Null);
        assert_eq!(number_value(Some(&serde_json::json!(["0,1"]))), Value::Null);
    }

    #[test]
    fn ceil_value_rounds_number_up() {
        assert_eq!(
            ceil_value(Some(&serde_json::json!(0.3333))),
            serde_json::json!(1)
        );
    }

    #[test]
    fn ceil_value_rounds_decimal_comma_string_up() {
        assert_eq!(
            ceil_value(Some(&serde_json::json!("0,3333"))),
            serde_json::json!(1)
        );
    }

    #[test]
    fn ceil_value_rejects_invalid_input() {
        assert_eq!(ceil_value(Some(&serde_json::json!("abc"))), Value::Null);
        assert_eq!(ceil_value(Some(&serde_json::json!(["0,1"]))), Value::Null);
    }

    #[test]
    fn min_value_returns_minimum_from_array() {
        assert_eq!(
            min_value(&[serde_json::json!([
                6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7
            ])]),
            serde_json::json!(6)
        );
    }

    #[test]
    fn min_value_returns_minimum_from_multiple_values() {
        assert_eq!(
            min_value(&[
                serde_json::json!(8),
                serde_json::json!("0,3333"),
                serde_json::json!(1)
            ]),
            serde_json::json!(0.3333)
        );
    }

    #[test]
    fn min_value_rejects_input_without_numbers() {
        assert_eq!(min_value(&[serde_json::json!(["abc"])]), Value::Null);
    }

    #[test]
    fn distinct_value_removes_duplicates_from_array() {
        assert_eq!(
            distinct_value(&[serde_json::json!(["a", "c", "b", "c"])]),
            serde_json::json!(["a", "c", "b"])
        );
    }

    #[test]
    fn distinct_value_removes_duplicates_from_multiple_values() {
        assert_eq!(
            distinct_value(&[
                serde_json::json!(["a", "c"]),
                serde_json::json!("b"),
                serde_json::json!("c")
            ]),
            serde_json::json!(["a", "c", "b"])
        );
    }

    #[test]
    fn sort_value_sorts_string_array() {
        assert_eq!(
            sort_value(&[serde_json::json!(["a", "c", "b"])]),
            serde_json::json!(["a", "b", "c"])
        );
    }

    #[test]
    fn sort_value_sorts_numbers_numerically() {
        assert_eq!(
            sort_value(&[serde_json::json!([10, 2, 1])]),
            serde_json::json!([1, 2, 10])
        );
    }

    #[test]
    fn iso_date_value_maps_dot_separated_date_to_iso_date() {
        assert_eq!(
            iso_date_value(Some(&serde_json::json!("24.03.2026"))),
            serde_json::json!("2026-03-24")
        );
    }

    #[test]
    fn iso_date_value_rejects_invalid_input() {
        assert_eq!(
            iso_date_value(Some(&serde_json::json!("31.02.2026"))),
            Value::Null
        );
        assert_eq!(iso_date_value(Some(&serde_json::json!(42))), Value::Null);
        assert_eq!(iso_date_value(None), Value::Null);
    }

    #[test]
    fn at_value_returns_array_item() {
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!(1))
            ),
            serde_json::json!("b")
        );
    }

    #[test]
    fn at_value_returns_array_slice_for_range_index() {
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c", "d", "e"])),
                Some(&serde_json::json!([2, 4]))
            ),
            serde_json::json!(["c", "d"])
        );
    }

    #[test]
    fn at_value_returns_array_slice_to_end_for_single_item_range() {
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c", "d", "e"])),
                Some(&serde_json::json!([2]))
            ),
            serde_json::json!(["c", "d", "e"])
        );
    }

    #[test]
    fn at_value_rejects_invalid_slice_ranges() {
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!([2, 4]))
            ),
            Value::Null
        );
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!([2, 1]))
            ),
            Value::Null
        );
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!([0, 1, 2]))
            ),
            Value::Null
        );
        assert_eq!(
            at_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!([]))
            ),
            Value::Null
        );
    }

    #[test]
    fn index_of_value_returns_zero_based_index() {
        assert_eq!(
            index_of_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!("c"))
            ),
            serde_json::json!(2)
        );
    }

    #[test]
    fn index_of_value_returns_minus_one_when_not_found() {
        assert_eq!(
            index_of_value(
                Some(&serde_json::json!(["a", "b", "c"])),
                Some(&serde_json::json!("d"))
            ),
            serde_json::json!(-1)
        );
    }

    #[test]
    fn index_of_value_rejects_invalid_input() {
        assert_eq!(
            index_of_value(
                Some(&serde_json::json!("abc")),
                Some(&serde_json::json!("b"))
            ),
            Value::Null
        );
        assert_eq!(
            index_of_value(Some(&serde_json::json!(["a", "b", "c"])), None),
            Value::Null
        );
    }

    #[test]
    fn color_value_returns_deterministic_rgb_color() {
        assert_eq!(
            color_value(Some(&serde_json::json!(0))),
            serde_json::json!("rgb(51, 255, 255)")
        );
        assert_eq!(
            color_value(Some(&serde_json::json!(1))),
            serde_json::json!("rgb(153, 255, 51)")
        );
    }

    #[test]
    fn color_value_rejects_invalid_input() {
        assert_eq!(color_value(Some(&serde_json::json!(-1))), Value::Null);
        assert_eq!(color_value(Some(&serde_json::json!("1"))), Value::Null);
        assert_eq!(color_value(None), Value::Null);
    }
}
