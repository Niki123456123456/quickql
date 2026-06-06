use crate::QueryResult;
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub(crate) fn load_csv_source(path: &Path) -> Result<QueryResult> {
    let mut reader = csv_reader_from_path(path)?;
    let columns: Vec<String> = reader
        .headers()
        .with_context(|| format!("Reading CSV headers {}", path.display()))?
        .iter()
        .map(ToString::to_string)
        .collect();
    let rows = reader
        .into_records()
        .map(|record| {
            let record = record.context("Reading CSV row")?;
            let row = columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    (
                        column.clone(),
                        Value::String(record.get(index).unwrap_or_default().to_string()),
                    )
                })
                .collect::<Map<_, _>>();
            Ok(Value::Object(row))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(QueryResult::new(rows))
}

pub(crate) fn csv_fields_from_source(source_path: &Path) -> Result<Vec<String>> {
    let mut reader = csv_reader_from_path(source_path)?;
    Ok(reader
        .headers()
        .with_context(|| format!("Reading CSV headers {}", source_path.display()))?
        .iter()
        .map(ToString::to_string)
        .collect())
}

fn csv_reader_from_path(source_path: &Path) -> Result<csv::Reader<File>> {
    let delimiter = csv_delimiter_from_path(source_path)?;
    csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(source_path)
        .with_context(|| format!("Opening CSV source {}", source_path.display()))
}

fn csv_delimiter_from_path(source_path: &Path) -> Result<u8> {
    let mut file = File::open(source_path)
        .with_context(|| format!("Opening CSV source {}", source_path.display()))?;
    let mut buffer = [0u8; 8192];
    let bytes_read = file.read(&mut buffer)?;
    Ok(detect_csv_delimiter(&buffer[..bytes_read]))
}

fn detect_csv_delimiter(sample: &[u8]) -> u8 {
    let mut comma_count = 0usize;
    let mut semicolon_count = 0usize;
    let mut tab_count = 0usize;
    let mut in_quotes = false;
    let mut i = 0usize;

    while i < sample.len() {
        match sample[i] {
            b'"' => {
                if in_quotes && sample.get(i + 1) == Some(&b'"') {
                    i += 1;
                } else {
                    in_quotes = !in_quotes;
                }
            }
            b'\n' | b'\r' if !in_quotes => break,
            b',' if !in_quotes => comma_count += 1,
            b';' if !in_quotes => semicolon_count += 1,
            b'\t' if !in_quotes => tab_count += 1,
            _ => {}
        }
        i += 1;
    }

    [
        (b',', comma_count),
        (b';', semicolon_count),
        (b'\t', tab_count),
    ]
    .into_iter()
    .max_by_key(|(_, count)| *count)
    .filter(|(_, count)| *count > 0)
    .map(|(delimiter, _)| delimiter)
    .unwrap_or(b',')
}
