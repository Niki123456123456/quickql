use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggFunc {
    Sum,
    Array,
    MinDate,
    MaxDate,
    Count,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregation {
    pub output: String,
    pub func: AggFunc,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub uri: String,
    pub headers: Vec<Header>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubQuery {
    From(Vec<Source>),
    ColumnFilter(Vec<SelectExpr>),
    RowFilter(Vec<Filter>),
    MapMany(String),
    GroupBy {
        keys: Vec<String>,
        aggregations: Vec<Aggregation>,
    },
    OrderBy(Vec<SortKey>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub steps: Vec<SubQuery>,
}

impl Query {
    pub fn source(&self) -> Option<&str> {
        self.steps.iter().find_map(|s| match s {
            SubQuery::From(sources) => sources.first().map(|source| source.uri.as_str()),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub column: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectExpr {
    All,
    Column(String),
    Alias { output: String, input: String },
    StaticString { output: String, value: String },
    GetDate { output: String, input: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub row: usize,
    pub offset: u64,
}

pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Box<dyn Iterator<Item = Result<Vec<Value>>> + Send>,
}

impl Default for QueryResult {
    fn default() -> Self {
        Self {
            columns: Default::default(),
            rows: Box::new(std::iter::empty::<Result<Vec<Value>>>()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum StreamMessage<'a> {
    Meta {
        columns: &'a [String],
        source: String,
    },
    Row {
        row: Vec<Value>,
    },
    Batch {
        start: usize,
        rows: &'a [Vec<Value>],
    },
    Done {
        #[serde(rename = "rowCount")]
        row_count: usize,
        #[serde(rename = "elapsedMs")]
        elapsed_ms: f64,
    },
}

pub const DEFAULT_BLOCK_SIZE: usize = 1000;
pub const DEFAULT_STREAM_BATCH_SIZE: usize = 1000;
const ALL_COLUMNS: &str = "*";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub fn parse_query(input: &str) -> Result<Query> {
    let mut steps = Vec::new();

    for raw_line in input.lines() {
        let line = raw_line.split("--").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let upper = line.to_ascii_uppercase();
        if upper.starts_with("SOURCE ") {
            steps.push(SubQuery::From(parse_sources(line)?));
        } else if upper.starts_with("MAP ") {
            steps.push(SubQuery::ColumnFilter(parse_select_exprs(&line[3..])?));
        } else if upper.starts_with("FILTER ") {
            steps.push(SubQuery::RowFilter(parse_row_filter(&line[6..])?));
        } else if upper.starts_with("MAP_MANY ") {
            steps.push(SubQuery::MapMany(parse_map_many(line)?));
        } else if upper.starts_with("GROUP_BY ") {
            steps.push(parse_group_by(line)?);
        } else if upper.starts_with("SORT_BY ") {
            steps.push(parse_order_by(line)?);
        } else {
            bail!("Unsupported query line: {line}");
        }
    }
    Ok(Query { steps })
}

pub fn stream_query_jsonl<W: Write>(
    query_path: &Path,
    writer: &mut W,
    batch_size: usize,
) -> Result<()> {
    let start = Instant::now();
    let query_text = fs::read_to_string(query_path)
        .with_context(|| format!("Reading query file {}", query_path.display()))?;
    let query = parse_query(&query_text)?;

    let result = execute_pipeline(&query, &query_path)
        .with_context(|| format!("excuting pipeline {}", query_path.display()))?;

    write_stream_message(
        writer,
        &StreamMessage::Meta {
            columns: &result.columns,
            source: query_path.display().to_string(),
        },
    )?;

    let mut row_count = 0usize;
    let mut batch_start = 0usize;
    let mut batch: Vec<Vec<Value>> = Vec::with_capacity(batch_size.max(1));

    for row_result in result.rows {
        let row = row_result.with_context(|| format!("Reading row {row_count}"))?;
        if batch.is_empty() {
            batch_start = row_count;
        }
        batch.push(row);
        row_count += 1;
        if batch.len() >= batch_size.max(1) {
            write_stream_message(
                writer,
                &StreamMessage::Batch {
                    start: batch_start,
                    rows: &batch,
                },
            )?;
            batch.clear();
        }
    }

    if !batch.is_empty() {
        write_stream_message(
            writer,
            &StreamMessage::Batch {
                start: batch_start,
                rows: &batch,
            },
        )?;
    }

    write_stream_message(
        writer,
        &StreamMessage::Done {
            row_count,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        },
    )?;
    writer.flush()?;
    Ok(())
}

pub fn json_fields_for_query(query_path: &Path, query_text: &str) -> Result<Vec<String>> {
    let query = parse_query_lenient(query_text)?;
    if query.sources.is_empty() {
        return Ok(Vec::new());
    }

    fields_from_sources(query_path, &query.sources)
}

pub fn source_path_for_query(query_path: &Path, query_text: &str) -> Result<Option<PathBuf>> {
    let query = parse_query_lenient(query_text)?;
    Ok(query
        .sources
        .first()
        .map(String::as_str)
        .map(|source| resolve_source(query_path, source)))
}

pub fn json_fields_from_source_sample(source_path: &Path, max_rows: usize) -> Result<Vec<String>> {
    if is_csv_path(source_path) {
        return csv_fields_from_source(source_path);
    }
    if is_ql_path(source_path) {
        return fields_from_ql_source(source_path);
    }

    let mut file = File::open(source_path)
        .with_context(|| format!("Opening JSON source {}", source_path.display()))?;
    let sample_bytes = (max_rows.max(1) * 4096).clamp(64 * 1024, 1024 * 1024);
    let mut buffer = vec![0u8; sample_bytes];
    let read = file.read(&mut buffer)?;
    buffer.truncate(read);
    Ok(fields_from_json_prefix(&String::from_utf8_lossy(&buffer)))
}

pub fn fields_from_source_sample(source_path: &Path, max_rows: usize) -> Result<Vec<String>> {
    if is_csv_path(source_path) {
        csv_fields_from_source(source_path)
    } else if is_ql_path(source_path) {
        fields_from_ql_source(source_path)
    } else {
        json_fields_from_source_sample(source_path, max_rows)
    }
}

fn fields_from_sources(query_path: &Path, sources: &[String]) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    for source in sources {
        let source_path = resolve_source(query_path, source);
        let source_fields = fields_from_source_sample(&source_path, 100)?;
        for field in source_fields {
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    Ok(fields)
}

// Executes the query pipeline against the resolved source. Row filters are
// applied before column projection, matching SQL's logical processing order.
fn execute_pipeline(query: &Query, query_path: &Path) -> Result<QueryResult> {
    let mut ql_stack = Vec::new();
    if let Ok(canonical) = fs::canonicalize(query_path) {
        ql_stack.push(canonical);
    }
    execute_pipeline_with_stack(query, query_path, &mut ql_stack)
}

fn execute_pipeline_with_stack(
    query: &Query,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
) -> Result<QueryResult> {
    let mut result = QueryResult::default();

    for filter in &query.steps {
        match filter {
            SubQuery::From(sources) => {
                result = load_sources(query_path, sources, ql_stack)?;
            }
            SubQuery::ColumnFilter(columns) => {
                result = apply_column_filter(result, &columns)?;
            }
            SubQuery::RowFilter(filter) => {
                result = apply_row_filter(result, filter)?;
            }
            SubQuery::MapMany(field) => {
                result = apply_map_many(result, field)?;
            }
            SubQuery::GroupBy { keys, aggregations } => {
                result = apply_group_by(result, keys, aggregations)?;
            }
            SubQuery::OrderBy(sort_keys) => {
                result = apply_order_by(result, sort_keys)?;
            }
        }
    }

    Ok(result)
}

fn apply_column_filter(result: QueryResult, exprs: &[SelectExpr]) -> Result<QueryResult> {
    if exprs.len() == 1 && matches!(exprs[0], SelectExpr::All) {
        return Ok(result);
    }

    let mut new_columns = Vec::new();
    let mut projections = Vec::new();
    for expr in exprs {
        match expr {
            SelectExpr::All => {
                for column in &result.columns {
                    new_columns.push(column.clone());
                    projections.push(Projection::Column(ColumnPath::new(
                        &result.columns,
                        column,
                    )?));
                }
            }
            SelectExpr::Column(column) => {
                new_columns.push(column.clone());
                projections.push(Projection::Column(ColumnPath::new(
                    &result.columns,
                    column,
                )?));
            }
            SelectExpr::Alias { output, input } => {
                new_columns.push(output.clone());
                projections.push(Projection::Column(ColumnPath::new(&result.columns, input)?));
            }
            SelectExpr::StaticString { output, value } => {
                new_columns.push(output.clone());
                projections.push(Projection::Static(Value::String(value.clone())));
            }
            SelectExpr::GetDate { output, input } => {
                new_columns.push(output.clone());
                projections.push(Projection::GetDate(ColumnPath::new(
                    &result.columns,
                    input,
                )?));
            }
        }
    }

    let rows = result.rows.map(move |row_result| {
        row_result.and_then(|row| {
            projections
                .iter()
                .map(|projection| projection.value(&row))
                .collect()
        })
    });

    Ok(QueryResult {
        columns: new_columns,
        rows: Box::new(rows),
    })
}

fn apply_row_filter(result: QueryResult, filters: &[Filter]) -> Result<QueryResult> {
    let conditions: Vec<(usize, String)> = filters
        .iter()
        .map(|filter| {
            result
                .columns
                .iter()
                .position(|c| c == &filter.column)
                .ok_or_else(|| anyhow!("Column '{}' does not exist", filter.column))
                .map(|index| (index, filter.value.clone()))
        })
        .collect::<Result<_>>()?;

    let rows = result.rows.filter(move |row_result| match row_result {
        Ok(row) => conditions
            .iter()
            .any(|(col_index, filter_value)| match row.get(*col_index) {
                Some(value) => value_matches_filter(value, filter_value),
                None => false,
            }),
        Err(_) => true,
    });

    Ok(QueryResult {
        columns: result.columns,
        rows: Box::new(rows),
    })
}

fn value_matches_filter(value: &Value, filter_value: &str) -> bool {
    match value {
        Value::String(s) => s == filter_value,
        Value::Number(n) => n.to_string() == filter_value,
        Value::Bool(b) => b.to_string() == filter_value,
        Value::Null => filter_value.eq_ignore_ascii_case("null"),
        other => other.to_string() == filter_value,
    }
}

fn apply_map_many(result: QueryResult, field: &str) -> Result<QueryResult> {
    let index = result
        .columns
        .iter()
        .position(|column| column == field)
        .ok_or_else(|| anyhow!("Column '{field}' does not exist"))?;

    let mut items = Vec::new();
    for row_result in result.rows {
        let row = row_result?;
        match row.get(index) {
            Some(Value::Array(values)) => items.extend(values.iter().cloned()),
            Some(Value::Null) | None => {}
            Some(value) => bail!("MAP_MANY column '{field}' must be an array, got {value}"),
        }
    }

    Ok(query_result_from_json_items(items))
}

fn load_sources(
    query_path: &Path,
    sources: &[Source],
    ql_stack: &mut Vec<PathBuf>,
) -> Result<QueryResult> {
    let results = sources
        .iter()
        .map(|source| load_query_source(query_path, source, ql_stack))
        .collect::<Result<Vec<_>>>()?;
    append_results(results)
}

fn load_query_source(
    query_path: &Path,
    source: &Source,
    ql_stack: &mut Vec<PathBuf>,
) -> Result<QueryResult> {
    if is_http_uri(&source.uri) {
        return load_json_http_source(&source.uri, &source.headers);
    }

    let source_path = resolve_source(query_path, &source.uri);
    if is_csv_path(&source_path) {
        load_csv_source(&source_path)
    } else if is_ql_path(&source_path) {
        load_ql_source(&source_path, ql_stack)
    } else {
        load_json_source(&source_path)
    }
}

fn append_results(results: Vec<QueryResult>) -> Result<QueryResult> {
    if results.len() == 1 {
        return Ok(results.into_iter().next().unwrap());
    }

    let mut columns = Vec::new();
    for result in &results {
        for column in &result.columns {
            if !columns.contains(column) {
                columns.push(column.clone());
            }
        }
    }

    let mut iterators = Vec::new();
    for result in results {
        let mapping: Vec<Option<usize>> = columns
            .iter()
            .map(|column| {
                result
                    .columns
                    .iter()
                    .position(|source_column| source_column == column)
            })
            .collect();
        let rows = result.rows.map(move |row_result| {
            row_result.map(|row| {
                mapping
                    .iter()
                    .map(|index| {
                        index
                            .and_then(|index| row.get(index).cloned())
                            .unwrap_or(Value::Null)
                    })
                    .collect::<Vec<_>>()
            })
        });
        iterators.push(Box::new(rows) as Box<dyn Iterator<Item = Result<Vec<Value>>> + Send>);
    }

    Ok(QueryResult {
        columns,
        rows: Box::new(iterators.into_iter().flatten()),
    })
}

#[allow(dead_code)]
fn load_source(path: &Path) -> Result<QueryResult> {
    if is_csv_path(path) {
        load_csv_source(path)
    } else if is_ql_path(path) {
        let mut ql_stack = Vec::new();
        load_ql_source(path, &mut ql_stack)
    } else {
        load_json_source(path)
    }
}

fn load_csv_source(path: &Path) -> Result<QueryResult> {
    let mut reader = csv_reader_from_path(path)?;
    let columns: Vec<String> = reader
        .headers()
        .with_context(|| format!("Reading CSV headers {}", path.display()))?
        .iter()
        .map(ToString::to_string)
        .collect();
    let col_count = columns.len();
    let rows = reader.into_records().map(move |r| {
        r.context("Reading CSV row").map(|record| {
            (0..col_count)
                .map(|i| Value::String(record.get(i).unwrap_or_default().to_string()))
                .collect()
        })
    });
    Ok(QueryResult {
        columns,
        rows: Box::new(rows),
    })
}

fn load_json_source(path: &Path) -> Result<QueryResult> {
    let file =
        File::open(path).with_context(|| format!("Opening JSON source {}", path.display()))?;
    let value: Value =
        serde_json::from_reader(BufReader::new(file)).context("Parsing JSON source")?;
    query_result_from_json_value(value)
}

fn load_json_http_source(uri: &str, headers: &[Header]) -> Result<QueryResult> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("Building HTTP client")?;
    let mut request = client.get(uri);
    for header in headers {
        request = request.header(header.name.as_str(), header.value.as_str());
    }

    let value: Value = request
        .send()
        .with_context(|| format!("GET JSON source {uri}"))?
        .error_for_status()
        .with_context(|| format!("GET JSON source {uri} returned an error status"))?
        .json()
        .with_context(|| format!("Parsing JSON response from {uri}"))?;
    query_result_from_json_value(value)
}

fn load_ql_source(path: &Path, ql_stack: &mut Vec<PathBuf>) -> Result<QueryResult> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("Resolving QL source {}", path.display()))?;
    if ql_stack.contains(&canonical) {
        bail!("Recursive QL source reference: {}", path.display());
    }

    let query_text = fs::read_to_string(path)
        .with_context(|| format!("Reading QL source {}", path.display()))?;
    let query = parse_query(&query_text)
        .with_context(|| format!("Parsing QL source {}", path.display()))?;

    ql_stack.push(canonical);
    let result = execute_pipeline_with_stack(&query, path, ql_stack)
        .with_context(|| format!("Executing QL source {}", path.display()));
    ql_stack.pop();
    result
}

fn fields_from_ql_source(path: &Path) -> Result<Vec<String>> {
    let mut ql_stack = Vec::new();
    let result = load_ql_source(path, &mut ql_stack)?;
    Ok(result.columns)
}

fn query_result_from_json_value(value: Value) -> Result<QueryResult> {
    let items: Vec<Value> = match value {
        Value::Array(items) => items,
        single => vec![single],
    };
    Ok(query_result_from_json_items(items))
}

fn query_result_from_json_items(items: Vec<Value>) -> QueryResult {
    let columns = fields_from_value(&Value::Array(items.clone()));
    let columns_clone = columns.clone();
    let rows = items.into_iter().map(move |v| {
        let row: Vec<Value> = columns_clone
            .iter()
            .map(|col| v.get(col).cloned().unwrap_or(Value::Null))
            .collect();
        Ok(row)
    });
    QueryResult {
        columns,
        rows: Box::new(rows),
    }
}

#[derive(Default)]
struct PartialQuery {
    sources: Vec<String>,
}

fn parse_query_lenient(input: &str) -> Result<PartialQuery> {
    let mut partial = PartialQuery::default();
    for raw_line in input.lines() {
        let line = raw_line.split("--").next().unwrap_or("").trim();
        if line.to_ascii_uppercase().starts_with("SOURCE ") {
            partial.sources = parse_sources(line)
                .unwrap_or_default()
                .into_iter()
                .map(|source| source.uri)
                .collect();
        }
    }
    Ok(partial)
}

fn parse_sources(line: &str) -> Result<Vec<Source>> {
    let raw = line[6..].trim();
    let sources: Vec<Source> = split_source_parts(raw)
        .into_iter()
        .map(parse_source)
        .collect::<Result<_>>()?;
    if sources.is_empty() {
        bail!("SOURCE must include at least one source");
    }
    Ok(sources)
}

fn parse_source(raw: &str) -> Result<Source> {
    let (uri, rest) = parse_source_uri(raw)?;
    let rest = rest.trim();
    let headers = if rest.is_empty() {
        Vec::new()
    } else if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("HEADERS") {
        parse_headers(rest[7..].trim())?
    } else {
        bail!("Unsupported SOURCE clause after source: {rest}");
    };
    Ok(Source { uri, headers })
}

fn split_source_parts(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (i, c) in input.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }

        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == ',' && !has_headers_clause(&input[start..i]) {
            parts.push(input[start..i].trim());
            start = i + 1;
        }
    }

    parts.push(input[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn has_headers_clause(input: &str) -> bool {
    input.to_ascii_uppercase().contains(" HEADERS")
}

fn parse_source_uri(raw: &str) -> Result<(String, &str)> {
    let Some(first) = raw.chars().next() else {
        bail!("SOURCE must include a source");
    };

    if first == '\'' || first == '"' {
        let mut escaped = false;
        for (i, c) in raw.char_indices().skip(1) {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == first {
                return Ok((raw[1..i].to_string(), &raw[i + first.len_utf8()..]));
            }
        }
        bail!("Unterminated quoted SOURCE source");
    }

    let end = raw.find(char::is_whitespace).unwrap_or(raw.len());
    Ok((raw[..end].to_string(), &raw[end..]))
}

fn parse_headers(input: &str) -> Result<Vec<Header>> {
    let headers: Vec<Header> = split_header_parts(input)
        .into_iter()
        .map(parse_header)
        .collect::<Result<_>>()?;
    if headers.is_empty() {
        bail!("HEADERS must include at least one header");
    }
    Ok(headers)
}

fn parse_header(input: &str) -> Result<Header> {
    let Some((name, value)) = input.split_once('=') else {
        bail!("Expected HTTP header as Name = value but got: {input}");
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        bail!("HTTP header name cannot be empty");
    }
    Ok(Header {
        name,
        value: unquote(value.trim()).to_string(),
    })
}

fn split_header_parts(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (i, c) in input.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }

        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == ',' {
            parts.push(input[start..i].trim());
            start = i + 1;
        }
    }

    parts.push(input[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn unquote(value: &str) -> &str {
    if is_quoted(value) {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn is_quoted(value: &str) -> bool {
    value.len() >= 2
        && ((value.starts_with('\'') && value.ends_with('\''))
            || (value.starts_with('"') && value.ends_with('"')))
}

fn parse_select_exprs(input: &str) -> Result<Vec<SelectExpr>> {
    let exprs: Vec<SelectExpr> = split_top_level_commas(input)
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_select_expr)
        .collect::<Result<_>>()?;
    if exprs.is_empty() {
        bail!("MAP must include at least one column");
    }
    Ok(exprs)
}

fn parse_select_expr(input: &str) -> Result<SelectExpr> {
    if input == ALL_COLUMNS {
        return Ok(SelectExpr::All);
    }

    let Some((output, expr)) = input.split_once('=') else {
        return Ok(SelectExpr::Column(input.to_string()));
    };

    let output = output.trim();
    if output.is_empty() {
        bail!("MAP output column cannot be empty");
    }

    let expr = expr.trim();
    if is_quoted(expr) {
        return Ok(SelectExpr::StaticString {
            output: output.to_string(),
            value: unquote(expr).to_string(),
        });
    }

    let Some(paren) = expr.find('(') else {
        if expr.is_empty() {
            bail!("MAP expression input cannot be empty");
        }
        return Ok(SelectExpr::Alias {
            output: output.to_string(),
            input: expr.to_string(),
        });
    };
    if !expr.ends_with(')') {
        bail!("Expected MAP expression FUNC(input) but got: {expr}");
    }

    let func_name = expr[..paren].trim().to_ascii_uppercase();
    let input = expr[paren + 1..expr.len() - 1].trim();
    if input.is_empty() {
        bail!("MAP expression input cannot be empty");
    }

    match func_name.as_str() {
        "GETDATE" => Ok(SelectExpr::GetDate {
            output: output.to_string(),
            input: input.to_string(),
        }),
        other => bail!("Unknown MAP function: {other}"),
    }
}

fn parse_row_filter(input: &str) -> Result<Vec<Filter>> {
    let filters: Vec<Filter> = split_or_conditions(input)
        .into_iter()
        .map(parse_filter)
        .collect::<Result<_>>()?;

    if filters.is_empty() {
        bail!("FILTER must include at least one filter");
    }

    Ok(filters)
}

fn parse_filter(input: &str) -> Result<Filter> {
    let Some((column, value)) = input.split_once('=') else {
        bail!("FILTER currently supports equality filters joined by OR: FILTER column = value OR other = value");
    };

    let value = unquote(value.trim()).to_string();

    Ok(Filter {
        column: column.trim().to_string(),
        value,
    })
}

fn parse_map_many(line: &str) -> Result<String> {
    let field = line[8..].trim();
    if field.is_empty() {
        bail!("MAP_MANY requires a column name");
    }
    Ok(field.to_string())
}

fn split_or_conditions(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let bytes = input.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i] as char;
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }

        if c == '\'' || c == '"' {
            quote = Some(c);
            i += 1;
            continue;
        }

        if is_or_at(input, i) {
            parts.push(input[start..i].trim());
            i += 2;
            start = i;
            continue;
        }

        i += 1;
    }

    parts.push(input[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn is_or_at(input: &str, index: usize) -> bool {
    let bytes = input.as_bytes();
    if index + 2 > bytes.len()
        || !input.is_char_boundary(index)
        || !input.is_char_boundary(index + 2)
        || !input[index..index + 2].eq_ignore_ascii_case("OR")
    {
        return false;
    }

    let before = if index == 0 {
        None
    } else {
        input[..index].chars().next_back()
    };
    let after = input[index + 2..].chars().next();

    before.is_none_or(|c| !is_identifier_char(c)) && after.is_none_or(|c| !is_identifier_char(c))
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn parse_group_by(line: &str) -> Result<SubQuery> {
    let rest = line[8..].trim(); // skip "GROUP_BY"
    let upper = rest.to_ascii_uppercase();

    let (keys_str, aggregations) = if let Some(pos) = upper.find(" MAP ") {
        let aggs_str = rest[pos + 5..].trim();
        let aggregations = split_top_level_commas(aggs_str)
            .into_iter()
            .map(|expr| parse_aggregation(expr.trim()))
            .collect::<Result<Vec<_>>>()?;
        (&rest[..pos], aggregations)
    } else {
        (rest, Vec::new())
    };

    let keys: Vec<String> = keys_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    if keys.is_empty() {
        bail!("GROUP_BY requires at least one key column");
    }

    Ok(SubQuery::GroupBy { keys, aggregations })
}

fn parse_aggregation(expr: &str) -> Result<Aggregation> {
    let Some((output, func_expr)) = expr.split_once('=') else {
        bail!("Expected 'output = FUNC(input)' but got: {expr}");
    };
    let output = output.trim().to_string();
    let func_expr = func_expr.trim();

    let Some(paren) = func_expr.find('(') else {
        bail!("Expected function call FUNC(col) but got: {func_expr}");
    };
    if !func_expr.ends_with(')') {
        bail!("Expected function call FUNC(col) but got: {func_expr}");
    }

    let func_name = func_expr[..paren].trim().to_ascii_uppercase();
    let input = func_expr[paren + 1..func_expr.len() - 1].trim().to_string();

    let func = match func_name.as_str() {
        "SUM" => AggFunc::Sum,
        "ARRAY" => AggFunc::Array,
        "MINDATE" => AggFunc::MinDate,
        "MAXDATE" => AggFunc::MaxDate,
        "COUNT" => AggFunc::Count,
        other => bail!("Unknown aggregation function: {other}"),
    };

    Ok(Aggregation {
        output,
        func,
        input,
    })
}

fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in input.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

fn apply_group_by(
    result: QueryResult,
    keys: &[String],
    aggregations: &[Aggregation],
) -> Result<QueryResult> {
    let group_all = keys.len() == 1 && keys[0] == ALL_COLUMNS;
    let key_paths: Vec<ColumnPath> = if group_all {
        Vec::new()
    } else {
        keys.iter()
            .map(|k| ColumnPath::new(&result.columns, k))
            .collect::<Result<_>>()?
    };

    let agg_paths: Vec<ColumnPath> = aggregations
        .iter()
        .map(|a| ColumnPath::new(&result.columns, &a.input))
        .collect::<Result<_>>()?;

    let mut group_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (Vec<Value>, Vec<Vec<Value>>)> = HashMap::new();

    for row_result in result.rows {
        let row = row_result?;
        let key_str = if group_all {
            ALL_COLUMNS.to_string()
        } else {
            let key_vals = key_paths
                .iter()
                .map(|path| path.value(&row))
                .collect::<Vec<_>>();
            group_key(&key_vals)
        };
        if !groups.contains_key(&key_str) {
            group_order.push(key_str.clone());
            let key_vals = if group_all {
                Vec::new()
            } else {
                key_paths.iter().map(|path| path.value(&row)).collect()
            };
            groups.insert(key_str.clone(), (key_vals, Vec::new()));
        }
        groups.get_mut(&key_str).unwrap().1.push(row);
    }

    let output_columns: Vec<String> = if group_all {
        aggregations.iter().map(|a| a.output.clone()).collect()
    } else {
        keys.iter()
            .chain(aggregations.iter().map(|a| &a.output))
            .cloned()
            .collect()
    };

    let output_rows: Vec<Result<Vec<Value>>> = group_order
        .into_iter()
        .map(|key_str| {
            let (key_vals, rows) = groups.remove(&key_str).unwrap();
            let mut out_row = key_vals;
            for (agg, input_path) in aggregations.iter().zip(agg_paths.iter()) {
                let agg_val = match agg.func {
                    AggFunc::Sum => {
                        let sum: f64 = rows
                            .iter()
                            .map(|row| match input_path.value(row) {
                                Value::Number(n) => n.as_f64().unwrap_or(0.0),
                                Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
                                _ => 0.0,
                            })
                            .sum();
                        if sum.fract() == 0.0 && sum >= i64::MIN as f64 && sum <= i64::MAX as f64 {
                            serde_json::json!(sum as i64)
                        } else {
                            serde_json::json!(sum)
                        }
                    }
                    AggFunc::Array => {
                        Value::Array(rows.iter().map(|row| input_path.value(row)).collect())
                    }
                    AggFunc::MinDate => aggregate_date(&rows, input_path, DateAggregate::Min)?,
                    AggFunc::MaxDate => aggregate_date(&rows, input_path, DateAggregate::Max)?,
                    AggFunc::Count => serde_json::json!(rows.len()),
                };
                out_row.push(agg_val);
            }
            Ok(out_row)
        })
        .collect();

    Ok(QueryResult {
        columns: output_columns,
        rows: Box::new(output_rows.into_iter()),
    })
}

fn parse_order_by(line: &str) -> Result<SubQuery> {
    let rest = line[7..].trim(); // skip "SORT_BY"
    let keys: Vec<SortKey> = rest
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut tokens = part.splitn(2, |c: char| c.is_ascii_whitespace());
            let column = tokens.next().unwrap_or("").trim().to_string();
            if column.is_empty() {
                bail!("SORT_BY: empty column name");
            }
            let direction = match tokens
                .next()
                .map(str::trim)
                .map(str::to_ascii_uppercase)
                .as_deref()
            {
                Some("DESC") => SortDirection::Desc,
                _ => SortDirection::Asc,
            };
            Ok(SortKey { column, direction })
        })
        .collect::<Result<_>>()?;

    if keys.is_empty() {
        bail!("SORT_BY requires at least one column");
    }
    Ok(SubQuery::OrderBy(keys))
}

fn apply_order_by(result: QueryResult, sort_keys: &[SortKey]) -> Result<QueryResult> {
    let key_indices: Vec<(usize, &SortDirection)> = sort_keys
        .iter()
        .map(|k| {
            result
                .columns
                .iter()
                .position(|c| c == &k.column)
                .ok_or_else(|| anyhow!("Column '{}' does not exist", k.column))
                .map(|i| (i, &k.direction))
        })
        .collect::<Result<_>>()?;

    let mut rows: Vec<Vec<Value>> = result.rows.map(|r| r).collect::<Result<_>>()?;

    rows.sort_by(|a, b| {
        for &(idx, dir) in &key_indices {
            let av = a.get(idx).unwrap_or(&Value::Null);
            let bv = b.get(idx).unwrap_or(&Value::Null);
            let ord = compare_values(av, bv);
            let ord = if matches!(dir, SortDirection::Desc) {
                ord.reverse()
            } else {
                ord
            };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });

    Ok(QueryResult {
        columns: result.columns,
        rows: Box::new(rows.into_iter().map(Ok)),
    })
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (Value::Number(an), Value::Number(bn)) => an
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&bn.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(as_), Value::String(bs)) => as_.cmp(bs),
        (Value::Bool(ab), Value::Bool(bb)) => ab.cmp(bb),
        // cross-type: sort by type tag so the order is deterministic
        (a, b) => type_rank(a).cmp(&type_rank(b)),
    }
}

fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

#[derive(Debug, Clone)]
struct ColumnPath {
    root: usize,
    path: Vec<String>,
}

impl ColumnPath {
    fn new(columns: &[String], name: &str) -> Result<Self> {
        let mut parts = name.split('.').filter(|part| !part.is_empty());
        let root_name = parts
            .next()
            .ok_or_else(|| anyhow!("Column path cannot be empty"))?;
        let root = columns
            .iter()
            .position(|column| column == root_name)
            .ok_or_else(|| anyhow!("Column '{root_name}' does not exist"))?;
        Ok(Self {
            root,
            path: parts.map(ToString::to_string).collect(),
        })
    }

    fn value(&self, row: &[Value]) -> Value {
        let mut value = row.get(self.root).unwrap_or(&Value::Null);
        for part in &self.path {
            let Value::Object(map) = value else {
                return Value::Null;
            };
            value = map.get(part).unwrap_or(&Value::Null);
        }
        value.clone()
    }
}

#[derive(Debug, Clone)]
enum Projection {
    Column(ColumnPath),
    Static(Value),
    GetDate(ColumnPath),
}

impl Projection {
    fn value(&self, row: &[Value]) -> Result<Value> {
        match self {
            Projection::Column(path) => Ok(path.value(row)),
            Projection::Static(value) => Ok(value.clone()),
            Projection::GetDate(path) => get_date_value(path.value(row)),
        }
    }
}

fn get_date_value(value: Value) -> Result<Value> {
    let Value::String(text) = value else {
        return Ok(Value::Null);
    };

    let date = text.split('T').next().unwrap_or("").trim();
    if date.is_empty() {
        return Ok(Value::Null);
    }

    if parse_date_key(date)?.is_some() {
        Ok(Value::String(date.to_string()))
    } else {
        Ok(Value::Null)
    }
}

enum DateAggregate {
    Min,
    Max,
}

fn aggregate_date(
    rows: &[Vec<Value>],
    input_path: &ColumnPath,
    aggregate: DateAggregate,
) -> Result<Value> {
    let mut selected: Option<(i32, String)> = None;

    for row in rows {
        let value = input_path.value(row);
        let Value::String(date_text) = value else {
            continue;
        };
        let Some(date_key) = parse_date_key(&date_text)? else {
            continue;
        };

        let should_replace = match (&selected, &aggregate) {
            (None, _) => true,
            (Some((current, _)), DateAggregate::Min) => date_key < *current,
            (Some((current, _)), DateAggregate::Max) => date_key > *current,
        };

        if should_replace {
            selected = Some((date_key, date_text));
        }
    }

    Ok(selected
        .map(|(_, date_text)| Value::String(date_text))
        .unwrap_or(Value::Null))
}

fn parse_date_key(input: &str) -> Result<Option<i32>> {
    let date = input.trim();
    if date.is_empty() {
        return Ok(None);
    }

    let separator = if date.contains('.') {
        '.'
    } else if date.contains('-') {
        '-'
    } else {
        return Ok(None);
    };
    let parts: Vec<&str> = date.split(separator).collect();
    if parts.len() != 3 {
        return Ok(None);
    }

    let (year_part, month_part, day_part) = if separator == '-' && parts[0].len() == 4 {
        (parts[0], parts[1], parts[2])
    } else {
        (parts[2], parts[1], parts[0])
    };

    let day: u32 = day_part
        .parse()
        .with_context(|| format!("Parsing date day in '{input}'"))?;
    let month: u32 = month_part
        .parse()
        .with_context(|| format!("Parsing date month in '{input}'"))?;
    let year: i32 = year_part
        .parse()
        .with_context(|| format!("Parsing date year in '{input}'"))?;

    if !is_valid_date(year, month, day) {
        bail!("Invalid date '{input}'");
    }

    Ok(Some(year * 10_000 + month as i32 * 100 + day as i32))
}

fn is_valid_date(year: i32, month: u32, day: u32) -> bool {
    if month == 0 || month > 12 || day == 0 {
        return false;
    }
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return false,
    };
    day <= max_day
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn group_key(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| match value {
            Value::String(s) => format!("s\x01{s}\x02"),
            Value::Number(n) => format!("n\x01{n}\x02"),
            Value::Bool(b) => format!("b\x01{b}\x02"),
            Value::Null => "N\x01\x02".to_string(),
            other => format!("j\x01{other}\x02"),
        })
        .collect::<Vec<_>>()
        .concat()
}

fn resolve_source(query_path: &Path, source: &str) -> PathBuf {
    let source_path = PathBuf::from(source);
    if source_path.is_absolute() {
        source_path
    } else {
        query_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(source_path)
    }
}

fn write_stream_message<W: Write>(writer: &mut W, message: &StreamMessage<'_>) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn is_csv_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
}

fn is_ql_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ql"))
}

fn is_http_uri(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("quickql-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn select_can_rename_columns_and_add_static_strings() -> Result<()> {
        let result = QueryResult {
            columns: vec!["count".to_string(), "name".to_string()],
            rows: Box::new(
                vec![
                    Ok(vec![Value::from(3), Value::String("alpha".to_string())]),
                    Ok(vec![Value::from(5), Value::String("beta".to_string())]),
                ]
                .into_iter(),
            ),
        };
        let exprs = parse_select_exprs(r#"length=count, text="test... ""#)?;

        let filtered = apply_column_filter(result, &exprs)?;
        let rows = filtered.rows.collect::<Result<Vec<_>>>()?;

        assert_eq!(filtered.columns, vec!["length", "text"]);
        assert_eq!(
            rows,
            vec![
                vec![Value::from(3), Value::String("test... ".to_string())],
                vec![Value::from(5), Value::String("test... ".to_string())],
            ]
        );
        Ok(())
    }

    #[test]
    fn select_alias_keeps_function_expressions_available() -> Result<()> {
        assert_eq!(
            parse_select_expr(r#"date=GETDATE(created_at)"#)?,
            SelectExpr::GetDate {
                output: "date".to_string(),
                input: "created_at".to_string(),
            }
        );
        assert_eq!(
            parse_select_expr("length=count")?,
            SelectExpr::Alias {
                output: "length".to_string(),
                input: "count".to_string(),
            }
        );
        assert_eq!(
            parse_select_expr(r#"text="test... ""#)?,
            SelectExpr::StaticString {
                output: "text".to_string(),
                value: "test... ".to_string(),
            }
        );
        Ok(())
    }

    #[test]
    fn ql_source_executes_inner_query_then_continues_pipeline() -> Result<()> {
        let dir = test_dir("ql-source");
        let nested_dir = dir.join("nested");
        fs::create_dir_all(&nested_dir)?;
        fs::write(
            nested_dir.join("data.csv"),
            "search,count\nalpha,3\nbeta,5\n",
        )?;
        fs::write(
            nested_dir.join("inner.ql"),
            "SOURCE 'data.csv'\nMAP length=count, search\n",
        )?;
        let outer_path = dir.join("outer.ql");
        fs::write(
            &outer_path,
            "SOURCE 'nested/inner.ql'\nFILTER search = alpha\nMAP length, text=\"nested\"\n",
        )?;

        let mut out = Vec::new();
        stream_query_jsonl(&outer_path, &mut out, 1000)?;
        let text = String::from_utf8(out)?;

        assert!(text.contains(r#""columns":["length","text"]"#));
        assert!(text.contains(r#"["3","nested"]"#));
        assert!(!text.contains(r#"["5","nested"]"#));

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn ql_source_rejects_recursive_references() -> Result<()> {
        let dir = test_dir("ql-recursion");
        let query_path = dir.join("self.ql");
        fs::write(&query_path, "SOURCE 'self.ql'\nMAP *\n")?;

        let query_text = fs::read_to_string(&query_path)?;
        let query = parse_query(&query_text)?;
        let err = match execute_pipeline(&query, &query_path) {
            Ok(_) => bail!("recursive query unexpectedly succeeded"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("Recursive QL source reference"));

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn from_supports_multiple_sources_and_appends_rows() -> Result<()> {
        let dir = test_dir("multi-source");
        fs::write(dir.join("one.csv"), "name,count\nalpha,3\n")?;
        fs::write(dir.join("two.csv"), "name,score\nbeta,7\n")?;
        let query_path = dir.join("query.ql");
        fs::write(
            &query_path,
            "SOURCE 'one.csv', 'two.csv'\nMAP name, count, score\n",
        )?;

        let mut out = Vec::new();
        stream_query_jsonl(&query_path, &mut out, 1000)?;
        let text = String::from_utf8(out)?;

        assert!(text.contains(r#""columns":["name","count","score"]"#));
        assert!(text.contains(r#"["alpha","3",null]"#));
        assert!(text.contains(r#"["beta",null,"7"]"#));

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn parses_comma_separated_sources_without_splitting_quotes_or_headers() -> Result<()> {
        assert_eq!(
            parse_sources(r#"SOURCE 'one,two.csv', three.csv"#)?,
            vec![
                Source {
                    uri: "one,two.csv".to_string(),
                    headers: Vec::new(),
                },
                Source {
                    uri: "three.csv".to_string(),
                    headers: Vec::new(),
                },
            ]
        );

        assert_eq!(
            parse_sources(r#"SOURCE 'https://example.test/users' HEADERS A = one, B = two"#)?,
            vec![Source {
                uri: "https://example.test/users".to_string(),
                headers: vec![
                    Header {
                        name: "A".to_string(),
                        value: "one".to_string(),
                    },
                    Header {
                        name: "B".to_string(),
                        value: "two".to_string(),
                    },
                ],
            }]
        );

        Ok(())
    }
}

fn fields_from_value(value: &Value) -> Vec<String> {
    let mut fields = BTreeSet::new();
    match value {
        Value::Array(items) => {
            for item in items.iter().take(100) {
                collect_fields(item, &mut fields);
            }
        }
        item => collect_fields(item, &mut fields),
    }
    fields.into_iter().collect()
}

fn collect_fields(value: &Value, fields: &mut BTreeSet<String>) {
    if let Value::Object(map) = value {
        for key in map.keys() {
            fields.insert(key.to_string());
        }
    }
}

fn fields_from_json_prefix(input: &str) -> Vec<String> {
    let bytes = input.as_bytes();
    let mut fields = BTreeSet::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }

        let start = i;
        i += 1;
        let mut escaped = false;
        while i < bytes.len() {
            if escaped {
                escaped = false;
            } else if bytes[i] == b'\\' {
                escaped = true;
            } else if bytes[i] == b'"' {
                break;
            }
            i += 1;
        }

        if i >= bytes.len() {
            break;
        }

        let end = i;
        i += 1;
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }

        if j < bytes.len() && bytes[j] == b':' {
            if let Ok(field) = serde_json::from_str::<String>(&input[start..=end]) {
                fields.insert(field);
            }
        }
    }

    fields.into_iter().collect()
}

fn csv_fields_from_source(source_path: &Path) -> Result<Vec<String>> {
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
