use std::io;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use quickql_core::{secret_var, FileProvider};
use reqwest::blocking::Client;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Parser)]
#[command(name = "ql-webservice")]
#[command(about = "QuickQL web UI and query API")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:3030")]
    listen: SocketAddr,

    #[arg(long)]
    cwd: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    cwd: Arc<PathBuf>,
    gitlab: Option<Arc<GitLabConfig>>,
}

#[derive(Deserialize)]
struct QueryRequest {
    query: String,
}

#[derive(Deserialize)]
struct GitQueryRequest {
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    columns: Vec<String>,
    row_count: usize,
    elapsed_ms: f64,
    source: String,
    rows: Vec<Value>,
}

#[derive(Clone)]
struct GitLabConfig {
    api_base: String,
    project: String,
    ref_name: String,
    folder: String,
    token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitFile {
    path: String,
    name: String,
}

#[derive(Deserialize)]
struct GitLabTreeItem {
    path: String,
    #[serde(rename = "type")]
    item_type: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum StreamMessage {
    Meta {
        columns: Vec<String>,
        source: String,
    },
    Progress {
        #[allow(dead_code)]
        substep: usize,
    },
    Batch {
        #[allow(dead_code)]
        start: usize,
        rows: Vec<Value>,
    },
    Done {
        #[serde(rename = "rowCount")]
        row_count: usize,
        #[serde(rename = "elapsedMs")]
        elapsed_ms: f64,
    },
}

struct WebQueryFileProvider {
    query_path: PathBuf,
    query: String,
}

#[derive(Clone)]
struct GitLabFileProvider {
    config: Arc<GitLabConfig>,
    client: Client,
}

impl FileProvider for GitLabFileProvider {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.fetch_file(path)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.fetch_file(path)
            .map(String::into_bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(normalize_provider_path(path))
    }
}

impl GitLabFileProvider {
    fn fetch_file(&self, path: &Path) -> Result<String> {
        let path = normalize_provider_path(path);
        let file_path = path
            .to_str()
            .context("GitLab file path is not valid UTF-8")?
            .replace('\\', "/");
        let url = format!(
            "{}/projects/{}/repository/files/{}/raw",
            self.config.api_base,
            url_encode_path_segment(&self.config.project),
            url_encode_path_segment(&file_path)
        );
        let response = self
            .client
            .get(url)
            .header("PRIVATE-TOKEN", &self.config.token)
            .query(&[("ref", self.config.ref_name.as_str())])
            .send()
            .with_context(|| format!("Fetching GitLab file {file_path}"))?
            .error_for_status()
            .with_context(|| format!("Fetching GitLab file {file_path}"))?;
        response
            .text()
            .with_context(|| format!("Reading GitLab file {file_path}"))
    }
}

impl FileProvider for WebQueryFileProvider {
    fn read_to_string(&self, path: &std::path::Path) -> io::Result<String> {
        if path == self.query_path {
            return Ok(self.query.clone());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("file not provided: {}", path.display()),
        ))
    }

    fn read_bytes(&self, path: &std::path::Path) -> io::Result<Vec<u8>> {
        if path == self.query_path {
            return Ok(self.query.clone().into_bytes());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("file not provided: {}", path.display()),
        ))
    }

    fn canonicalize(&self, path: &std::path::Path) -> io::Result<PathBuf> {
        if path == self.query_path {
            return Ok(self.query_path.clone());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("file not provided: {}", path.display()),
        ))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    for (key, value) in std::env::vars() {
        println!("env: {key}");
    }

    let cli = Cli::parse();
    let cwd = cli
        .cwd
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)?
        .canonicalize()
        .context("Resolving query working directory")?;

    let gitlab = load_gitlab_config().transpose()?.map(Arc::new);
    let state = AppState {
        cwd: Arc::new(cwd),
        gitlab,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/query", post(run_query))
        .route("/api/git/files", get(git_files))
        .route("/api/git/query", post(run_git_query))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cli.listen).await?;
    println!("QuickQL webservice listening on http://{}", cli.listen);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn run_query(State(state): State<AppState>, Json(request): Json<QueryRequest>) -> Response {
    match execute_query(&state.cwd, request.query).await {
        Ok(output) => Json(output).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn git_files(State(state): State<AppState>) -> Response {
    let Some(config) = state.gitlab.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "GitLab is not configured. Set QL_WEBSERVICE_GITLAB_PATH and QL_WEBSERVICE_GITLAB_TOKEN.".to_string(),
            }),
        )
            .into_response();
    };

    match tokio::task::spawn_blocking(move || list_gitlab_ql_files(&config)).await {
        Ok(Ok(files)) => Json(files).into_response(),
        Ok(Err(error)) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn run_git_query(
    State(state): State<AppState>,
    Json(request): Json<GitQueryRequest>,
) -> Response {
    let Some(config) = state.gitlab.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "GitLab is not configured. Set QL_WEBSERVICE_GITLAB_PATH and QL_WEBSERVICE_GITLAB_TOKEN.".to_string(),
            }),
        )
            .into_response();
    };

    match execute_git_query(config, request.path).await {
        Ok(output) => Json(output).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn execute_query(cwd: &PathBuf, query: String) -> Result<QueryResponse> {
    let cwd = cwd.clone();
    tokio::task::spawn_blocking(move || {
        let query_path = cwd.join("__quickql_web_query.ql");
        let file_provider = WebQueryFileProvider {
            query_path: query_path.clone(),
            query,
        };
        let mut stream = Vec::new();
        quickql_core::stream_query_jsonl_with_provider(
            &query_path,
            &mut stream,
            200,
            &file_provider,
        )?;
        query_response_from_stream(&stream)
    })
    .await?
}

fn query_response_from_stream(stream: &[u8]) -> Result<QueryResponse> {
    let mut columns = Vec::new();
    let mut row_count = 0usize;
    let mut elapsed_ms = 0.0;
    let mut rows = Vec::new();

    for line in String::from_utf8_lossy(stream).lines() {
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<StreamMessage>(line)? {
            StreamMessage::Meta {
                columns: value,
                source,
            } => {
                let _ = source;
                columns = value;
            }
            StreamMessage::Progress { .. } => {}
            StreamMessage::Batch { rows: value, .. } => {
                rows.extend(value);
            }
            StreamMessage::Done {
                row_count: value,
                elapsed_ms: value_elapsed_ms,
            } => {
                row_count = value;
                elapsed_ms = value_elapsed_ms;
            }
        }
    }

    if row_count == 0 {
        row_count = rows.len();
    }

    Ok(QueryResponse {
        columns,
        row_count,
        elapsed_ms,
        source: "QuickQL web query".to_string(),
        rows,
    })
}

async fn execute_git_query(config: Arc<GitLabConfig>, path: String) -> Result<QueryResponse> {
    tokio::task::spawn_blocking(move || {
        if !path.ends_with(".ql") {
            bail!("GitLab query path must point to a .ql file");
        }
        let provider = GitLabFileProvider {
            config: config.clone(),
            client: Client::new(),
        };
        let query_path = normalize_provider_path(Path::new(&path));
        let mut stream = Vec::new();
        quickql_core::stream_query_jsonl_with_provider(&query_path, &mut stream, 200, &provider)?;
        let mut response = query_response_from_stream(&stream)?;
        response.source = path;
        Ok(response)
    })
    .await?
}

fn load_gitlab_config() -> Option<Result<GitLabConfig>> {
    let path = secret_var("QL_WEBSERVICE_GITLAB_PATH").or_else(|| secret_var("GITLAB_PATH"));
    println!("GitLab path: {:?}", path);
    let token = secret_var("QL_WEBSERVICE_GITLAB_TOKEN").or_else(|| secret_var("GITLAB_TOKEN"));

    match (path, token) {
        (None, None) => None,
        (Some(path), Some(token)) => Some(parse_gitlab_config(&path, token)),
        _ => Some(Err(anyhow::anyhow!(
            "GitLab requires both QL_WEBSERVICE_GITLAB_PATH and QL_WEBSERVICE_GITLAB_TOKEN"
        ))),
    }
}

fn parse_gitlab_config(path: &str, token: String) -> Result<GitLabConfig> {
    let url = Url::parse(path).context("Parsing GitLab folder URL")?;
    let segments = url
        .path_segments()
        .context("GitLab URL must contain path segments")?
        .collect::<Vec<_>>();
    let blob_index = segments
        .iter()
        .position(|segment| *segment == "blob" || *segment == "tree")
        .context("GitLab URL must contain /blob/<ref>/ or /tree/<ref>/")?;
    if blob_index == 0 || blob_index + 1 >= segments.len() {
        bail!("GitLab URL must include a project path, ref, and folder path");
    }

    let project = segments[..blob_index].join("/");
    let ref_name = segments[blob_index + 1].to_string();
    let folder = segments.get(blob_index + 2..).unwrap_or_default().join("/");
    let api_base = format!(
        "{}://{}/api/v4",
        url.scheme(),
        url.host_str().context("GitLab URL must include a host")?
    );

    Ok(GitLabConfig {
        api_base,
        project,
        ref_name,
        folder,
        token,
    })
}

fn list_gitlab_ql_files(config: &GitLabConfig) -> Result<Vec<GitFile>> {
    let client = Client::new();
    let mut files = Vec::new();
    let mut page = 1usize;

    loop {
        let url = format!(
            "{}/projects/{}/repository/tree",
            config.api_base,
            url_encode_path_segment(&config.project)
        );
        let response = client
            .get(url)
            .header("PRIVATE-TOKEN", &config.token)
            .query(&[
                ("ref", config.ref_name.as_str()),
                ("path", config.folder.as_str()),
                ("recursive", "true"),
                ("per_page", "100"),
                ("page", &page.to_string()),
            ])
            .send()
            .context("Listing GitLab repository tree")?
            .error_for_status()
            .context("Listing GitLab repository tree")?;
        let next_page = next_page(&response.headers());
        let items = response
            .json::<Vec<GitLabTreeItem>>()
            .context("Parsing GitLab repository tree")?;
        for item in items {
            if item.item_type == "blob" && item.path.ends_with(".ql") {
                let name = item
                    .path
                    .strip_prefix(config.folder.trim_end_matches('/'))
                    .unwrap_or(&item.path)
                    .trim_start_matches('/')
                    .to_string();
                files.push(GitFile {
                    path: item.path,
                    name,
                });
            }
        }

        let Some(next) = next_page else {
            break;
        };
        page = next;
    }

    files.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(files)
}

fn next_page(headers: &HeaderMap) -> Option<usize> {
    headers
        .get("x-next-page")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<usize>().ok())
}

fn normalize_provider_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized
}

fn url_encode_path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>QuickQL Webservice</title>
  <style>
    :root {
      color-scheme: light dark;
      --bg: #101214;
      --fg: #e9edf0;
      --muted: #96a0aa;
      --panel: #171a1d;
      --header: #20252a;
      --border: #343a40;
      --accent: #3d8bfd;
      --accent-hover: #2f73d0;
      --danger: #f07878;
      --row-hover: rgba(61, 139, 253, 0.13);
      font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    @media (prefers-color-scheme: light) {
      :root {
        --bg: #f6f8fa;
        --fg: #1f2328;
        --muted: #656d76;
        --panel: #ffffff;
        --header: #eef1f4;
        --border: #d0d7de;
        --row-hover: rgba(9, 105, 218, 0.08);
      }
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      color: var(--fg);
      background: var(--bg);
      overflow: hidden;
    }
    button {
      border: 1px solid transparent;
      border-radius: 4px;
      padding: 7px 13px;
      color: white;
      background: var(--accent);
      font: inherit;
      cursor: pointer;
      white-space: nowrap;
    }
    button:hover { background: var(--accent-hover); }
    button:disabled { cursor: default; opacity: 0.62; }
    .app {
      height: 100vh;
      display: grid;
      grid-template-rows: 42px minmax(0, 1fr);
    }
    .tabs {
      display: flex;
      align-items: stretch;
      gap: 0;
      border-bottom: 1px solid var(--border);
      background: var(--panel);
    }
    .tab {
      border: 0;
      border-radius: 0;
      color: var(--muted);
      background: transparent;
      padding: 0 18px;
    }
    .tab.active {
      color: #ffffff;
      background: var(--accent);
    }
    .pane {
      display: none;
      min-height: 0;
    }
    .pane.active {
      display: grid;
    }
    .lab-pane {
      grid-template-rows: 188px minmax(0, 1fr);
    }
    .git-pane {
      grid-template-columns: 320px minmax(0, 1fr);
    }
    .query {
      display: grid;
      grid-template-rows: 1fr 40px;
      gap: 10px;
      padding: 12px;
      border-bottom: 1px solid var(--border);
      background: var(--panel);
      min-height: 0;
    }
    textarea {
      width: 100%;
      min-height: 0;
      resize: none;
      border: 1px solid var(--border);
      border-radius: 4px;
      padding: 10px;
      color: var(--fg);
      background: var(--bg);
      font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      outline: none;
    }
    textarea:focus { border-color: var(--accent); }
    .actions, .sidebar-head {
      display: flex;
      align-items: center;
      gap: 10px;
      min-width: 0;
    }
    .status {
      color: var(--muted);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .status.error { color: var(--danger); }
    .git-sidebar {
      display: grid;
      grid-template-rows: 41px minmax(0, 1fr);
      border-right: 1px solid var(--border);
      background: var(--panel);
      min-width: 0;
      min-height: 0;
    }
    .sidebar-head {
      padding: 0 10px;
      border-bottom: 1px solid var(--border);
    }
    .file-list {
      overflow: auto;
      min-height: 0;
      padding: 6px;
    }
    .file-item {
      width: 100%;
      display: block;
      border: 0;
      border-radius: 4px;
      padding: 7px 8px;
      color: var(--fg);
      background: transparent;
      text-align: left;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .file-item:hover { background: var(--row-hover); }
    .file-item.active {
      color: #ffffff;
      background: var(--accent);
    }
    .result-host {
      min-height: 0;
      min-width: 0;
    }
    .results {
      height: 100%;
      min-height: 0;
      display: grid;
      grid-template-rows: 34px minmax(0, 1fr);
    }
    .toolbar {
      height: 34px;
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 0 10px;
      border-bottom: 1px solid var(--border);
      color: var(--muted);
      background: var(--panel);
      white-space: nowrap;
      min-width: 0;
    }
    .toolbar strong {
      color: var(--fg);
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .toolbar-spacer { flex: 1 1 auto; }
    .view-switch {
      display: inline-flex;
      border: 1px solid var(--border);
      border-radius: 4px;
      overflow: hidden;
      flex: 0 0 auto;
    }
    .view-switch button {
      border: 0;
      border-radius: 0;
      color: var(--fg);
      background: transparent;
      padding: 5px 10px;
    }
    .view-switch button.active {
      color: #ffffff;
      background: var(--accent);
    }
    .table {
      position: relative;
      grid-row: 2;
      grid-column: 1;
      overflow: auto;
      min-height: 0;
      background: var(--bg);
    }
    .spacer {
      position: relative;
      width: max-content;
      min-width: 100%;
    }
    .header, .row {
      display: grid;
      grid-template-columns: repeat(var(--column-count, 1), minmax(140px, 1fr));
      min-width: 100%;
    }
    .header {
      position: sticky;
      top: 0;
      z-index: 2;
      background: var(--header);
      border-bottom: 1px solid var(--border);
      font-weight: 600;
    }
    .cell {
      min-height: 28px;
      padding: 6px 10px;
      border-right: 1px solid var(--border);
      border-bottom: 1px solid var(--border);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 13px;
    }
    .viewport {
      position: absolute;
      left: 0;
      right: 0;
      top: 28px;
    }
    .row { height: 28px; }
    .row:hover { background: var(--row-hover); }
    .table.hidden { display: none; }
    .graph {
      display: none;
      grid-row: 2;
      grid-column: 1;
      overflow: auto;
      min-height: 0;
      position: relative;
      background: var(--bg);
    }
    .graph.active { display: block; }
    #graphPlot {
      display: flex;
      flex-direction: column;
      align-items: stretch;
      gap: 32px;
      min-height: 100%;
      padding: 8px;
      width: 100%;
    }
    .graph-item {
      flex: 0 0 auto;
      height: min(760px, calc(100vh - 140px));
      min-height: 420px;
      border: 1px solid var(--border);
      overflow: hidden;
      background: var(--panel);
    }
    .graph-item-plot {
      width: 100%;
      height: 100%;
      min-height: 0;
    }
    .graph-message {
      position: absolute;
      inset: 0;
      display: none;
      align-items: center;
      justify-content: center;
      padding: 16px;
      color: var(--muted);
      text-align: center;
    }
    .graph-message.active { display: flex; }
    .empty {
      padding: 18px;
      color: var(--muted);
    }
  </style>
</head>
<body>
  <div class="app">
    <nav class="tabs" aria-label="QuickQL modes">
      <button id="labTab" class="tab active" type="button" aria-pressed="true">Lab</button>
      <button id="gitTab" class="tab" type="button" aria-pressed="false">Git</button>
    </nav>

    <section id="labPane" class="pane lab-pane active">
      <form id="queryForm" class="query">
        <textarea id="queryInput" spellcheck="false" autofocus>SOURCE [{id: 1, name: 'Alice'}, {id: 2, name: 'Bob'}]
MAP id, name</textarea>
        <div class="actions">
          <button id="runButton" type="submit">Run Query</button>
          <span id="labStatus" class="status">Ready</span>
        </div>
      </form>
      <div id="labResultHost" class="result-host"></div>
    </section>

    <section id="gitPane" class="pane git-pane">
      <aside class="git-sidebar">
        <div class="sidebar-head">
          <button id="refreshGit" type="button">Refresh</button>
          <span id="gitStatus" class="status">Open Git to load files.</span>
        </div>
        <div id="fileList" class="file-list"></div>
      </aside>
      <div id="gitResultHost" class="result-host"></div>
    </section>
  </div>

  <div id="resultView" class="results">
    <div class="toolbar">
      <strong id="source">No results</strong>
      <span id="rowCount">0 rows</span>
      <span id="elapsed">0.0 ms</span>
      <span class="toolbar-spacer"></span>
      <div class="view-switch" role="group" aria-label="Result view">
        <button id="tableView" class="active" type="button" aria-pressed="true">Table</button>
        <button id="graphView" type="button" aria-pressed="false">Graph</button>
      </div>
    </div>
    <div id="table" class="table">
      <div id="empty" class="empty">Run a query or select a GitLab .ql file to show results.</div>
      <div id="spacer" class="spacer" hidden>
        <div id="header" class="header"></div>
        <div id="viewport" class="viewport"></div>
      </div>
    </div>
    <div id="graph" class="graph">
      <div id="graphPlot"></div>
      <div id="graphMessage" class="graph-message"></div>
    </div>
  </div>

  <script src="https://cdn.plot.ly/plotly-2.35.2.min.js" onerror="window.__plotlyLoadFailed = true"></script>
  <script>
    const labTab = document.getElementById('labTab');
    const gitTab = document.getElementById('gitTab');
    const labPane = document.getElementById('labPane');
    const gitPane = document.getElementById('gitPane');
    const labResultHost = document.getElementById('labResultHost');
    const gitResultHost = document.getElementById('gitResultHost');
    const resultView = document.getElementById('resultView');
    const form = document.getElementById('queryForm');
    const input = document.getElementById('queryInput');
    const button = document.getElementById('runButton');
    const labStatus = document.getElementById('labStatus');
    const gitStatus = document.getElementById('gitStatus');
    const refreshGit = document.getElementById('refreshGit');
    const fileList = document.getElementById('fileList');
    const sourceEl = document.getElementById('source');
    const rowCountEl = document.getElementById('rowCount');
    const elapsedEl = document.getElementById('elapsed');
    const table = document.getElementById('table');
    const empty = document.getElementById('empty');
    const spacer = document.getElementById('spacer');
    const header = document.getElementById('header');
    const viewport = document.getElementById('viewport');
    const graph = document.getElementById('graph');
    const graphPlot = document.getElementById('graphPlot');
    const graphMessage = document.getElementById('graphMessage');
    const tableView = document.getElementById('tableView');
    const graphView = document.getElementById('graphView');
    const rowHeight = 28;
    let rows = [];
    let columns = [];
    let displayColumns = ['row'];
    let hasMetaColumns = false;
    let activeView = 'table';
    let activeMode = 'lab';
    let graphRendered = false;
    let gitFilesLoaded = false;
    let selectedGitPath = '';

    labResultHost.appendChild(resultView);
    labTab.addEventListener('click', () => setMode('lab'));
    gitTab.addEventListener('click', () => setMode('git'));
    refreshGit.addEventListener('click', loadGitFiles);
    form.addEventListener('submit', async event => {
      event.preventDefault();
      await runLabQuery();
    });
    table.addEventListener('scroll', render);
    tableView.addEventListener('click', () => setView('table'));
    graphView.addEventListener('click', () => setView('graph'));

    async function setMode(mode) {
      activeMode = mode;
      const isGit = mode === 'git';
      labPane.classList.toggle('active', !isGit);
      gitPane.classList.toggle('active', isGit);
      labTab.classList.toggle('active', !isGit);
      gitTab.classList.toggle('active', isGit);
      labTab.setAttribute('aria-pressed', String(!isGit));
      gitTab.setAttribute('aria-pressed', String(isGit));
      (isGit ? gitResultHost : labResultHost).appendChild(resultView);
      if (isGit && !gitFilesLoaded) {
        await loadGitFiles();
      }
      if (activeView === 'table') {
        render();
      } else if (window.Plotly) {
        resizeGraphs();
      }
    }

    async function runLabQuery() {
      await runResultRequest(
        '/api/query',
        { query: input.value },
        labStatus,
        button,
        'Running...',
        'Done'
      );
    }

    async function loadGitFiles() {
      setStatus(gitStatus, 'Loading files...', false);
      refreshGit.disabled = true;
      try {
        const response = await fetch('/api/git/files');
        const body = await response.json();
        if (!response.ok) {
          throw new Error(body.error || 'Unable to load GitLab files');
        }
        renderGitFiles(body);
        gitFilesLoaded = true;
        setStatus(gitStatus, body.length.toLocaleString() + ' files', false);
      } catch (error) {
        setStatus(gitStatus, error && error.message ? error.message : String(error), true);
      } finally {
        refreshGit.disabled = false;
      }
    }

    function renderGitFiles(files) {
      fileList.innerHTML = (files || []).map(file =>
        '<button class="file-item" type="button" data-path="' + escapeAttr(file.path) + '" title="' + escapeAttr(file.path) + '">' +
          escapeHtml(file.name || file.path) +
        '</button>'
      ).join('');
      fileList.querySelectorAll('.file-item').forEach(item => {
        item.addEventListener('click', () => runGitFile(item.dataset.path, item));
      });
    }

    async function runGitFile(path, item) {
      if (!path) return;
      selectedGitPath = path;
      fileList.querySelectorAll('.file-item').forEach(entry => entry.classList.toggle('active', entry === item));
      await runResultRequest(
        '/api/git/query',
        { path },
        gitStatus,
        null,
        'Running ' + path,
        'Done'
      );
    }

    async function runResultRequest(url, payload, statusTarget, disabledButton, runningText, doneText) {
      setStatus(statusTarget, runningText, false);
      if (disabledButton) disabledButton.disabled = true;
      try {
        const response = await fetch(url, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(payload)
        });
        const body = await response.json();
        if (!response.ok) {
          throw new Error(body.error || 'Query failed');
        }
        setResult(body);
        setStatus(statusTarget, doneText, false);
      } catch (error) {
        setStatus(statusTarget, error && error.message ? error.message : String(error), true);
      } finally {
        if (disabledButton) disabledButton.disabled = false;
      }
    }

    function setResult(result) {
      rows = Array.isArray(result.rows) ? result.rows : [];
      columns = Array.isArray(result.columns) ? result.columns : [];
      hasMetaColumns = columns.length > 0;
      displayColumns = hasMetaColumns ? columns : ['row'];
      graphRendered = false;
      graphPlot.innerHTML = '';
      hideGraphMessage();

      sourceEl.textContent = result.source || selectedGitPath || 'Query';
      sourceEl.title = sourceEl.textContent;
      rowCountEl.textContent = (result.rowCount || rows.length).toLocaleString() + ' rows';
      elapsedEl.textContent = Number(result.elapsedMs || 0).toFixed(1) + ' ms';

      document.documentElement.style.setProperty('--column-count', String(Math.max(displayColumns.length, 1)));
      header.innerHTML = displayColumns.map(column => '<div class="cell">' + escapeHtml(column) + '</div>').join('');
      spacer.style.height = ((rows.length * rowHeight) + rowHeight) + 'px';
      spacer.hidden = false;
      empty.hidden = rows.length > 0;
      table.scrollTop = 0;
      if (activeView === 'graph') {
        renderGraph(rows);
      } else {
        render();
      }
    }

    function render() {
      if (spacer.hidden) return;
      const firstVisible = Math.max(0, Math.floor((table.scrollTop - rowHeight) / rowHeight));
      const visibleCount = Math.ceil(table.clientHeight / rowHeight) + 8;
      const visibleRows = [];
      for (let index = firstVisible; index < Math.min(rows.length, firstVisible + visibleCount); index++) {
        visibleRows.push(rows[index]);
      }

      viewport.style.transform = 'translateY(' + (firstVisible * rowHeight) + 'px)';
      viewport.innerHTML = visibleRows.map(row =>
        '<div class="row">' + displayColumns.map(column => {
          const value = hasMetaColumns ? row && row[column] : row;
          const formatted = format(value);
          return '<div class="cell" title="' + escapeAttr(formatted) + '">' + escapeHtml(formatted) + '</div>';
        }).join('') + '</div>'
      ).join('');
    }

    function setView(view) {
      activeView = view;
      const isGraph = view === 'graph';
      table.classList.toggle('hidden', isGraph);
      graph.classList.toggle('active', isGraph);
      tableView.classList.toggle('active', !isGraph);
      graphView.classList.toggle('active', isGraph);
      tableView.setAttribute('aria-pressed', String(!isGraph));
      graphView.setAttribute('aria-pressed', String(isGraph));

      if (isGraph) {
        if (!graphRendered) {
          renderGraph(rows);
        } else if (window.Plotly) {
          resizeGraphs();
        }
      } else {
        render();
      }
    }

    function renderGraph(resultRows) {
      if (activeView !== 'graph') return;
      if (!Array.isArray(resultRows) || resultRows.length === 0) {
        showGraphMessage('No result rows available for graph view.');
        return;
      }

      ensurePlotly(() => {
        const specs = resultRows
          .map(unwrapPlotSpec)
          .filter(spec => spec && spec.data !== undefined);
        if (specs.length === 0) {
          showGraphMessage('At least one result row must contain a data field for Plotly.');
          return;
        }

        try {
          hideGraphMessage();
          graphPlot.innerHTML = specs.map((_, index) =>
            '<div class="graph-item"><div id="graphItem' + index + '" class="graph-item-plot"></div></div>'
          ).join('');

          specs.forEach((spec, index) => {
            const item = document.getElementById('graphItem' + index);
            const config = Object.assign({ responsive: true, displaylogo: false }, spec.config || {});
            Plotly.newPlot(item, normalizePlotData(spec.data), spec.layout || {}, config);
          });
          graphRendered = true;
        } catch (error) {
          graphRendered = false;
          showGraphMessage('Unable to render graph: ' + (error && error.message ? error.message : String(error)));
        }
      });
    }

    function ensurePlotly(callback) {
      if (window.Plotly) {
        callback();
        return;
      }
      showGraphMessage(window.__plotlyLoadFailed ? 'Unable to load Plotly.' : 'Loading graph renderer...');
    }

    function unwrapPlotSpec(row) {
      if (row && row.data !== undefined) return row;
      const values = Object.values(row || {});
      if (values.length === 1 && values[0] && typeof values[0] === 'object' && values[0].data !== undefined) {
        return values[0];
      }
      return row;
    }

    function normalizePlotData(data) {
      return Array.isArray(data) ? data : [data];
    }

    function resizeGraphs() {
      graphPlot.querySelectorAll('.graph-item-plot').forEach(plot => {
        Plotly.Plots.resize(plot);
      });
    }

    function showGraphMessage(message) {
      graphMessage.textContent = message;
      graphMessage.classList.add('active');
      graphPlot.style.visibility = 'hidden';
    }

    function hideGraphMessage() {
      graphMessage.textContent = '';
      graphMessage.classList.remove('active');
      graphPlot.style.visibility = 'visible';
    }

    function setStatus(target, message, isError) {
      target.textContent = message;
      target.title = message;
      target.classList.toggle('error', Boolean(isError));
    }

    function format(value) {
      if (value === null || value === undefined) return '';
      if (typeof value === 'object') return JSON.stringify(value);
      return String(value);
    }

    function escapeHtml(value) {
      return String(value).replace(/[&<>"']/g, ch => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch]));
    }

    function escapeAttr(value) {
      return escapeHtml(value);
    }
  </script>
</body>
</html>
"#;
