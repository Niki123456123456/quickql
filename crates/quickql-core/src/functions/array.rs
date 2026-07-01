use quickql_macros::fn_info;
use serde_json::Value;

use crate::{flatten_value, value_to_string, FnInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![
        index_of_info(),
        range_info(),
        at_info(),
        distinct_info(),
        sort_info(),
        count_info(),
        cross_join_info(),
        zip_rows_info(),
    ]
}

#[fn_info]
fn index_of(values: &[Value], needle: &Value) -> Option<usize> {
    values.iter().position(|value| value == needle)
}

#[fn_info()]
fn range(start: i64, end: i64) -> Value {
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

#[fn_info()]
fn at(values: &[Value], index: &Value) -> Value {
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

#[fn_info()]
fn distinct(values: &[Value]) -> Vec<Value> {
    let mut output = Vec::new();

    for value in values.iter().flat_map(flatten_value) {
        if !output.contains(value) {
            output.push(value.clone());
        }
    }

    output
}

#[fn_info()]
fn sort(values: &[Value]) -> Vec<Value> {
    let mut output: Vec<_> = values.iter().flat_map(flatten_value).cloned().collect();
    output.sort_by(compare_sort_values);
    output
}

#[fn_info()]
fn count(values: &[Value]) -> usize {
    values.iter().flat_map(flatten_value).count()
}

#[fn_info()]
fn cross_join(input: &Value) -> Value {
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

#[fn_info()]
fn zip_rows(input: &Value) -> Value {
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

fn compare_sort_values(left: &Value, right: &Value) -> std::cmp::Ordering {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => value_to_string(left).cmp(&value_to_string(right)),
    }
}

fn value_to_i64(value: &Value) -> Option<i64> {
    value.as_i64()
}
