use quickql_macros::fn_info;
use serde_json::Value;

use crate::{flatten_value, FnInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![
        sum_info(),
        min_info(),
        to_number_info(),
        ceil_info(),
        floor_info(),
    ]
}

#[fn_info()]
fn sum(values: &[Value]) -> Value {
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

#[fn_info()]
fn min(values: &[Value]) -> Value {
    values
        .iter()
        .flat_map(flatten_value)
        .filter_map(|value| number_from_value(Some(value)))
        .reduce(f64::min)
        .map(json_number_value)
        .unwrap_or(Value::Null)
}

#[fn_info]
fn to_number(value: &Value) -> Option<f64> {
    number_from_value(Some(value))
}

#[fn_info]
fn ceil(value: f64) -> i64 {
    value.ceil() as i64
}

#[fn_info]
fn floor(value: f64) -> i64 {
    value.floor() as i64
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_comma_and_rounds_down() {
        let value = to_number(&Value::String("82,3125".into())).unwrap();

        assert_eq!(value, 82.3125);
        assert_eq!(floor(value), 82);
    }
}
