import * as vscode from 'vscode';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export interface QueryResult {
  columns: string[];
  rowCount: number;
  pageSize: number;
  elapsedMs: number;
  source: string;
  pages: Map<number, Record<string, unknown>[]>;
}

export class ResultsViewProvider implements vscode.WebviewViewProvider {
  static readonly viewType = 'quickql.results';

  private view: vscode.WebviewView | undefined;
  private result: QueryResult | undefined;

  constructor(private readonly context: vscode.ExtensionContext) {}

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.view = webviewView;
    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.context.extensionUri, 'node_modules', 'plotly.js-dist-min')
      ]
    };
    webviewView.webview.onDidReceiveMessage(async message => {
      if (message?.type === 'page' && this.result) {
        const rows = await this.readRows(message.start, message.count);
        await this.view?.webview.postMessage({ type: 'page', start: message.start, rows });
      } else if (message?.type === 'graphRow' && this.result) {
        const rows = await this.readRows(0, 1);
        await this.view?.webview.postMessage({ type: 'graphRow', row: rows[0] ?? null });
      } else if (message?.type === 'openJson' && this.result) {
        try {
          await this.openJsonResult(this.result);
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          void vscode.window.showErrorMessage(`Unable to open QuickQL results as JSON: ${message}`);
        }
      }
    }, undefined, this.context.subscriptions);
    this.render();
  }

  setResult(result: QueryResult): void {
    this.result = result;
    this.render();
  }

  private async readRows(start: number, count: number): Promise<Record<string, unknown>[]> {
    if (!this.result) {
      return [];
    }

    const safeStart = Math.max(0, Math.min(start, this.result.rowCount));
    const safeCount = Math.max(0, Math.min(count, this.result.rowCount - safeStart));
    if (safeCount === 0) {
      return [];
    }

    const rows: Record<string, unknown>[] = [];
    for (let rowIndex = safeStart; rowIndex < safeStart + safeCount; rowIndex += 1) {
      const pageStart = Math.floor(rowIndex / this.result.pageSize) * this.result.pageSize;
      const page = this.result.pages.get(pageStart);
      rows.push(page?.[rowIndex - pageStart] ?? {});
    }
    return rows;
  }

  private async openJsonResult(result: QueryResult): Promise<void> {
    const uri = await this.writeJsonResult(result);
    await vscode.commands.executeCommand('vscode.open', uri, {
      preview: false
    });
  }

  private async writeJsonResult(result: QueryResult): Promise<vscode.Uri> {
    const outputDir = path.join(os.tmpdir(), 'quickql-results');
    await fs.promises.mkdir(outputDir, { recursive: true });
    const fileName = `${this.resultFileBaseName(result.source)}-${Date.now()}.json`;
    const filePath = path.join(outputDir, fileName);
    const stream = fs.createWriteStream(filePath, { encoding: 'utf8' });

    try {
      await writeToStream(stream, '[\n');
      let isFirstRow = true;
      for (let rowIndex = 0; rowIndex < result.rowCount; rowIndex += 1) {
        const pageStart = Math.floor(rowIndex / result.pageSize) * result.pageSize;
        const page = result.pages.get(pageStart);
        const row = page?.[rowIndex - pageStart] ?? {};
        const prefix = isFirstRow ? '  ' : ',\n  ';
        await writeToStream(stream, prefix + JSON.stringify(row));
        isFirstRow = false;
      }
      await writeToStream(stream, '\n]\n');
    } catch (error) {
      stream.destroy();
      throw error;
    }

    await closeStream(stream);
    return vscode.Uri.file(filePath);
  }

  private resultFileBaseName(source: string): string {
    const parsed = path.parse(source);
    const baseName = parsed.name || parsed.base || 'quickql-results';
    return baseName.replace(/[^a-zA-Z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'quickql-results';
  }

  private render(): void {
    if (this.view) {
      this.view.webview.html = this.result ? this.html(this.result, this.view.webview) : this.emptyHtml();
    }
  }

  private emptyHtml(): string {
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style>
    body {
      margin: 0;
      padding: 16px;
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      color: var(--vscode-descriptionForeground);
      background: var(--vscode-editor-background);
    }
  </style>
</head>
<body>Run a .ql file to show results.</body>
</html>`;
  }

  private html(result: QueryResult, webview: vscode.Webview): string {
    const nonce = getNonce();
    const columns = JSON.stringify(result.columns);
    const rowCount = JSON.stringify(result.rowCount);
    const pageSize = result.pageSize;
    const renderedColumnCount = Math.max(result.columns.length, 1);
    const elapsed = Number.isFinite(result.elapsedMs) ? result.elapsedMs.toFixed(1) : '0.0';
    const plotlyScriptUri = webview.asWebviewUri(vscode.Uri.joinPath(
      this.context.extensionUri,
      'node_modules',
      'plotly.js-dist-min',
      'plotly.min.js'
    ));

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style>
    :root {
      color-scheme: light dark;
      --border: var(--vscode-panel-border);
      --bg: var(--vscode-editor-background);
      --fg: var(--vscode-editor-foreground);
      --muted: var(--vscode-descriptionForeground);
      --header: var(--vscode-sideBar-background);
      --row-hover: var(--vscode-list-hoverBackground);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      color: var(--fg);
      background: var(--bg);
      overflow: hidden;
    }
    .toolbar {
      height: 34px;
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 0 10px;
      border-bottom: 1px solid var(--border);
      color: var(--muted);
      white-space: nowrap;
    }
    .toolbar .meta {
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .toolbar .source {
      color: var(--fg);
    }
    .toolbar-spacer {
      flex: 1 1 auto;
    }
    .view-switch {
      display: inline-flex;
      border: 1px solid var(--vscode-button-border, var(--border));
      border-radius: 2px;
      overflow: hidden;
      flex: 0 0 auto;
    }
    button {
      border: 1px solid var(--vscode-button-border, transparent);
      border-radius: 2px;
      color: var(--vscode-button-foreground);
      background: var(--vscode-button-background);
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      padding: 3px 9px;
      cursor: pointer;
      white-space: nowrap;
    }
    .view-switch button {
      border: 0;
      border-radius: 0;
      color: var(--vscode-foreground);
      background: transparent;
      padding: 3px 8px;
    }
    .view-switch button.active {
      color: var(--vscode-button-foreground);
      background: var(--vscode-button-background);
    }
    button:hover {
      background: var(--vscode-button-hoverBackground);
    }
    .table {
      position: relative;
      height: calc(100vh - 34px);
      overflow: auto;
    }
    .spacer {
      position: relative;
      width: max-content;
      min-width: 100%;
    }
    .header, .row {
      display: grid;
      grid-template-columns: repeat(${renderedColumnCount}, minmax(140px, 1fr));
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
    }
    .viewport {
      position: absolute;
      left: 0;
      right: 0;
      top: 28px;
    }
    .row {
      height: 28px;
    }
    .row:hover {
      background: var(--row-hover);
    }
    .graph {
      display: none;
      height: calc(100vh - 34px);
      overflow: hidden;
      position: relative;
    }
    .graph.active {
      display: block;
    }
    .table.hidden {
      display: none;
    }
    #graphPlot {
      width: 100%;
      height: 100%;
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
    .graph-message.active {
      display: flex;
    }
  </style>
</head>
<body>
  <div class="toolbar">
    <strong class="meta source" title="${escapeHtml(result.source)}">${escapeHtml(result.source)}</strong>
    <span class="meta">${result.rowCount.toLocaleString()} rows</span>
    <span class="meta">${elapsed} ms</span>
    <span class="toolbar-spacer"></span>
    <div class="view-switch" role="group" aria-label="Result view">
      <button id="tableView" class="active" type="button" aria-pressed="true">Table</button>
      <button id="graphView" type="button" aria-pressed="false">Graph</button>
    </div>
    <button id="openJson" type="button" title="Open results as JSON">Open JSON</button>
  </div>
  <div id="table" class="table">
    <div id="spacer" class="spacer">
      <div id="header" class="header"></div>
      <div id="viewport" class="viewport"></div>
    </div>
  </div>
  <div id="graph" class="graph">
    <div id="graphPlot"></div>
    <div id="graphMessage" class="graph-message"></div>
  </div>
  <script nonce="${nonce}" src="${plotlyScriptUri}"></script>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    const columns = ${columns};
    const hasMetaColumns = columns.length > 0;
    const displayColumns = hasMetaColumns ? columns : ['row'];
    const rowCount = ${rowCount};
    const pageSize = ${pageSize};
    const rowHeight = 28;
    const table = document.getElementById('table');
    const header = document.getElementById('header');
    const spacer = document.getElementById('spacer');
    const viewport = document.getElementById('viewport');
    const graph = document.getElementById('graph');
    const graphPlot = document.getElementById('graphPlot');
    const graphMessage = document.getElementById('graphMessage');
    const tableView = document.getElementById('tableView');
    const graphView = document.getElementById('graphView');
    const openJson = document.getElementById('openJson');
    const cache = new Map();
    const pending = new Set();
    let activeView = 'table';
    let graphRowRequested = false;
    let graphRowLoaded = false;
    let graphRow = null;
    let graphRendered = false;

    openJson.addEventListener('click', () => {
      vscode.postMessage({ type: 'openJson' });
    });
    tableView.addEventListener('click', () => setView('table'));
    graphView.addEventListener('click', () => setView('graph'));

    header.innerHTML = displayColumns.map(c => '<div class="cell">' + escapeHtml(c) + '</div>').join('');
    spacer.style.height = ((rowCount * rowHeight) + rowHeight) + 'px';

    window.addEventListener('message', event => {
      const message = event.data;
      if (message.type === 'page') {
        cache.set(message.start, message.rows);
        pending.delete(message.start);
        render();
      } else if (message.type === 'graphRow') {
        graphRowLoaded = true;
        graphRow = message.row;
        if (activeView === 'graph') {
          renderGraph(graphRow);
        }
      }
    });

    table.addEventListener('scroll', render);
    render();

    function render() {
      const firstVisible = Math.max(0, Math.floor((table.scrollTop - rowHeight) / rowHeight));
      const visibleCount = Math.ceil(table.clientHeight / rowHeight) + 8;
      const firstPage = Math.floor(firstVisible / pageSize) * pageSize;
      const lastPage = Math.floor(Math.min(rowCount - 1, firstVisible + visibleCount) / pageSize) * pageSize;
      for (let pageStart = firstPage; pageStart <= lastPage; pageStart += pageSize) {
        if (!cache.has(pageStart) && !pending.has(pageStart)) {
          pending.add(pageStart);
          vscode.postMessage({ type: 'page', start: pageStart, count: pageSize });
        }
      }

      const rows = collectRows(firstVisible, visibleCount);
      viewport.style.transform = 'translateY(' + (firstVisible * rowHeight) + 'px)';
      viewport.innerHTML = rows.map(item => {
        const row = item.row || {};
        return '<div class="row">' + displayColumns.map(col => {
          const value = hasMetaColumns ? row[col] : row;
          const formatted = format(value);
          return '<div class="cell" title="' + escapeAttr(formatted) + '">' + escapeHtml(formatted) + '</div>';
        }).join('') + '</div>';
      }).join('');
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
        requestGraphRow();
        if (graphRowLoaded && !graphRendered) {
          renderGraph(graphRow);
        } else if (graphRendered && window.Plotly) {
          Plotly.Plots.resize(graphPlot);
        }
      } else {
        render();
      }
    }

    function requestGraphRow() {
      if (graphRowLoaded) return;
      if (graphRowRequested) return;
      graphRowRequested = true;
      showGraphMessage('Loading graph...');
      vscode.postMessage({ type: 'graphRow' });
    }

    function renderGraph(row) {
      if (activeView !== 'graph') return;
      if (!row) {
        showGraphMessage('No result row available for graph view.');
        return;
      }

      const spec = unwrapPlotSpec(row);
      if (!spec || spec.data === undefined) {
        showGraphMessage('The first result row must contain a data field for Plotly.');
        return;
      }

      try {
        hideGraphMessage();
        const config = Object.assign({ responsive: true, displaylogo: false }, spec.config || {});
        Plotly.newPlot(graphPlot, normalizePlotData(spec.data), spec.layout || {}, config);
        graphRendered = true;
      } catch (error) {
        graphRendered = false;
        showGraphMessage('Unable to render graph: ' + (error && error.message ? error.message : String(error)));
      }
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

    function collectRows(start, count) {
      const result = [];
      for (let rowIndex = start; rowIndex < Math.min(rowCount, start + count); rowIndex++) {
        const pageStart = Math.floor(rowIndex / pageSize) * pageSize;
        const page = cache.get(pageStart);
        if (page) {
          result.push({ row: page[rowIndex - pageStart] });
        } else {
          result.push({ row: {} });
        }
      }
      return result;
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
</html>`;
  }
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, ch => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch] ?? ch));
}

function writeToStream(stream: fs.WriteStream, chunk: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const onError = (error: Error): void => {
      stream.off('drain', onDrain);
      reject(error);
    };
    const onDrain = (): void => {
      stream.off('error', onError);
      resolve();
    };

    stream.once('error', onError);
    if (stream.write(chunk)) {
      stream.off('error', onError);
      resolve();
    } else {
      stream.once('drain', onDrain);
    }
  });
}

function closeStream(stream: fs.WriteStream): Promise<void> {
  return new Promise((resolve, reject) => {
    const onError = (error: Error): void => {
      stream.off('finish', onFinish);
      reject(error);
    };
    const onFinish = (): void => {
      stream.off('error', onError);
      resolve();
    };

    stream.once('error', onError);
    stream.once('finish', onFinish);
    stream.end();
  });
}

function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let value = '';
  for (let i = 0; i < 32; i += 1) {
    value += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return value;
}
