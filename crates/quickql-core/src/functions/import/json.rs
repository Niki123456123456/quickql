use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_PAGES: usize = 1000;

pub(crate) fn load_json_source(path: &Path) -> Result<Value> {
    let file =
        File::open(path).with_context(|| format!("Opening JSON source {}", path.display()))?;
    let value: Value =
        serde_json::from_reader(BufReader::new(file)).context("Parsing JSON source")?;
    Ok(value)
}

pub(crate) fn load_json_http_source(
    method: reqwest::Method,
    uri: &str,
    headers: HashMap<&str, &str>,
    body: Option<&Value>,
    paging: Option<&Value>,
) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("Building HTTP client")?;
    let method_label = method.as_str().to_string();
    let headers = headers
        .into_iter()
        .map(|(name, value)| {
            let name = HeaderName::from_str(name)
                .with_context(|| format!("Invalid HTTP header name {name}"))?;
            let value = HeaderValue::from_str(value)
                .with_context(|| format!("Invalid HTTP header value for {name}"))?;
            Ok((name, value))
        })
        .collect::<Result<Vec<_>>>()?;
    match paging.map(parse_paging_config).transpose()? {
        None => send_json_request(&client, method, uri, &headers, body, None, &method_label),
        Some(PagingConfig::Cursor { cursor_in, from }) => {
            let mut result = send_json_request(
                &client,
                method.clone(),
                uri,
                &headers,
                body,
                None,
                &method_label,
            )?;

            let mut seen_cursors = HashSet::new();
            for _ in 0..MAX_PAGES {
                let Some(cursor) = value_at_path(&result, &from.path).cloned() else {
                    return Ok(result);
                };
                if cursor.is_null() {
                    return Ok(result);
                }

                let cursor_key = value_to_cursor_string(&cursor);
                if cursor_key.is_empty() || !seen_cursors.insert(cursor_key) {
                    return Ok(result);
                }

                let page = send_json_request(
                    &client,
                    method.clone(),
                    uri,
                    &headers,
                    body,
                    Some((&cursor_in, &cursor)),
                    &method_label,
                )?;
                merge_page(&mut result, page);
            }

            bail!("HTTP JSON source {uri} exceeded pagination limit of {MAX_PAGES} pages")
        }
        Some(PagingConfig::Offset {
            offset_in,
            path,
            page_size,
        }) => {
            let mut result = Value::Null;
            let mut offset = 0usize;
            for page_index in 0..MAX_PAGES {
                let offset_value = serde_json::json!(offset);
                let page = send_json_request(
                    &client,
                    method.clone(),
                    uri,
                    &headers,
                    body,
                    Some((&offset_in, &offset_value)),
                    &method_label,
                )?;
                let entry_count = entry_count_at_path(&page, &path);
                if page_index == 0 {
                    result = page;
                } else {
                    merge_page(&mut result, page);
                }

                if entry_count < page_size {
                    return Ok(result);
                }

                offset += page_size;
            }

            bail!("HTTP JSON source {uri} exceeded pagination limit of {MAX_PAGES} pages")
        }
    }
}

fn send_json_request(
    client: &reqwest::blocking::Client,
    method: reqwest::Method,
    uri: &str,
    headers: &[(HeaderName, HeaderValue)],
    body: Option<&Value>,
    cursor: Option<(&PagingCursorLocation, &Value)>,
    method_label: &str,
) -> Result<Value> {
    let mut request = client.request(method, uri);
    for (name, value) in headers {
        request = request.header(name.clone(), value.clone());
    }

    let mut request_body = body.cloned();
    if let Some((cursor_in, cursor)) = cursor {
        match cursor_in.location {
            PagingLocation::Query => {
                let cursor_value = value_to_cursor_string(cursor);
                request = request.query(&[(cursor_in.path.as_str(), cursor_value.as_str())]);
            }
            PagingLocation::Body => {
                let mut body = request_body.unwrap_or_else(|| Value::Object(Default::default()));
                set_value_at_path(&mut body, &cursor_in.path, cursor.clone());
                request_body = Some(body);
            }
        }
    }

    if let Some(body) = request_body.as_ref() {
        request = request.json(body);
    }

    let value: Value = request
        .send()
        .with_context(|| format!("{method_label} JSON source {uri}"))?
        //.error_for_status()
        //.with_context(|| format!("{method_label} JSON source {uri} returned an error status"))?
        .json()
        .with_context(|| format!("Parsing JSON response from {uri}"))?;
    Ok(value)
}

#[derive(Debug, Clone)]
enum PagingConfig {
    Cursor {
        cursor_in: PagingCursorLocation,
        from: PagingCursorLocation,
    },
    Offset {
        offset_in: PagingCursorLocation,
        path: String,
        page_size: usize,
    },
}

#[derive(Debug, Clone)]
struct PagingCursorLocation {
    location: PagingLocation,
    path: String,
}

#[derive(Debug, Clone, Copy)]
enum PagingLocation {
    Query,
    Body,
}

fn parse_paging_config(value: &Value) -> Result<PagingConfig> {
    let obj = value
        .as_object()
        .context("HTTP paging config must be an object")?;
    let paging_type = obj
        .get("type")
        .and_then(Value::as_str)
        .context("HTTP paging config requires type")?;
    match paging_type.to_ascii_lowercase().as_str() {
        "cursor" => {
            let cursor_in = parse_paging_location(
                obj.get("in")
                    .context("HTTP cursor paging config requires in")?,
            )?;
            let from = parse_paging_location(
                obj.get("from")
                    .context("HTTP cursor paging config requires from")?,
            )?;
            if !matches!(from.location, PagingLocation::Body) {
                bail!("HTTP cursor paging from.location currently only supports body");
            }

            Ok(PagingConfig::Cursor { cursor_in, from })
        }
        "offset" => {
            let offset_in = parse_paging_location(
                obj.get("in")
                    .context("HTTP offset paging config requires in")?,
            )?;
            let path = obj
                .get("path")
                .and_then(Value::as_str)
                .context("HTTP offset paging config requires path")?
                .to_string();
            if path.trim().is_empty() {
                bail!("HTTP offset paging path cannot be empty");
            }
            let page_size = obj
                .get("pagesize")
                .and_then(Value::as_u64)
                .context("HTTP offset paging config requires pagesize")?
                as usize;
            if page_size == 0 {
                bail!("HTTP offset paging pagesize must be greater than zero");
            }

            Ok(PagingConfig::Offset {
                offset_in,
                path,
                page_size,
            })
        }
        _ => bail!("Unsupported HTTP paging type {paging_type}"),
    }
}

fn parse_paging_location(value: &Value) -> Result<PagingCursorLocation> {
    let obj = value
        .as_object()
        .context("HTTP paging location must be an object")?;
    let location = match obj
        .get("location")
        .and_then(Value::as_str)
        .context("HTTP paging location requires location")?
        .to_ascii_lowercase()
        .as_str()
    {
        "query" => PagingLocation::Query,
        "body" => PagingLocation::Body,
        location => bail!("Unsupported HTTP paging location {location}"),
    };
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .context("HTTP paging location requires path")?
        .to_string();
    if path.trim().is_empty() {
        bail!("HTTP paging location path cannot be empty");
    }

    Ok(PagingCursorLocation { location, path })
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, part| match current {
            Value::Object(map) => map.get(part),
            _ => None,
        })
}

fn entry_count_at_path(value: &Value, path: &str) -> usize {
    match value_at_path(value, path) {
        Some(Value::Array(values)) => values.len(),
        Some(Value::Object(values)) => values.len(),
        _ => 0,
    }
}

fn set_value_at_path(value: &mut Value, path: &str, new_value: Value) {
    let mut current = value;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if !current.is_object() {
                *current = Value::Object(Default::default());
            }
            if let Value::Object(map) = current {
                map.insert(part.to_string(), new_value);
            }
            return;
        }

        if !current.is_object() {
            *current = Value::Object(Default::default());
        }
        if let Value::Object(map) = current {
            current = map
                .entry(part.to_string())
                .or_insert_with(|| Value::Object(Default::default()));
        }
    }
}

fn value_to_cursor_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        value => value.to_string(),
    }
}

fn merge_page(result: &mut Value, page: Value) {
    match (result, page) {
        (Value::Array(result), Value::Array(mut page)) => result.append(&mut page),
        (Value::Object(result), Value::Object(page)) => {
            for (key, value) in page {
                match result.get_mut(&key) {
                    Some(existing) => merge_page(existing, value),
                    None => {
                        result.insert(key, value);
                    }
                }
            }
        }
        (result, page) => *result = page,
    }
}
