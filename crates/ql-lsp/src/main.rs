use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug, Default)]
struct Backend {
    documents: Arc<RwLock<HashMap<Url, String>>>,
    field_cache: Arc<RwLock<HashMap<PathBuf, FieldCacheEntry>>>,
}

#[derive(Debug, Clone)]
struct FieldCacheEntry {
    modified: Option<SystemTime>,
    len: u64,
    fields: Vec<String>,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![",".to_string(), " ".to_string()]),
                    ..CompletionOptions::default()
                }),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents
            .write()
            .await
            .insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents
                .write()
                .await
                .insert(params.text_document.uri, change.text);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let docs = self.documents.read().await;
        let text = docs.get(&uri).cloned().unwrap_or_default();
        drop(docs);

        let mut items = keyword_items();
        if let Ok(path) = uri.to_file_path() {
            items.extend(self.field_items(path, &text).await);
        }

        Ok(Some(CompletionResponse::Array(items)))
    }
}

fn keyword_items() -> Vec<CompletionItem> {
    [
        "SOURCE", "MAP", "FILTER", "OR", "GROUP_BY", "SUM", "ARRAY", "MINDATE", "MAXDATE", "COUNT",
        "GETDATE", "CONCAT", "MAP_MANY", "SORT_BY",
    ]
    .into_iter()
    .map(|label| CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("QuickQL keyword".to_string()),
        ..CompletionItem::default()
    })
    .collect()
}

impl Backend {
    async fn field_items(&self, query_path: PathBuf, text: &str) -> Vec<CompletionItem> {
        let fields = self.fields_for_query(query_path, text).await;
        fields
            .into_iter()
            .map(|label| CompletionItem {
                label,
                kind: Some(CompletionItemKind::FIELD),
                detail: Some("Source field".to_string()),
                ..CompletionItem::default()
            })
            .collect()
    }

    async fn fields_for_query(&self, query_path: PathBuf, text: &str) -> Vec<String> {
        let Ok(Some(source_path)) = quickql_core::source_path_for_query(&query_path, text) else {
            return Vec::new();
        };

        let Ok(metadata) = fs::metadata(&source_path) else {
            return Vec::new();
        };
        let modified = metadata.modified().ok();
        let len = metadata.len();

        {
            let cache = self.field_cache.read().await;
            if let Some(entry) = cache.get(&source_path) {
                if entry.modified == modified && entry.len == len {
                    return entry.fields.clone();
                }
            }
        }

        let fields = quickql_core::fields_from_source_sample(&source_path, 100).unwrap_or_default();
        self.field_cache.write().await.insert(
            source_path,
            FieldCacheEntry {
                modified,
                len,
                fields: fields.clone(),
            },
        );
        fields
    }
}

fn _field_items_uncached(query_path: PathBuf, text: &str) -> Vec<CompletionItem> {
    quickql_core::json_fields_for_query(&query_path, text)
        .unwrap_or_default()
        .into_iter()
        .map(|label| CompletionItem {
            label,
            kind: Some(CompletionItemKind::FIELD),
            detail: Some("Source field".to_string()),
            ..CompletionItem::default()
        })
        .collect()
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (_service, socket) = LspService::new(|_client: Client| Backend::default());
    Server::new(stdin, stdout, socket).serve(_service).await;
}
