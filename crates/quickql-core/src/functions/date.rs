use chrono::{Duration, Local, NaiveDate};
use quickql_macros::fn_info;
use serde_json::Value;

use crate::{execution::parse_date_key, flatten_value, FnInfo, JsonTypeInfo, ParamInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![
        add_date_info(),
        iso_date_info(),
        get_date_info(),
        min_date_info(),
        max_date_info(),
        today_info(),
    ]
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
        let Ok(Some(date_key)) = parse_date_key(date_text) else {
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

#[fn_info()]
fn add_date(date: &str, days: i64) -> Value {
    let Some(date) = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok() else {
        return Value::Null;
    };

    date.checked_add_signed(Duration::days(days))
        .map(|date| Value::String(date.to_string()))
        .unwrap_or(Value::Null)
}

#[fn_info()]
fn iso_date(date: &str) -> Value {
    let Some(date_key) = parse_date_key(date).ok().flatten() else {
        return Value::Null;
    };

    let year = date_key / 10_000;
    let month = (date_key / 100) % 100;
    let day = date_key % 100;
    Value::String(format!("{year:04}-{month:02}-{day:02}"))
}

#[fn_info()]
fn get_date(text: &str) -> Value {
    text.split('T')
        .next()
        .map(str::trim)
        .filter(|date| !date.is_empty())
        .map(|date| Value::String(date.to_string()))
        .unwrap_or(Value::Null)
}

#[fn_info()]
fn min_date(values: &[Value]) -> Value {
    aggregate_date(
        &values
            .iter()
            .flat_map(flatten_value)
            .cloned()
            .collect::<Vec<_>>(),
        DateAggregate::Min,
    )
}

#[fn_info()]
fn max_date(values: &[Value]) -> Value {
    aggregate_date(
        &values
            .iter()
            .flat_map(flatten_value)
            .cloned()
            .collect::<Vec<_>>(),
        DateAggregate::Max,
    )
}

#[fn_info]
fn today() -> String {
    Local::now().date_naive().to_string()
}
