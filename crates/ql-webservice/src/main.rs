use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::NamedTempFile;

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
}

#[derive(Deserialize)]
struct QueryRequest {
    query: String,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = cli
        .cwd
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)?
        .canonicalize()
        .context("Resolving query working directory")?;

    let state = AppState { cwd: Arc::new(cwd) };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/query", post(run_query))
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

async fn execute_query(cwd: &PathBuf, query: String) -> Result<QueryResponse> {
    let cwd = cwd.clone();
    tokio::task::spawn_blocking(move || {
        let mut file = NamedTempFile::with_suffix_in(".ql", &cwd)
            .with_context(|| format!("Creating temporary query file in {}", cwd.display()))?;
        std::io::Write::write_all(&mut file, query.as_bytes())?;
        std::io::Write::flush(&mut file)?;
        let mut stream = Vec::new();
        quickql_core::stream_query_jsonl(file.path(), &mut stream, 200)?;
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
    .app {
      height: 100vh;
      display: grid;
      grid-template-rows: 188px 34px minmax(0, 1fr);
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
    textarea:focus {
      border-color: var(--accent);
    }
    .actions {
      display: flex;
      align-items: center;
      gap: 10px;
      min-width: 0;
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
    button:disabled {
      cursor: default;
      opacity: 0.62;
    }
    .status {
      color: var(--muted);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .status.error { color: var(--danger); }
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
    .table {
      position: relative;
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
    .empty {
      padding: 18px;
      color: var(--muted);
    }
  </style>
</head>
<body>
  <div class="app">
    <form id="queryForm" class="query">
      <textarea id="queryInput" spellcheck="false" autofocus>SOURCE [{id: 1, name: 'Alice'}, {id: 2, name: 'Bob'}]
MAP id, name</textarea>
      <div class="actions">
        <button id="runButton" type="submit">Run Query</button>
        <span id="status" class="status">Ready</span>
      </div>
    </form>
    <div class="toolbar">
      <strong id="source">No results</strong>
      <span id="rowCount">0 rows</span>
      <span id="elapsed">0.0 ms</span>
    </div>
    <div id="table" class="table">
      <div id="empty" class="empty">Run a query to show results.</div>
      <div id="spacer" class="spacer" hidden>
        <div id="header" class="header"></div>
        <div id="viewport" class="viewport"></div>
      </div>
    </div>
  </div>
  <script>
    const form = document.getElementById('queryForm');
    const input = document.getElementById('queryInput');
    const button = document.getElementById('runButton');
    const statusEl = document.getElementById('status');
    const sourceEl = document.getElementById('source');
    const rowCountEl = document.getElementById('rowCount');
    const elapsedEl = document.getElementById('elapsed');
    const table = document.getElementById('table');
    const empty = document.getElementById('empty');
    const spacer = document.getElementById('spacer');
    const header = document.getElementById('header');
    const viewport = document.getElementById('viewport');
    const rowHeight = 28;
    let rows = [];
    let columns = [];
    let displayColumns = ['row'];
    let hasMetaColumns = false;

    form.addEventListener('submit', async event => {
      event.preventDefault();
      await runQuery();
    });
    table.addEventListener('scroll', render);

    async function runQuery() {
      setStatus('Running...', false);
      button.disabled = true;
      try {
        const response = await fetch('/api/query', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ query: input.value })
        });
        const body = await response.json();
        if (!response.ok) {
          throw new Error(body.error || 'Query failed');
        }
        setResult(body);
        setStatus('Done', false);
      } catch (error) {
        setStatus(error && error.message ? error.message : String(error), true);
      } finally {
        button.disabled = false;
      }
    }

    function setResult(result) {
      rows = Array.isArray(result.rows) ? result.rows : [];
      columns = Array.isArray(result.columns) ? result.columns : [];
      hasMetaColumns = columns.length > 0;
      displayColumns = hasMetaColumns ? columns : ['row'];

      sourceEl.textContent = result.source || 'Query';
      sourceEl.title = sourceEl.textContent;
      rowCountEl.textContent = (result.rowCount || rows.length).toLocaleString() + ' rows';
      elapsedEl.textContent = Number(result.elapsedMs || 0).toFixed(1) + ' ms';

      document.documentElement.style.setProperty('--column-count', String(Math.max(displayColumns.length, 1)));
      header.innerHTML = displayColumns.map(column => '<div class="cell">' + escapeHtml(column) + '</div>').join('');
      spacer.style.height = ((rows.length * rowHeight) + rowHeight) + 'px';
      spacer.hidden = false;
      empty.hidden = rows.length > 0;
      table.scrollTop = 0;
      render();
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

    function setStatus(message, isError) {
      statusEl.textContent = message;
      statusEl.title = message;
      statusEl.classList.toggle('error', Boolean(isError));
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
