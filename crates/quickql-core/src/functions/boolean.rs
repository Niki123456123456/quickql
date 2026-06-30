use quickql_macros::fn_info;
use serde_json::Value;

use crate::{FnInfo, JsonTypeInfo, ParamInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![eq_info(), or_info(), and_info()]
}

#[fn_info()]
fn eq(left: &Value, right: &Value) -> bool {
    left == right
}

#[fn_info()]
fn r#or(values: &[Value]) -> bool {
    values.iter().any(value_truthy)
}

#[fn_info()]
fn r#and(values: &[Value]) -> bool {
    values.iter().all(value_truthy)
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
