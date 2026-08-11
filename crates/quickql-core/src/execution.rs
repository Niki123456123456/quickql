use crate::csv::{csv_fields_from_source, load_csv_source};
use crate::json::{load_json_http_source, load_json_source};
use crate::parsing::{parse_query, parse_query_lenient};
use crate::{
    CaluculatedValue, FileProvider, FsFileProvider, FunctionProgressReporter, KeyDescriptor,
    MapExpr, MapMany, MapStep, Query, QueryResult, SortDirection, SortKey, StreamMessage, SubQuery,
    ALL_COLUMNS,
};
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

pub fn stream_query_jsonl<W: Write>(
    query_path: &Path,
    writer: &mut W,
    batch_size: usize,
) -> Result<()> {
    stream_query_jsonl_with_provider(query_path, writer, batch_size, &FsFileProvider)
}

pub fn execute_query(query_path: &Path) -> Result<QueryResult> {
    let query_text = std::fs::read_to_string(query_path)
        .with_context(|| format!("Reading query file {}", query_path.display()))?;
    let query = parse_query(&query_text)?;
    let mut ql_stack = Vec::new();
    if let Ok(canonical) = query_path.canonicalize() {
        ql_stack.push(canonical);
    }

    execute_pipeline_with_stack(&query, query_path, &mut ql_stack, &FsFileProvider)
        .with_context(|| format!("Executing pipeline {}", query_path.display()))
}

pub fn execute_query_with_progress<W: Write>(
    query_path: &Path,
    progress_writer: &mut W,
) -> Result<QueryResult> {
    let query_text = std::fs::read_to_string(query_path)
        .with_context(|| format!("Reading query file {}", query_path.display()))?;
    let query = parse_query(&query_text)?;

    execute_pipeline_streaming(&query, query_path, progress_writer, &FsFileProvider)
        .with_context(|| format!("Executing pipeline {}", query_path.display()))
}

pub fn stream_query_jsonl_with_provider<W: Write>(
    query_path: &Path,
    writer: &mut W,
    batch_size: usize,
    file_provider: &dyn FileProvider,
) -> Result<()> {
    let start = Instant::now();
    let query_text = file_provider
        .read_to_string(query_path)
        .with_context(|| format!("Reading query file {}", query_path.display()))?;
    let query = parse_query(&query_text)?;
    let result = execute_pipeline_streaming(&query, query_path, writer, file_provider)
        .with_context(|| format!("Executing pipeline {}", query_path.display()))?;
    let columns = columns_from_descriptor(&result.columns);

    write_stream_message(
        writer,
        &StreamMessage::Meta {
            columns: &columns,
            source: query_path.display().to_string(),
        },
    )?;

    let mut row_count = 0usize;
    let mut batch_start = 0usize;
    let mut batch: Vec<Value> = Vec::with_capacity(batch_size.max(1));

    for row in result.rows {
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

fn execute_pipeline_streaming<W: Write>(
    query: &Query,
    query_path: &Path,
    writer: &mut W,
    file_provider: &dyn FileProvider,
) -> Result<QueryResult> {
    let mut ql_stack = Vec::new();
    if let Ok(canonical) = file_provider.canonicalize(query_path) {
        ql_stack.push(canonical);
    }

    let mut result = QueryResult::default();
    let total_substeps = query.steps.len();

    for (index, step) in query.steps.iter().enumerate() {
        let substep = index + 1;
        let substep_name = subquery_name(step);
        let step_start = Instant::now();
        write_step_progress(
            writer,
            substep,
            total_substeps,
            substep_name,
            0.0,
            step_start,
        )?;

        result = match step {
            SubQuery::Source(sources) => load_sources_streaming(
                query_path,
                sources,
                &mut ql_stack,
                file_provider,
                &mut StepProgress::new(substep, total_substeps, substep_name, step_start),
                writer,
            )?,
            SubQuery::Map(map) => apply_map_streaming(
                result,
                map,
                query_path,
                &mut ql_stack,
                file_provider,
                &mut StepProgress::new(substep, total_substeps, substep_name, step_start),
                writer,
            )?,
            SubQuery::Filter(filter) => apply_filter_streaming(
                result,
                filter,
                query_path,
                &mut ql_stack,
                file_provider,
                &mut StepProgress::new(substep, total_substeps, substep_name, step_start),
                writer,
            )?,
            SubQuery::MapMany(map_many) => apply_map_many_streaming(
                result,
                map_many,
                &mut StepProgress::new(substep, total_substeps, substep_name, step_start),
                writer,
            )?,
            SubQuery::GroupBy { keys, mapping } => apply_group_by(
                result,
                keys,
                mapping,
                query_path,
                &mut ql_stack,
                file_provider,
            )?,
            SubQuery::SortBy(sort_keys) => apply_sort_by(result, sort_keys),
        };

        write_step_progress(
            writer,
            substep,
            total_substeps,
            substep_name,
            100.0,
            step_start,
        )?;
    }

    Ok(result)
}

struct StepProgress<'a> {
    substep: usize,
    total_substeps: usize,
    substep_name: &'a str,
    started_at: Instant,
    last_percent: f64,
}

impl<'a> StepProgress<'a> {
    fn new(
        substep: usize,
        total_substeps: usize,
        substep_name: &'a str,
        started_at: Instant,
    ) -> Self {
        Self {
            substep,
            total_substeps,
            substep_name,
            started_at,
            last_percent: 0.0,
        }
    }

    fn update<W: Write>(&mut self, writer: &mut W, completed: usize, total: usize) -> Result<()> {
        if total == 0 {
            return Ok(());
        }

        let percent = ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0);
        self.update_percent(writer, percent, self.substep_name)
    }

    fn update_percent<W: Write>(&mut self, writer: &mut W, percent: f64, name: &str) -> Result<()> {
        if percent < 100.0 && percent < self.last_percent + 1.0 {
            return Ok(());
        }

        self.last_percent = percent;
        write_step_progress(
            writer,
            self.substep,
            self.total_substeps,
            name,
            percent,
            self.started_at,
        )
    }
}

struct StepFunctionProgress<'progress, 'name, W> {
    step: &'progress mut StepProgress<'name>,
    writer: &'progress mut W,
    completed_rows: usize,
    total_rows: usize,
    error: Option<anyhow::Error>,
}

impl<W: Write> FunctionProgressReporter for StepFunctionProgress<'_, '_, W> {
    fn report(&mut self, name: &str, completed: usize, total: usize) {
        if total == 0 || self.total_rows == 0 || self.error.is_some() {
            return;
        }

        let row_fraction = (completed as f64 / total as f64).clamp(0.0, 1.0);
        let percent =
            ((self.completed_rows as f64 + row_fraction) / self.total_rows as f64) * 100.0;
        if let Err(error) = self.step.update_percent(self.writer, percent, name) {
            self.error = Some(error);
        }
    }
}

fn write_step_progress<W: Write>(
    writer: &mut W,
    substep: usize,
    total_substeps: usize,
    substep_name: &str,
    percent: f64,
    started_at: Instant,
) -> Result<()> {
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    let remaining_ms = if percent > 0.0 && percent < 100.0 {
        Some(elapsed_ms * ((100.0 - percent) / percent))
    } else if percent >= 100.0 {
        Some(0.0)
    } else {
        None
    };

    write_stream_message(
        writer,
        &StreamMessage::Progress {
            substep,
            total_substeps,
            substep_name,
            percent,
            elapsed_ms,
            remaining_ms,
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
    json_fields_from_source_sample_with_provider(source_path, max_rows, &FsFileProvider)
}

fn json_fields_from_source_sample_with_provider(
    source_path: &Path,
    max_rows: usize,
    file_provider: &dyn FileProvider,
) -> Result<Vec<String>> {
    if is_csv_path(source_path) {
        return csv_fields_from_source(source_path, file_provider);
    }
    if is_ql_path(source_path) {
        return fields_from_ql_source(source_path, file_provider);
    }

    let sample_bytes = (max_rows.max(1) * 4096).clamp(64 * 1024, 1024 * 1024);
    let mut buffer = file_provider
        .read_bytes(source_path)
        .with_context(|| format!("Opening JSON source {}", source_path.display()))?;
    buffer.truncate(sample_bytes.min(buffer.len()));
    Ok(fields_from_json_prefix(&String::from_utf8_lossy(&buffer)))
}

pub fn fields_from_source_sample(source_path: &Path, max_rows: usize) -> Result<Vec<String>> {
    fields_from_source_sample_with_provider(source_path, max_rows, &FsFileProvider)
}

fn fields_from_source_sample_with_provider(
    source_path: &Path,
    max_rows: usize,
    file_provider: &dyn FileProvider,
) -> Result<Vec<String>> {
    if is_csv_path(source_path) {
        csv_fields_from_source(source_path, file_provider)
    } else if is_ql_path(source_path) {
        fields_from_ql_source(source_path, file_provider)
    } else {
        json_fields_from_source_sample_with_provider(source_path, max_rows, file_provider)
    }
}

fn fields_from_sources(query_path: &Path, sources: &[String]) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    for source in sources {
        let source_path = resolve_source(query_path, source);
        let source_fields =
            fields_from_source_sample_with_provider(&source_path, 100, &FsFileProvider)?;
        for field in source_fields {
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    Ok(fields)
}

fn execute_pipeline_with_stack(
    query: &Query,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> Result<QueryResult> {
    let mut result = QueryResult::default();

    for step in &query.steps {
        result = match step {
            SubQuery::Source(sources) => {
                load_sources(query_path, sources, ql_stack, file_provider)?
            }
            SubQuery::Map(map) => apply_map(result, map, query_path, ql_stack, file_provider),
            SubQuery::Filter(filter) => {
                apply_filter(result, filter, query_path, ql_stack, file_provider)
            }
            SubQuery::MapMany(map_many) => apply_map_many(result, map_many)?,
            SubQuery::GroupBy { keys, mapping } => {
                apply_group_by(result, keys, mapping, query_path, ql_stack, file_provider)?
            }
            SubQuery::SortBy(sort_keys) => apply_sort_by(result, sort_keys),
        };
    }

    Ok(result)
}

fn apply_map(
    result: QueryResult,
    map: &MapStep,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> QueryResult {
    let mapping = &map.mapping;
    if mapping.len() == 1 && matches!(mapping[0], MapExpr::All) {
        return result;
    }

    let Some(parallelism) = map_parallelism(&map.config, result.rows.len()) else {
        let rows = result
            .rows
            .iter()
            .map(|row| map_row(row, mapping, query_path, ql_stack, file_provider))
            .collect();

        return QueryResult::new(rows);
    };

    apply_map_parallel(
        result,
        mapping,
        query_path,
        ql_stack,
        file_provider,
        parallelism,
    )
}

fn apply_map_streaming<W: Write>(
    result: QueryResult,
    map: &MapStep,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
    progress: &mut StepProgress<'_>,
    writer: &mut W,
) -> Result<QueryResult> {
    let mapping = &map.mapping;
    if mapping.len() == 1 && matches!(mapping[0], MapExpr::All) {
        return Ok(result);
    }

    if let Some(parallelism) = map_parallelism(&map.config, result.rows.len()) {
        return apply_map_parallel_streaming(
            result,
            mapping,
            query_path,
            ql_stack,
            file_provider,
            parallelism,
            progress,
            writer,
        );
    }

    let total = result.rows.len();
    let mut rows = Vec::with_capacity(total);
    for (index, row) in result.rows.iter().enumerate() {
        let mut function_progress = StepFunctionProgress {
            step: progress,
            writer,
            completed_rows: index,
            total_rows: total,
            error: None,
        };
        rows.push(map_row_with_progress(
            row,
            mapping,
            query_path,
            ql_stack,
            file_provider,
            &mut function_progress,
        ));
        if let Some(error) = function_progress.error {
            return Err(error);
        }
        progress.update(writer, index + 1, total)?;
    }

    Ok(QueryResult::new(rows))
}

fn apply_map_parallel(
    result: QueryResult,
    mapping: &[MapExpr],
    query_path: &Path,
    ql_stack: &[PathBuf],
    file_provider: &dyn FileProvider,
    parallelism: usize,
) -> QueryResult {
    let row_count = result.rows.len();
    let work = Mutex::new(result.rows.into_iter().enumerate().collect::<Vec<_>>());
    let output = Mutex::new(vec![None; row_count]);

    thread::scope(|scope| {
        for _ in 0..parallelism {
            scope.spawn(|| loop {
                let next = work.lock().unwrap().pop();
                let Some((index, row)) = next else {
                    break;
                };

                let mut ql_stack = ql_stack.to_vec();
                let mapped = map_row(&row, mapping, query_path, &mut ql_stack, file_provider);
                output.lock().unwrap()[index] = Some(mapped);
            });
        }
    });

    let rows = output
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|row| row.unwrap_or(Value::Null))
        .collect();

    QueryResult::new(rows)
}

fn apply_map_parallel_streaming<W: Write>(
    result: QueryResult,
    mapping: &[MapExpr],
    query_path: &Path,
    ql_stack: &[PathBuf],
    file_provider: &dyn FileProvider,
    parallelism: usize,
    progress: &mut StepProgress<'_>,
    writer: &mut W,
) -> Result<QueryResult> {
    let row_count = result.rows.len();
    let completed = AtomicUsize::new(0);
    let work = Mutex::new(result.rows.into_iter().enumerate().collect::<Vec<_>>());
    let output = Mutex::new(vec![None; row_count]);

    thread::scope(|scope| {
        for _ in 0..parallelism {
            scope.spawn(|| loop {
                let next = work.lock().unwrap().pop();
                let Some((index, row)) = next else {
                    break;
                };

                let mut ql_stack = ql_stack.to_vec();
                let mapped = map_row(&row, mapping, query_path, &mut ql_stack, file_provider);
                output.lock().unwrap()[index] = Some(mapped);
                completed.fetch_add(1, AtomicOrdering::Relaxed);
            });
        }

        loop {
            let done = completed.load(AtomicOrdering::Relaxed);
            progress.update(writer, done, row_count)?;
            if done >= row_count {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        Ok::<(), anyhow::Error>(())
    })?;

    let rows = output
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|row| row.unwrap_or(Value::Null))
        .collect();

    Ok(QueryResult::new(rows))
}

fn map_parallelism(config: &Value, row_count: usize) -> Option<usize> {
    if row_count <= 1 {
        return None;
    }

    let parallel = config.get("parallel").and_then(Value::as_u64)? as usize;
    if parallel <= 1 {
        return None;
    }

    Some(parallel.min(row_count))
}

fn map_row(
    row: &Value,
    mapping: &[MapExpr],
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> Value {
    map_row_with_progress(
        row,
        mapping,
        query_path,
        ql_stack,
        file_provider,
        &mut crate::NoopFunctionProgress,
    )
}

fn map_row_with_progress(
    row: &Value,
    mapping: &[MapExpr],
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
    progress: &mut dyn FunctionProgressReporter,
) -> Value {
    let mut output = Value::Object(Map::new());
    for expr in mapping {
        match expr {
            MapExpr::All => {
                if let Value::Object(map) = row {
                    output_as_object_mut(&mut output).extend(map.clone());
                }
            }
            MapExpr::Specific { column, value } => {
                let value = value.caluculate_with_progress(
                    row,
                    query_path,
                    ql_stack,
                    file_provider,
                    progress,
                );
                assign_output(&mut output, column, value);
            }
        }
    }
    output
}

fn apply_filter(
    result: QueryResult,
    filter: &CaluculatedValue,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> QueryResult {
    let rows = result
        .rows
        .into_iter()
        .filter(|row| value_truthy(&filter.caluculate(row, query_path, ql_stack, file_provider)))
        .collect();
    QueryResult::new(rows)
}

fn apply_filter_streaming<W: Write>(
    result: QueryResult,
    filter: &CaluculatedValue,
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
    progress: &mut StepProgress<'_>,
    writer: &mut W,
) -> Result<QueryResult> {
    let total = result.rows.len();
    let mut rows = Vec::new();
    for (index, row) in result.rows.into_iter().enumerate() {
        if value_truthy(&filter.caluculate(&row, query_path, ql_stack, file_provider)) {
            rows.push(row);
        }
        progress.update(writer, index + 1, total)?;
    }
    Ok(QueryResult::new(rows))
}

fn apply_map_many(result: QueryResult, map_many: &MapMany) -> Result<QueryResult> {
    let path = path_parts(&map_many.field);
    let include_paths = map_many
        .include
        .iter()
        .map(|column| (column, path_parts(column)))
        .collect::<Vec<_>>();
    let mut rows = Vec::new();

    for row in result.rows {
        match get_path(&row, &path) {
            Value::Array(values) if include_paths.is_empty() => rows.extend(values),
            Value::Array(values) => {
                for value in values {
                    let mut output = map_many_output(value, &path);

                    for (_, include_path) in &include_paths {
                        set_path(&mut output, include_path, get_path(&row, include_path));
                    }

                    rows.push(Value::Object(output));
                }
            }
            Value::Null => {}
            value => bail!(
                "MAP_MANY column '{}' must be an array, got {value}",
                map_many.field
            ),
        }
    }

    Ok(QueryResult::new(rows))
}

fn apply_map_many_streaming<W: Write>(
    result: QueryResult,
    map_many: &MapMany,
    progress: &mut StepProgress<'_>,
    writer: &mut W,
) -> Result<QueryResult> {
    let path = path_parts(&map_many.field);
    let include_paths = map_many
        .include
        .iter()
        .map(|column| (column, path_parts(column)))
        .collect::<Vec<_>>();
    let total = result.rows.len();
    let mut rows = Vec::new();

    for (index, row) in result.rows.into_iter().enumerate() {
        match get_path(&row, &path) {
            Value::Array(values) if include_paths.is_empty() => rows.extend(values),
            Value::Array(values) => {
                for value in values {
                    let mut output = map_many_output(value, &path);

                    for (_, include_path) in &include_paths {
                        set_path(&mut output, include_path, get_path(&row, include_path));
                    }

                    rows.push(Value::Object(output));
                }
            }
            Value::Null => {}
            value => bail!(
                "MAP_MANY column '{}' must be an array, got {value}",
                map_many.field
            ),
        }
        progress.update(writer, index + 1, total)?;
    }

    Ok(QueryResult::new(rows))
}

fn map_many_output(value: Value, default_path: &[String]) -> Map<String, Value> {
    match value {
        Value::Object(output) => output,
        value => {
            let mut output = Map::new();
            set_path(&mut output, default_path, value);
            output
        }
    }
}

fn load_sources(
    query_path: &Path,
    sources: &[CaluculatedValue],
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> Result<QueryResult> {
    let mut rows = Vec::new();
    for source in sources {
        let source = source.caluculate(&Value::Null, query_path, ql_stack, file_provider);
        if let Value::Array(array) = source {
            rows.extend(array);
        } else {
            rows.push(source);
        }
    }
    Ok(QueryResult::new(rows))
}

fn load_sources_streaming<W: Write>(
    query_path: &Path,
    sources: &[CaluculatedValue],
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
    progress: &mut StepProgress<'_>,
    writer: &mut W,
) -> Result<QueryResult> {
    let mut rows = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let source = source.caluculate(&Value::Null, query_path, ql_stack, file_provider);
        if let Value::Array(array) = source {
            rows.extend(array);
        } else {
            rows.push(source);
        }
        progress.update(writer, index + 1, sources.len())?;
    }
    Ok(QueryResult::new(rows))
}

pub fn load_query_source(
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
    source: &str,
    method: reqwest::Method,
    headers: HashMap<&str, &str>,
    body: Option<&Value>,
    paging: Option<&Value>,
) -> Result<Value> {
    if is_http_uri(source) {
        return load_json_http_source(method, source, headers, body, paging);
    }

    let source_path = resolve_source(query_path, source);
    if is_csv_path(&source_path) {
        load_csv_source(&source_path, file_provider)
    } else if is_ql_path(&source_path) {
        let result = load_ql_source(&source_path, ql_stack, file_provider)?;
        return Ok(Value::Array(result.rows));
    } else {
        load_json_source(&source_path, file_provider)
    }
}

fn load_ql_source(
    path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> Result<QueryResult> {
    let canonical = file_provider
        .canonicalize(path)
        .with_context(|| format!("Resolving QL source {}", path.display()))?;
    if ql_stack.contains(&canonical) {
        bail!("Recursive QL source reference: {}", path.display());
    }

    let query_text = file_provider
        .read_to_string(path)
        .with_context(|| format!("Reading QL source {}", path.display()))?;
    let query = parse_query(&query_text)
        .with_context(|| format!("Parsing QL source {}", path.display()))?;

    ql_stack.push(canonical);
    let result = execute_pipeline_with_stack(&query, path, ql_stack, file_provider)
        .with_context(|| format!("Executing QL source {}", path.display()));
    ql_stack.pop();
    result
}

fn fields_from_ql_source(path: &Path, file_provider: &dyn FileProvider) -> Result<Vec<String>> {
    let mut ql_stack = Vec::new();
    let result = load_ql_source(path, &mut ql_stack, file_provider)?;
    Ok(columns_from_descriptor(&result.columns))
}

fn apply_group_by(
    result: QueryResult,
    keys: &[String],
    mapping: &[MapExpr],
    query_path: &Path,
    ql_stack: &mut Vec<PathBuf>,
    file_provider: &dyn FileProvider,
) -> Result<QueryResult> {
    let group_all = keys.len() == 1 && keys[0] == ALL_COLUMNS;
    let key_paths: Vec<Vec<String>> = if group_all {
        Vec::new()
    } else {
        keys.iter().map(|key| path_parts(key)).collect()
    };

    let mut group_order = Vec::new();
    let mut groups: HashMap<String, (Vec<Value>, Vec<Value>)> = HashMap::new();

    for row in result.rows {
        let key_values = if group_all {
            Vec::new()
        } else {
            key_paths
                .iter()
                .map(|path| get_path(&row, path))
                .collect::<Vec<_>>()
        };
        let key = if group_all {
            ALL_COLUMNS.to_string()
        } else {
            group_key(&key_values)
        };
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
            groups.insert(key.clone(), (key_values, Vec::new()));
        }
        groups.get_mut(&key).unwrap().1.push(row);
    }

    let mut output_rows = Vec::new();
    for key in group_order {
        let (key_values, rows) = groups.remove(&key).unwrap();
        let mut output = Map::new();

        if !group_all {
            for (key, value) in keys.iter().zip(key_values) {
                set_path(&mut output, &path_parts(key), value);
            }
        }

        let group_value = grouped_rows_value(&rows);
        for expr in mapping {
            match expr {
                MapExpr::All => {}
                MapExpr::Specific { column, value } => {
                    set_path(
                        &mut output,
                        column,
                        value.caluculate(&group_value, query_path, ql_stack, file_provider),
                    );
                }
            }
        }

        output_rows.push(Value::Object(output));
    }

    Ok(QueryResult::new(output_rows))
}

fn apply_sort_by(result: QueryResult, sort_keys: &[SortKey]) -> QueryResult {
    let mut rows = result.rows;
    rows.sort_by(|a, b| {
        for key in sort_keys {
            let path = path_parts(&key.column);
            let ord = compare_values(&get_path(a, &path), &get_path(b, &path));
            let ord = if matches!(key.direction, SortDirection::Desc) {
                ord.reverse()
            } else {
                ord
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    QueryResult::new(rows)
}

fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&b.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (a, b) => type_rank(a).cmp(&type_rank(b)),
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

pub fn parse_date_key(input: &str) -> Result<Option<i32>> {
    let date = input.split('T').next().unwrap_or("").trim();
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

fn get_path(value: &Value, path: &[String]) -> Value {
    if path.is_empty() {
        return value.clone();
    }

    path.iter()
        .try_fold(value, |current, part| match current {
            Value::Object(map) => map.get(part),
            _ => None,
        })
        .cloned()
        .unwrap_or(Value::Null)
}

fn is_whole_output_column(path: &[String]) -> bool {
    path.len() == 1 && path[0] == "$"
}

pub(crate) fn assign_output(output: &mut Value, path: &[String], value: Value) {
    if is_whole_output_column(path) {
        *output = value;
    } else {
        set_path(output_as_object_mut(output), path, value);
    }
}

fn output_as_object_mut(output: &mut Value) -> &mut Map<String, Value> {
    if !output.is_object() {
        *output = Value::Object(Map::new());
    }
    output.as_object_mut().unwrap()
}

fn set_path(output: &mut Map<String, Value>, path: &[String], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };

    let mut current = output;
    for part in parents {
        let value = current
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !value.is_object() {
            *value = Value::Object(Map::new());
        }
        current = value.as_object_mut().unwrap();
    }
    current.insert(last.clone(), value);
}

fn grouped_rows_value(rows: &[Value]) -> Value {
    if rows.iter().any(|value| matches!(value, Value::Object(_)))
        && rows
            .iter()
            .all(|value| matches!(value, Value::Object(_) | Value::Null))
    {
        return Value::Object(grouped_object_rows(rows));
    }

    Value::Array(rows.to_vec())
}

fn grouped_object_rows(rows: &[Value]) -> Map<String, Value> {
    let keys = rows
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|object| object.keys().cloned())
        .collect::<BTreeSet<_>>();

    keys.into_iter()
        .map(|key| {
            let values = rows
                .iter()
                .map(|row| match row {
                    Value::Object(object) => object.get(&key).cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                })
                .collect::<Vec<_>>();
            (key, grouped_rows_value(&values))
        })
        .collect()
}

fn path_parts(input: &str) -> Vec<String> {
    input
        .split('.')
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
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

fn group_key(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| match value {
            Value::String(value) => format!("s\x01{value}\x02"),
            Value::Number(value) => format!("n\x01{value}\x02"),
            Value::Bool(value) => format!("b\x01{value}\x02"),
            Value::Null => "N\x01\x02".to_string(),
            value => format!("j\x01{value}\x02"),
        })
        .collect::<Vec<_>>()
        .concat()
}

fn columns_from_descriptor(descriptor: &KeyDescriptor) -> Vec<String> {
    match descriptor {
        KeyDescriptor::Value => vec![],
        KeyDescriptor::Object(fields) => {
            let mut keys: Vec<String> = fields.keys().cloned().collect();
            keys.sort();
            keys
        }
    }
}

fn subquery_name(step: &SubQuery) -> &'static str {
    match step {
        SubQuery::Source(_) => "Source",
        SubQuery::Map(_) => "Map",
        SubQuery::Filter(_) => "Filter",
        SubQuery::MapMany(_) => "Map many",
        SubQuery::GroupBy { .. } => "Group by",
        SubQuery::SortBy(_) => "Sort by",
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
    use serde_json::json;

    fn primitive_map_many_input() -> (QueryResult, MapMany) {
        (
            QueryResult::new(vec![json!({
                "day": "2026-07-14",
                "result": { "numbers": [1, 2] }
            })]),
            MapMany {
                field: "result.numbers".to_string(),
                include: vec!["day".to_string()],
            },
        )
    }

    fn assert_primitive_values_use_default_path(result: QueryResult) {
        assert_eq!(
            result.rows,
            vec![
                json!({ "result": { "numbers": 1 }, "day": "2026-07-14" }),
                json!({ "result": { "numbers": 2 }, "day": "2026-07-14" }),
            ]
        );
    }

    #[test]
    fn map_many_wraps_primitive_values_at_default_path_when_including_columns() {
        let (result, map_many) = primitive_map_many_input();

        assert_primitive_values_use_default_path(apply_map_many(result, &map_many).unwrap());
    }

    #[test]
    fn streaming_map_many_wraps_primitive_values_at_default_path_when_including_columns() {
        let (result, map_many) = primitive_map_many_input();
        let mut progress = StepProgress::new(1, 1, "MAP_MANY", Instant::now());
        let mut writer = Vec::new();

        let result =
            apply_map_many_streaming(result, &map_many, &mut progress, &mut writer).unwrap();

        assert_primitive_values_use_default_path(result);
    }

    #[test]
    fn streaming_nn_reports_function_progress() {
        let result = QueryResult::new(vec![json!({
            "rows": [[10.0, 0.0]],
            "neighbors": [[1.0, 0.0], [9.0, 1.0]],
        })]);
        let map = MapStep {
            config: Value::Object(Map::new()),
            mapping: vec![MapExpr::Specific {
                column: vec!["nearest".to_string()],
                value: CaluculatedValue::FunctionCall {
                    function: "nn".to_string(),
                    parameters: vec![
                        CaluculatedValue::Reference(vec!["rows".to_string()]),
                        CaluculatedValue::Reference(vec!["neighbors".to_string()]),
                        CaluculatedValue::Static(json!(1)),
                        CaluculatedValue::Static(json!("cosine")),
                    ],
                },
            }],
        };
        let mut progress = StepProgress::new(2, 3, "Map", Instant::now());
        let mut writer = Vec::new();

        let result = apply_map_streaming(
            result,
            &map,
            Path::new("query.ql"),
            &mut Vec::new(),
            &FsFileProvider,
            &mut progress,
            &mut writer,
        )
        .unwrap();

        assert_eq!(result.rows[0]["nearest"]["index"], json!([[0]]));
        assert_eq!(result.rows[0]["nearest"]["distance"], json!([[0.0]]));

        let messages = String::from_utf8(writer).unwrap();
        assert!(messages.lines().any(|line| {
            let message: Value = serde_json::from_str(line).unwrap();
            message["type"] == "progress"
                && message["substep"] == 2
                && message["totalSubsteps"] == 3
                && message["substepName"] == "NN"
                && message["percent"].as_f64().is_some_and(|value| value > 0.0)
        }));
    }
}
