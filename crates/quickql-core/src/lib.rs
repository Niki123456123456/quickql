use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

mod csv;
mod execution;
mod json;
mod parsing;

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
    Map(Vec<MapExpr>),
    Filter(CaluculatedValue),
    MapMany(String),
    GroupBy {
        keys: Vec<String>,
        mapping: Vec<MapExpr>,
    },
    OrderBy(Vec<SortKey>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub steps: Vec<SubQuery>,
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
                .try_fold(value, |current, part| match current {
                    Value::Object(map) => map.get(part),
                    _ => None,
                })
                .cloned()
                .unwrap_or(Value::Null),
            CaluculatedValue::Static(value) => value.clone(),
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
                    "LEN" => serde_json::json!(values.len()),
                    "EQ" => Value::Bool(values.first() == values.get(1)),
                    "OR" => Value::Bool(values.iter().any(value_truthy)),
                    "AND" => Value::Bool(values.iter().all(value_truthy)),
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
                    "OPEN" => {
                        if let Some(Value::String(source)) = values.first() {
                            if let Ok(result) =
                                execution::load_query_source(query_path, source, ql_stack)
                            {
                                return Value::Array(result.rows);
                            }
                        }
                        return Value::Null;
                    }
                    _ => Value::Null,
                }
            }
        }
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
