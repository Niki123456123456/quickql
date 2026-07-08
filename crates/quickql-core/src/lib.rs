use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use quickql_macros::fn_info;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[path = "functions/array.rs"]
mod array;
#[path = "functions/boolean.rs"]
mod boolean;
#[path = "functions/color.rs"]
mod color;
#[path = "functions/common.rs"]
mod common;
#[path = "functions/import/csv.rs"]
mod csv;
#[path = "functions/date.rs"]
mod date;
mod execution;
#[path = "functions/import/json.rs"]
mod json;
#[path = "functions/ml.rs"]
mod ml;
#[path = "functions/numbers.rs"]
mod numbers;
#[path = "functions/ml/optics.rs"]
mod optics;
mod parsing;
#[path = "functions/strings.rs"]
mod strings;
#[path = "functions/ml/tsne.rs"]
mod tsne;
#[path = "functions/ml/umap.rs"]
mod umap;

pub use execution::{
    fields_from_source_sample, json_fields_for_query, json_fields_from_source_sample,
    source_path_for_query, stream_query_jsonl, stream_query_jsonl_with_provider,
};
pub use parsing::parse_query;

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
pub enum SubQuery {
    Source(Vec<CaluculatedValue>),
    Map(MapStep),
    Filter(CaluculatedValue),
    MapMany(MapMany),
    GroupBy {
        keys: Vec<String>,
        mapping: Vec<MapExpr>,
    },
    SortBy(Vec<SortKey>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub steps: Vec<SubQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapStep {
    pub config: Value,
    pub mapping: Vec<MapExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapMany {
    pub field: String,
    pub include: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapExpr {
    All,
    Specific {
        column: Vec<String>,
        value: CaluculatedValue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaluculatedValue {
    Reference(Vec<String>),
    Static(Value),
    Object(Vec<(String, CaluculatedValue)>),
    Array(Vec<CaluculatedValue>),
    FunctionCall {
        function: String,
        parameters: Vec<CaluculatedValue>,
    },
}

impl CaluculatedValue {
    pub(crate) fn caluculate(
        &self,
        value: &Value,
        query_path: &Path,
        ql_stack: &mut Vec<PathBuf>,
        file_provider: &dyn FileProvider,
    ) -> Value {
        match self {
            CaluculatedValue::Reference(path) => path
                .iter()
                .try_fold(value, |current, part| {
                    if part == "$" {
                        return Some(current);
                    }

                    match current {
                        Value::Object(map) => map.get(part),
                        _ => None,
                    }
                })
                .cloned()
                .unwrap_or(Value::Null),
            CaluculatedValue::Static(value) => value.clone(),
            CaluculatedValue::Object(entries) => {
                let mut output = serde_json::Map::new();
                for (key, entry) in entries {
                    output.insert(
                        key.clone(),
                        entry.caluculate(value, query_path, ql_stack, file_provider),
                    );
                }
                Value::Object(output)
            }
            CaluculatedValue::Array(entries) => Value::Array(
                entries
                    .iter()
                    .map(|entry| entry.caluculate(value, query_path, ql_stack, file_provider))
                    .collect(),
            ),
            CaluculatedValue::FunctionCall {
                function,
                parameters,
            } => {
                let values: Vec<_> = parameters
                    .iter()
                    .map(|x| x.caluculate(value, query_path, ql_stack, file_provider))
                    .collect();

                let metaparams = MetaParameters {
                    query_path,
                    ql_stack,
                    file_provider,
                };

                fn_info_for_call(function, &values)
                    .map(|function_info| (function_info.function)(&values, metaparams))
                    .unwrap_or(Value::Null)
            }
        }
    }
}

struct MetaParameters<'a> {
    query_path: &'a Path,
    ql_stack: &'a mut Vec<PathBuf>,
    file_provider: &'a dyn FileProvider,
}

pub trait FileProvider: Sync {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_to_string(path).map(String::into_bytes)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
}

#[derive(Debug, Default)]
pub struct FsFileProvider;

impl FileProvider for FsFileProvider {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
}
#[fn_info()]
fn get(values: &[Value], params: MetaParameters) -> Value {
    CaluculatedValue::Reference(
        values
            .iter()
            .skip(1)
            .filter_map(|x| x.as_str())
            .map(|x| x.to_string())
            .collect(),
    )
    .caluculate(
        values.first().unwrap_or_default(),
        params.query_path,
        params.ql_stack,
        params.file_provider,
    )
}
#[fn_info()]
fn open(value: OneOf<String, SourceConfig>, params: MetaParameters) -> Value {
    open_source(value, reqwest::Method::GET, params)
}
#[fn_info()]
fn post(value: OneOf<String, SourceConfig>, params: MetaParameters) -> Value {
    open_source(value, reqwest::Method::POST, params)
}
#[fn_info()]
fn put(value: OneOf<String, SourceConfig>, params: MetaParameters) -> Value {
    open_source(value, reqwest::Method::PUT, params)
}

enum OneOf<A, B> {
    A(A),
    B(B),
}

#[derive(Debug, Clone, Deserialize)]
struct SourceConfig {
    src: String,
    headers: Option<HashMap<String, String>>,
    body: Option<Value>,
    paging: Option<Value>,
}

fn open_source(
    value: OneOf<String, SourceConfig>,
    method: reqwest::Method,
    params: MetaParameters,
) -> Value {
    match value {
        OneOf::A(source) => {
            return execution::load_query_source(
                params.query_path,
                params.ql_stack,
                params.file_provider,
                &source,
                method,
                Default::default(),
                None,
                None,
            )
            .unwrap_or_default();
        }
        OneOf::B(config) => {
            let mut headers: HashMap<&str, &str> = Default::default();
            if let Some(source_headers) = &config.headers {
                for (key, value) in source_headers.iter() {
                    headers.insert(key.as_str(), value.as_str());
                }
            }
            return execution::load_query_source(
                params.query_path,
                params.ql_stack,
                params.file_provider,
                &config.src,
                method,
                headers,
                config.body.as_ref(),
                config.paging.as_ref(),
            )
            .unwrap_or_default();
        }
    }
}

fn flatten_value(value: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match value {
        Value::Array(values) => Box::new(values.iter()),
        value => Box::new(std::iter::once(value)),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        value => value.to_string(),
    }
}

#[allow(dead_code)]
struct FnInfo {
    name: &'static str,
    params: Vec<ParamInfo>,
    min_params: usize,
    variadic: bool,
    return_type: JsonTypeInfo,
    function: Box<dyn Fn(&[Value], MetaParameters) -> Value + Send + Sync>,
}

#[allow(dead_code)]
struct ParamInfo {
    name: &'static str,
    r#type: JsonTypeInfo,
}

#[allow(dead_code)]
enum JsonTypeInfo {
    Any,
    Null,
    Bool,
    Number,
    String,
    Array(Arc<JsonTypeInfo>),
    Object(HashMap<String, JsonTypeInfo>),
    OneOf(Vec<JsonTypeInfo>),
}

static FN_INFO_BY_NAME: LazyLock<HashMap<String, Vec<FnInfo>>> = LazyLock::new(|| {
    let mut infos_by_name = HashMap::new();
    for info in [
        source_infos(),
        numbers::infos(),
        common::infos(),
        boolean::infos(),
        date::infos(),
        ml::infos(),
        array::infos(),
        strings::infos(),
    ]
    .into_iter()
    .flatten()
    {
        infos_by_name
            .entry(normalized_function_name(info.name))
            .or_insert_with(Vec::new)
            .push(info);
    }
    infos_by_name
});

fn fn_info_for_call(function: &str, values: &[Value]) -> Option<&'static FnInfo> {
    FN_INFO_BY_NAME
        .get(&normalized_function_name(function))
        .and_then(|infos| {
            infos.iter().find(|info| {
                values.len() >= info.min_params
                    && (info.variadic || values.len() <= info.params.len())
            })
        })
}

fn source_infos() -> Vec<FnInfo> {
    vec![
        with_name(open_info(), "GET"),
        get_info(),
        open_info(),
        post_info(),
        put_info(),
    ]
}

fn with_name(mut info: FnInfo, name: &'static str) -> FnInfo {
    info.name = name;
    info
}

fn normalized_function_name(function: &str) -> String {
    function
        .chars()
        .filter(|character| *character != '_')
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs, io,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct MemoryFileProvider {
        path: PathBuf,
        contents: String,
    }

    impl FileProvider for MemoryFileProvider {
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            if path == self.path {
                Ok(self.contents.clone())
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "missing file"))
            }
        }

        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }
    }

    #[test]
    fn stream_query_jsonl_accepts_provider_backed_query_file() {
        let query_path = PathBuf::from("memory-query.ql");
        let provider = MemoryFileProvider {
            path: query_path.clone(),
            contents: "SOURCE [{id: 1}, {id: 2}]\nMAP id".to_string(),
        };
        let mut output = Vec::new();

        stream_query_jsonl_with_provider(&query_path, &mut output, 100, &provider).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(r#""type":"meta""#));
        assert!(output.contains(r#""type":"batch""#));
        assert!(output.contains(r#""rowCount":2"#));
    }

    #[test]
    fn source_functions_load_with_meta_parameters() {
        let temp_dir = std::env::temp_dir().join(format!(
            "quickql-core-source-functions-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&temp_dir).unwrap();
        fs::write(temp_dir.join("rows.json"), r#"[{"id":1}]"#).unwrap();
        let query_path = temp_dir.join("query.ql");
        let mut ql_stack = Vec::new();

        for function in ["GET", "OPEN", "POST", "PUT"] {
            let value = CaluculatedValue::FunctionCall {
                function: function.to_string(),
                parameters: vec![CaluculatedValue::Static(Value::String(
                    "rows.json".to_string(),
                ))],
            }
            .caluculate(&Value::Null, &query_path, &mut ql_stack, &FsFileProvider);

            assert_eq!(value, serde_json::json!([{ "id": 1 }]));
        }

        fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn open_accepts_source_config_object() {
        let temp_dir = std::env::temp_dir().join(format!(
            "quickql-core-source-config-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&temp_dir).unwrap();
        fs::write(temp_dir.join("rows.json"), r#"[{"id":2}]"#).unwrap();
        let query_path = temp_dir.join("query.ql");

        let value = CaluculatedValue::FunctionCall {
            function: "OPEN".to_string(),
            parameters: vec![CaluculatedValue::Static(serde_json::json!({
                "src": "rows.json"
            }))],
        }
        .caluculate(&Value::Null, &query_path, &mut Vec::new(), &FsFileProvider);

        assert_eq!(value, serde_json::json!([{ "id": 2 }]));

        fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn multi_argument_get_still_reads_fields() {
        let value = CaluculatedValue::FunctionCall {
            function: "GET".to_string(),
            parameters: vec![
                CaluculatedValue::Static(serde_json::json!({ "nested": { "id": 1 } })),
                CaluculatedValue::Static(Value::String("nested".to_string())),
                CaluculatedValue::Static(Value::String("id".to_string())),
            ],
        }
        .caluculate(
            &Value::Null,
            Path::new("query.ql"),
            &mut Vec::new(),
            &FsFileProvider,
        );

        assert_eq!(value, serde_json::json!(1));
    }
}

pub struct QueryResult {
    pub columns: KeyDescriptor,
    pub rows: Vec<Value>,
}

impl QueryResult {
    pub fn new(rows: Vec<Value>) -> Self {
        Self {
            columns: KeyDescriptor::from_values(&rows),
            rows,
        }
    }
}

impl Default for QueryResult {
    fn default() -> Self {
        Self {
            columns: KeyDescriptor::Value,
            rows: vec![],
        }
    }
}

pub enum KeyDescriptor {
    Value,
    Object(HashMap<String, KeyDescriptor>),
}

impl KeyDescriptor {
    fn from_values(values: &[Value]) -> Self {
        let mut fields = HashMap::new();

        for value in values {
            Self::merge_value(&mut fields, value);
        }

        if fields.is_empty() {
            Self::Value
        } else {
            Self::Object(fields)
        }
    }

    fn from_value(value: &Value) -> Self {
        match value {
            Value::Object(map) => {
                let mut fields = HashMap::new();
                for (key, value) in map {
                    fields.insert(key.clone(), Self::from_value(value));
                }
                Self::Object(fields)
            }
            _ => Self::Value,
        }
    }

    fn merge_value(fields: &mut HashMap<String, KeyDescriptor>, value: &Value) {
        let Value::Object(map) = value else {
            return;
        };

        for (key, value) in map {
            match fields.get_mut(key) {
                Some(existing) => existing.merge(value),
                None => {
                    fields.insert(key.clone(), Self::from_value(value));
                }
            }
        }
    }

    fn merge(&mut self, value: &Value) {
        match (self, value) {
            (Self::Object(fields), Value::Object(_)) => Self::merge_value(fields, value),
            (descriptor, _) => *descriptor = Self::Value,
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
    Progress {
        substep: usize,
        #[serde(rename = "totalSubsteps")]
        total_substeps: usize,
        #[serde(rename = "substepName")]
        substep_name: &'a str,
        percent: f64,
        #[serde(rename = "elapsedMs")]
        elapsed_ms: f64,
        #[serde(rename = "remainingMs")]
        remaining_ms: Option<f64>,
    },
    Batch {
        start: usize,
        rows: &'a [Value],
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
pub(crate) const ALL_COLUMNS: &str = "*";
