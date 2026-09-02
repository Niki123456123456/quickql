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
        unzip_rows_info(),
        join_rows_info(),
        join_rows_index_info(),
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

#[fn_info()]
fn unzip_rows(input: &Value) -> Value {
    let Some(rows) = input.as_array() else {
        return Value::Null;
    };
    let Some(first_row) = rows.first() else {
        return Value::Object(serde_json::Map::new());
    };
    let Some(first_row) = first_row.as_object() else {
        return Value::Null;
    };

    let keys = first_row.keys().cloned().collect::<Vec<_>>();
    let rows = rows
        .iter()
        .map(Value::as_object)
        .collect::<Option<Vec<_>>>();
    let Some(rows) = rows else {
        return Value::Null;
    };
    if rows
        .iter()
        .any(|row| row.len() != keys.len() || keys.iter().any(|key| !row.contains_key(key)))
    {
        return Value::Null;
    }

    Value::Object(
        keys.into_iter()
            .map(|key| {
                let values = rows
                    .iter()
                    .map(|row| row.get(&key).unwrap().clone())
                    .collect();
                (key, Value::Array(values))
            })
            .collect(),
    )
}

/// Joins exactly two named arrays of objects using a shared field as their key.
///
/// The joined field is promoted to the output row and removed from both nested
/// objects. Only keys that occur in both arrays are included.
#[fn_info()]
fn join_rows(input: &Value, identifier: &str) -> Value {
    join_rows_with_key(input, identifier, |row, _| row.get(identifier).cloned())
}

/// Joins two named arrays of objects using a field from the first array and the
/// zero-based position of each item in the second array as its key.
#[fn_info()]
fn join_rows_index(input: &Value, identifier: &str) -> Value {
    join_rows_with_key(input, identifier, |_, index| {
        Some(Value::from(index as u64))
    })
}

fn join_rows_with_key<F>(input: &Value, identifier: &str, second_key: F) -> Value
where
    F: Fn(&serde_json::Map<String, Value>, usize) -> Option<Value>,
{
    let Some(input) = input.as_object() else {
        return Value::Null;
    };
    if input.len() != 2 {
        return Value::Null;
    }

    let Some(arrays) = input
        .iter()
        .map(|(name, values)| values.as_array().map(|values| (name, values)))
        .collect::<Option<Vec<_>>>()
    else {
        return Value::Null;
    };
    let [(first_name, first_values), (second_name, second_values)] = arrays.as_slice() else {
        return Value::Null;
    };

    let first_rows = first_values
        .iter()
        .map(Value::as_object)
        .collect::<Option<Vec<_>>>();
    let second_rows = second_values
        .iter()
        .map(Value::as_object)
        .collect::<Option<Vec<_>>>();
    let (Some(first_rows), Some(second_rows)) = (first_rows, second_rows) else {
        return Value::Null;
    };

    let mut rows = Vec::new();
    for first_row in first_rows {
        let Some(key) = first_row.get(identifier).cloned() else {
            return Value::Null;
        };
        let Some((_, second_row)) = second_rows
            .iter()
            .enumerate()
            .find(|(index, row)| second_key(row, *index).as_ref() == Some(&key))
        else {
            continue;
        };

        let mut first_value = first_row.clone();
        first_value.remove(identifier);
        let mut second_value = (*second_row).clone();
        second_value.remove(identifier);

        let mut row = serde_json::Map::new();
        row.insert(identifier.to_string(), key);
        row.insert(first_name.to_string(), Value::Object(first_value));
        row.insert(second_name.to_string(), Value::Object(second_value));
        rows.push(Value::Object(row));
    }

    rows.sort_by(|left, right| {
        compare_sort_values(
            left.as_object()
                .and_then(|row| row.get(identifier))
                .unwrap(),
            right
                .as_object()
                .and_then(|row| row.get(identifier))
                .unwrap(),
        )
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unzip_rows_converts_rows_to_column_arrays() {
        assert_eq!(
            unzip_rows(&json!([
                {"a": 42, "b": 2},
                {"a": 43, "b": 3}
            ])),
            json!({"a": [42, 43], "b": [2, 3]})
        );
    }

    #[test]
    fn unzip_rows_rejects_invalid_or_inconsistent_rows() {
        assert_eq!(unzip_rows(&json!({"a": 1})), Value::Null);
        assert_eq!(unzip_rows(&json!([{"a": 1}, 2])), Value::Null);
        assert_eq!(
            unzip_rows(&json!([{"a": 1}, {"a": 2, "b": 3}])),
            Value::Null
        );
        assert_eq!(unzip_rows(&json!([])), json!({}));
    }

    #[test]
    fn join_rows_joins_two_arrays_on_the_given_identifier() {
        let input = json!({
            "a": [{"f": "hello", "i": 1}, {"f": "world", "i": 0}],
            "b": [{"f": "a", "i": 0}, {"f": "b", "i": 1}]
        });

        assert_eq!(
            join_rows(&input, "i"),
            json!([
                {"i": 0, "a": {"f": "world"}, "b": {"f": "a"}},
                {"i": 1, "a": {"f": "hello"}, "b": {"f": "b"}}
            ])
        );
    }

    #[test]
    fn join_rows_index_uses_the_second_array_position_as_the_key() {
        let input = json!({
            "a": [{"f": "hello", "i": 1}, {"f": "world", "i": 0}],
            "b": [{"f": "a"}, {"f": "b"}]
        });

        assert_eq!(
            join_rows_index(&input, "i"),
            json!([
                {"i": 0, "a": {"f": "world"}, "b": {"f": "a"}},
                {"i": 1, "a": {"f": "hello"}, "b": {"f": "b"}}
            ])
        );
    }

    #[test]
    fn join_rows_rejects_invalid_input() {
        assert_eq!(join_rows(&json!({"a": []}), "i"), Value::Null);
        assert_eq!(join_rows(&json!({"a": [{}], "b": []}), "i"), Value::Null);
        assert_eq!(join_rows(&json!({"a": [1], "b": []}), "i"), Value::Null);
    }
}
