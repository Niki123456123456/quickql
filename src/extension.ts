import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import { spawn } from 'child_process';
import { LanguageClient, LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node';
import { QueryResult, ResultsViewProvider } from './resultsPanel';

let client: LanguageClient | undefined;
let resultsProvider: ResultsViewProvider;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  resultsProvider = new ResultsViewProvider(context);
  context.subscriptions.push(vscode.window.registerWebviewViewProvider(ResultsViewProvider.viewType, resultsProvider));

  const codeLensProvider = vscode.languages.registerCodeLensProvider({ language: 'ql' }, new QueryCodeLensProvider());
  context.subscriptions.push(codeLensProvider);

  context.subscriptions.push(vscode.commands.registerCommand('quickql.runQuery', async () => {
    await runActiveQuery(context);
  }));

  context.subscriptions.push(vscode.commands.registerCommand('quickql.openResults', () => {
    void vscode.commands.executeCommand('quickql.results.focus');
  }));

  startLanguageServer(context);
}

export async function deactivate(): Promise<void> {
  await client?.stop();
}

class QueryCodeLensProvider implements vscode.CodeLensProvider {
  provideCodeLenses(document: vscode.TextDocument): vscode.CodeLens[] {
    if (document.languageId !== 'ql') {
      return [];
    }

    const range = new vscode.Range(0, 0, 0, 0);
    return [
      new vscode.CodeLens(range, {
        title: '$(play) Run Query',
        command: 'quickql.runQuery',
        arguments: [document.uri]
      })
    ];
  }
}

async function runActiveQuery(context: vscode.ExtensionContext): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== 'ql') {
    void vscode.window.showWarningMessage('Open a .ql file before running a query.');
    return;
  }

  if (editor.document.isDirty) {
    await editor.document.save();
  }

  const engine = resolveBinary(context, 'quickql.enginePath', 'ql-engine');
  if (!fs.existsSync(engine)) {
    void vscode.window.showErrorMessage(`Query engine not found at ${engine}. Run "npm run build:rust" first.`);
    return;
  }

  const workspaceFolder = vscode.workspace.getWorkspaceFolder(editor.document.uri);
  const cwd = workspaceFolder?.uri.fsPath ?? path.dirname(editor.document.uri.fsPath);
  const pageSize = vscode.workspace.getConfiguration().get<number>('quickql.pageSize', 200);

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: 'Running QuickQL query',
      cancellable: false
    },
    async () => {
      const result = await runEngine(engine, editor.document.uri.fsPath, cwd, pageSize);
      resultsProvider.setResult(result);
      await vscode.commands.executeCommand('quickql.results.focus');
    }
  );
}

interface StreamMeta {
  type: 'meta';
  columns: string[];
  source: string;
}

interface StreamRow {
  type: 'row';
  row: unknown[];
}

interface StreamBatch {
  type: 'batch';
  start: number;
  rows: unknown[][];
}

interface StreamDone {
  type: 'done';
  rowCount?: number;
  row_count?: number;
  elapsedMs?: number;
  elapsed_ms?: number;
}

type StreamMessage = StreamMeta | StreamRow | StreamBatch | StreamDone;

function runEngine(engine: string, queryPath: string, cwd: string, pageSize: number): Promise<QueryResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(engine, ['stream', '--query', queryPath, '--batch-size', String(pageSize)], { cwd });
    let stderr = '';
    let stdoutBuffer = '';
    let settled = false;
    let columns: string[] = [];
    let source = queryPath;
    let rowCount = 0;
    let elapsedMs = 0;
    const pages = new Map<number, unknown[][]>();

    child.stdout.on('data', chunk => {
      stdoutBuffer += chunk.toString();
      processStdoutLines(false);
    });
    child.stderr.on('data', chunk => {
      stderr += chunk.toString();
    });
    child.on('error', error => {
      settled = true;
      reject(error);
    });
    child.on('close', code => {
      if (settled) {
        return;
      }

      if (code !== 0) {
        reject(new Error(stderr.trim() || `Query engine exited with code ${code}`));
        return;
      }

      try {
        processStdoutLines(true);
        resolve({
          columns,
          rowCount,
          pageSize,
          elapsedMs,
          source,
          pages
        });
      } catch (error) {
        reject(error);
      }
    });

    function processStdoutLines(flush: boolean): void {
      let newline = stdoutBuffer.indexOf('\n');
      while (newline >= 0) {
        const line = stdoutBuffer.slice(0, newline);
        stdoutBuffer = stdoutBuffer.slice(newline + 1);
        processLine(line);
        newline = stdoutBuffer.indexOf('\n');
      }

      if (flush && stdoutBuffer.trim().length > 0) {
        processLine(stdoutBuffer);
        stdoutBuffer = '';
      }
    }

    function processLine(line: string): void {
      if (line.trim().length === 0) {
        return;
      }

      const message = JSON.parse(line) as StreamMessage;
      if (message.type === 'meta') {
        columns = message.columns;
        source = message.source;
      } else if (message.type === 'row') {
        appendRow(message.row);
      } else if (message.type === 'batch') {
        for (let i = 0; i < message.rows.length; i += 1) {
          appendRow(message.rows[i]);
        }
      } else if (message.type === 'done') {
        rowCount = message.rowCount ?? message.row_count ?? rowCount;
        elapsedMs = message.elapsedMs ?? message.elapsed_ms ?? elapsedMs;
      }
    }

    function appendRow(row: unknown[]): void {
      const pageStart = Math.floor(rowCount / pageSize) * pageSize;
      let page = pages.get(pageStart);
      if (!page) {
        page = [];
        pages.set(pageStart, page);
      }
      page.push(row);
      rowCount += 1;
    }
  });
}

function startLanguageServer(context: vscode.ExtensionContext): void {
  const binary = resolveBinary(context, 'quickql.languageServerPath', 'ql-lsp');
  if (!fs.existsSync(binary)) {
    void vscode.window.showWarningMessage(`QuickQL language server not found at ${binary}. Run "npm run build:rust" to enable completions.`);
    return;
  }

  const serverOptions: ServerOptions = {
    run: { command: binary },
    debug: { command: binary }
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'ql' }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.json')
    }
  };

  client = new LanguageClient('quickqlLsp', 'QuickQL Language Server', serverOptions, clientOptions);
  context.subscriptions.push(client);
  void client.start();
}

function resolveBinary(context: vscode.ExtensionContext, setting: string, binaryName: string): string {
  const configured = vscode.workspace.getConfiguration().get<string>(setting);
  if (configured && configured.trim().length > 0) {
    return configured;
  }

  const exe = process.platform === 'win32' ? `${binaryName}.exe` : binaryName;
  const packaged = path.join(context.extensionPath, 'bin', exe);
  if (fs.existsSync(packaged)) {
    return packaged;
  }

  return path.join(context.extensionPath, 'target', 'release', exe);
}
