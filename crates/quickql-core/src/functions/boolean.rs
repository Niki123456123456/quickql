use quickql_macros::fn_info;
use serde_json::Value;

use crate::FnInfo;

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![eq_info(), or_info(), and_info(), not_info()]
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

#[fn_info()]
fn not(value: &Value) -> bool {
    !value_truthy(value)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn not_negates_truthiness_for_all_value_types() {
        for value in [
            json!(false),
            json!(null),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(not(&value), "expected NOT({value}) to be true");
        }
        for value in [
            json!(true),
            json!(1),
            json!(-1),
            json!("false"),
            json!([false]),
            json!({"a": false}),
        ] {
            assert!(!not(&value), "expected NOT({value}) to be false");
        }
    }
}
