use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use quickql_macros::fn_info;
use rand::{distributions::Alphanumeric, Rng};
use serde_json::Value;

use crate::{flatten_value, value_to_string, FnInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![
        new_line_info(),
        concat_info(),
        base64_info(),
        len_info(),
        split_info(),
        join_string_info(),
        random_string_info(),
    ]
}

#[fn_info]
fn new_line() -> String {
    "\n".into()
}

#[fn_info()]
fn concat(values: &[Value]) -> String {
    values
        .iter()
        .flat_map(flatten_value)
        .map(value_to_string)
        .collect()
}

#[fn_info()]
fn base64(value: String) -> String {
    BASE64_STANDARD.encode(value)
}

#[fn_info]
fn len(value: &str) -> usize {
    value.chars().count()
}

#[fn_info()]
fn split(input: &str, max_part_length: usize) -> Value {
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

#[fn_info()]
fn join_string(input: &Value, separator: &str) -> String {
    flatten_value(input)
        .map(value_to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

#[fn_info()]
fn random_string(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}
