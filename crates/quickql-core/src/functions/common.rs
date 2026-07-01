use quickql_macros::fn_info;
use serde_json::Value;

use crate::{color as color_function, execution, FnInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![assign_info(), parse_json_info(), color_info()]
}

#[fn_info()]
fn assign(values: &[Value]) -> Value {
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

#[fn_info()]
fn parse_json(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or(Value::Null)
}

#[fn_info()]
fn color(index: usize) -> String {
    color_function::get_color(index as u64)
}
