use crate::*;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub(crate) fn load_json_source(path: &Path) -> Result<QueryResult> {
    let file =
        File::open(path).with_context(|| format!("Opening JSON source {}", path.display()))?;
    let value: Value =
        serde_json::from_reader(BufReader::new(file)).context("Parsing JSON source")?;
    query_result_from_json_value(value)
}

pub(crate) fn load_json_http_source(uri: &str) -> Result<QueryResult> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("Building HTTP client")?;
    let request = client.get(uri);
    // for header in headers {
    //     request = request.header(header.name.as_str(), header.value.as_str());
    // }

    let value: Value = request
        .send()
        .with_context(|| format!("GET JSON source {uri}"))?
        .error_for_status()
        .with_context(|| format!("GET JSON source {uri} returned an error status"))?
        .json()
        .with_context(|| format!("Parsing JSON response from {uri}"))?;
    query_result_from_json_value(value)
}

pub(crate) fn query_result_from_json_value(value: Value) -> Result<QueryResult> {
    let items: Vec<Value> = match value {
        Value::Array(items) => items,
        single => vec![single],
    };
    Ok(QueryResult::new(items))
}
