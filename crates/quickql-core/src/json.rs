use anyhow::{Context, Result};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub(crate) fn load_json_source(path: &Path) -> Result<Value> {
    let file =
        File::open(path).with_context(|| format!("Opening JSON source {}", path.display()))?;
    let value: Value =
        serde_json::from_reader(BufReader::new(file)).context("Parsing JSON source")?;
    Ok(value)
}

pub(crate) fn load_json_http_source(uri: &str, headers: HashMap<&str, &str>) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("Building HTTP client")?;
    let mut request = client.get(uri);
    for (name, value) in headers {
        let name = HeaderName::from_str(name)
            .with_context(|| format!("Invalid HTTP header name {name}"))?;
        let value = HeaderValue::from_str(value)
            .with_context(|| format!("Invalid HTTP header value for {name}"))?;
        request = request.header(name, value);
    }

    let value: Value = request
        .send()
        .with_context(|| format!("GET JSON source {uri}"))?
        .error_for_status()
        .with_context(|| format!("GET JSON source {uri} returned an error status"))?
        .json()
        .with_context(|| format!("Parsing JSON response from {uri}"))?;
    Ok(value)
}
