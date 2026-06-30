use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use chrono::{Duration, Local, NaiveDate};
use quickql_macros::fn_info;
use rand::{distributions::Alphanumeric, Rng};
use serde::Serialize;
use serde_json::Value;

#[path = "functions/color.rs"]
mod color;
#[path = "functions/import/csv.rs"]
mod csv;
mod execution;
#[path = "functions/import/json.rs"]
mod json;
#[path = "functions/ml/optics.rs"]
mod optics;
mod parsing;
#[path = "functions/ml/tsne.rs"]
mod tsne;
#[path = "functions/ml/umap.rs"]
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
                if let Some(function_info) = fn_info_for_call(function, &values) {
                    return (function_info.function)(&values);
                }

                match function.to_ascii_uppercase().as_str() {
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

#[fn_info(name = "SUM")]
fn sum_value(values: &[Value]) -> Value {
    let sum: f64 = values
        .iter()
        .flat_map(flatten_value)
        .map(|value| match value {
            Value::Number(number) => number.as_f64().unwrap_or(0.0),
            Value::String(text) => text.parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        })
        .sum();
    json_number_value(sum)
}

#[fn_info(name = "ARRAY")]
fn array_value(values: &[Value]) -> Vec<Value> {
    values.iter().flat_map(flatten_value).cloned().collect()
}

#[fn_info(name = "COLOR")]
fn color_value(index: usize) -> String {
    color::get_color(index as u64)
}

#[fn_info(name = "LEN")]
fn len_value(value: &str) -> usize {
    value.chars().count()
}

#[fn_info(name = "NUMBER")]
fn number_value(value: &Value) -> Value {
    number_from_value(Some(value))
        .map(|value| serde_json::json!(value))
        .unwrap_or(Value::Null)
}

#[fn_info(name = "TONUMBER")]
fn to_number_value(value: &Value) -> Value {
    number_value(value)
}

#[fn_info(name = "CEIL")]
fn ceil_value(value: &Value) -> Value {
    number_from_value(Some(value))
        .map(|value| serde_json::json!(value.ceil() as i64))
        .unwrap_or(Value::Null)
}

#[fn_info(name = "MIN")]
fn min_value(values: &[Value]) -> Value {
    values
        .iter()
        .flat_map(flatten_value)
        .filter_map(|value| number_from_value(Some(value)))
        .reduce(f64::min)
        .map(json_number_value)
        .unwrap_or(Value::Null)
}

#[fn_info(name = "DISTINCT")]
fn distinct_value(values: &[Value]) -> Vec<Value> {
    let mut output = Vec::new();

    for value in values.iter().flat_map(flatten_value) {
        if !output.contains(value) {
            output.push(value.clone());
        }
    }

    output
}

#[fn_info(name = "SORT")]
fn sort_value(values: &[Value]) -> Vec<Value> {
    let mut output: Vec<_> = values.iter().flat_map(flatten_value).cloned().collect();
    output.sort_by(compare_sort_values);
    output
}

#[fn_info(name = "SOFTMAX")]
fn softmax_value(values: &[Value]) -> Value {
    let Some(values) = numeric_vector_from_values(values) else {
        return Value::Null;
    };
    if values.is_empty() {
        return Value::Array(Vec::new());
    }

    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exp_values: Vec<_> = values.iter().map(|value| (value - max).exp()).collect();
    let sum: f64 = exp_values.iter().sum();
    if !sum.is_finite() || sum == 0.0 {
        return Value::Null;
    }

    Value::Array(
        exp_values
            .into_iter()
            .map(|value| serde_json::json!(value / sum))
            .collect(),
    )
}

#[fn_info(name = "ENTROPY")]
fn shannon_entropy_value(values: &[Value]) -> Value {
    let Some(values) = numeric_vector_from_values(values) else {
        return Value::Null;
    };
    if values.is_empty() || values.iter().any(|value| *value < 0.0) {
        return Value::Null;
    }

    let sum: f64 = values.iter().sum();
    if !sum.is_finite() || sum <= 0.0 {
        return Value::Null;
    }

    let entropy: f64 = values
        .iter()
        .filter(|value| **value > 0.0)
        .map(|value| {
            let probability = value / sum;
            -probability * probability.log2()
        })
        .sum();

    serde_json::json!(entropy)
}

#[fn_info(name = "L2")]
fn l2_value(values: &[Value]) -> Value {
    let Some(values) = numeric_vector_from_values(values) else {
        return Value::Null;
    };

    serde_json::json!(values.iter().map(|value| value * value).sum::<f64>().sqrt())
}

#[fn_info(name = "COUNT")]
fn count_value(values: &[Value]) -> usize {
    values.iter().flat_map(flatten_value).count()
}

fn compare_sort_values(left: &Value, right: &Value) -> std::cmp::Ordering {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => value_to_string(left).cmp(&value_to_string(right)),
    }
}

fn numeric_vector_from_values(values: &[Value]) -> Option<Vec<f64>> {
    values
        .iter()
        .flat_map(flatten_value)
        .map(|value| number_from_value(Some(value)))
        .collect()
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

#[fn_info(name = "SPLIT")]
fn split_value(input: &str, max_part_length: usize) -> Value {
    if max_part_length == 0 {
        return Value::Null;
    }

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

#[fn_info(name = "JOINSTRING")]
fn join_string_value(input: &Value, separator: &str) -> String {
    flatten_value(input)
        .map(value_to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

#[fn_info(name = "ASSIGN")]
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

#[fn_info(name = "PARSE")]
fn parse_json_value(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or(Value::Null)
}

#[fn_info(name = "RAND")]
fn random_string_value(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
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

#[fn_info(name = "RANGE")]
fn range_value(start: i64, end: i64) -> Value {
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

#[fn_info(name = "AT")]
fn at_value(values: &[Value], index: &Value) -> Value {
    if let Some(range) = index.as_array() {
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

    value_to_i64(index)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| values.get(index))
        .cloned()
        .unwrap_or(Value::Null)
}

#[fn_info(name = "EQ")]
fn eq_value(left: &Value, right: &Value) -> bool {
    left == right
}

#[fn_info(name = "OR")]
fn or_value(values: &[Value]) -> bool {
    values.iter().any(value_truthy)
}

#[fn_info(name = "AND")]
fn and_value(values: &[Value]) -> bool {
    values.iter().all(value_truthy)
}

#[fn_info(name = "CONCAT")]
fn concat_value(values: &[Value]) -> String {
    values
        .iter()
        .flat_map(flatten_value)
        .map(value_to_string)
        .collect()
}

#[fn_info(name = "BASE64")]
fn base64_value(value: &Value) -> String {
    BASE64_STANDARD.encode(value_to_string(value))
}

#[fn_info]
fn index_of(values: &[Value], needle: &Value) -> Option<usize> {
    values.iter().position(|value| value == needle)
}

#[fn_info]
fn today() -> String {
    Local::now().date_naive().to_string()
}

#[fn_info]
fn new_line() -> String {
    "\n".into()
}

#[allow(dead_code)]
struct FnInfo {
    name: &'static str,
    params: Vec<ParamInfo>,
    min_params: usize,
    variadic: bool,
    return_type: JsonTypeInfo,
    function: Box<dyn Fn(&[Value]) -> Value + Send + Sync>,
}

#[allow(dead_code)]
struct ParamInfo {
    name: &'static str,
    r#type: JsonTypeInfo,
}

#[allow(dead_code)]
enum JsonTypeInfo {
    Any,
    Null,
    Bool,
    Number,
    String,
    Array(Arc<JsonTypeInfo>),
    Object(HashMap<String, JsonTypeInfo>),
    OneOf(Vec<JsonTypeInfo>),
}

static FN_INFO_BY_NAME: LazyLock<HashMap<String, FnInfo>> = LazyLock::new(|| {
    vec![
        sum_value_info(),
        array_value_info(),
        assign_value_info(),
        parse_json_value_info(),
        number_value_info(),
        to_number_value_info(),
        ceil_value_info(),
        min_value_info(),
        distinct_value_info(),
        sort_value_info(),
        softmax_value_info(),
        shannon_entropy_value_info(),
        l2_value_info(),
        count_value_info(),
        len_value_info(),
        split_value_info(),
        join_string_value_info(),
        random_string_value_info(),
        range_value_info(),
        at_value_info(),
        cross_join_value_info(),
        zip_rows_value_info(),
        eq_value_info(),
        or_value_info(),
        and_value_info(),
        add_date_value_info(),
        iso_date_value_info(),
        get_date_value_info(),
        min_date_value_info(),
        max_date_value_info(),
        concat_value_info(),
        base64_value_info(),
        color_value_info(),
        optics_value_info(),
        tsne_value_info(),
        umap_value_info(),
        index_of_info(),
        today_info(),
        new_line_info(),
    ]
    .into_iter()
    .map(|info| (normalized_function_name(info.name), info))
    .collect()
});

fn fn_info_for_call(function: &str, values: &[Value]) -> Option<&'static FnInfo> {
    FN_INFO_BY_NAME
        .get(&normalized_function_name(function))
        .filter(|info| {
            values.len() >= info.min_params && (info.variadic || values.len() <= info.params.len())
        })
}

fn normalized_function_name(function: &str) -> String {
    function
        .chars()
        .filter(|character| *character != '_')
        .flat_map(char::to_uppercase)
        .collect()
}

#[fn_info(name = "ADDDATE")]
fn add_date_value(date: &str, days: i64) -> Value {
    let Some(date) = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok() else {
        return Value::Null;
    };

    date.checked_add_signed(Duration::days(days))
        .map(|date| Value::String(date.to_string()))
        .unwrap_or(Value::Null)
}

#[fn_info(name = "ISODATE")]
fn iso_date_value(date: &str) -> Value {
    let Some(date_key) = parse_date_key(date).ok().flatten() else {
        return Value::Null;
    };

    let year = date_key / 10_000;
    let month = (date_key / 100) % 100;
    let day = date_key % 100;
    Value::String(format!("{year:04}-{month:02}-{day:02}"))
}

#[fn_info(name = "GETDATE")]
fn get_date_value(text: &str) -> Value {
    text.split('T')
        .next()
        .map(str::trim)
        .filter(|date| !date.is_empty())
        .map(|date| Value::String(date.to_string()))
        .unwrap_or(Value::Null)
}

#[fn_info(name = "MINDATE")]
fn min_date_value(values: &[Value]) -> Value {
    aggregate_date(
        &values
            .iter()
            .flat_map(flatten_value)
            .cloned()
            .collect::<Vec<_>>(),
        DateAggregate::Min,
    )
}

#[fn_info(name = "MAXDATE")]
fn max_date_value(values: &[Value]) -> Value {
    aggregate_date(
        &values
            .iter()
            .flat_map(flatten_value)
            .cloned()
            .collect::<Vec<_>>(),
        DateAggregate::Max,
    )
}

#[fn_info(name = "OPTICS")]
fn optics_value(input: Option<&Value>, options: Option<&Value>) -> Value {
    optics::optics_value(input, options)
}

#[fn_info(name = "TSNE")]
fn tsne_value(input: Option<&Value>, options: Option<&Value>) -> Value {
    tsne::tsne_value(input, options)
}

#[fn_info(name = "UMAP")]
fn umap_value(input: Option<&Value>, options: Option<&Value>) -> Value {
    umap::umap_value(input, options)
}

#[fn_info(name = "CROSSJOIN")]
fn cross_join_value(input: &Value) -> Value {
    let Some(input) = input.as_object() else {
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

#[fn_info(name = "ZIPROWS")]
fn zip_rows_value(input: &Value) -> Value {
    let Some(input) = input.as_object() else {
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
