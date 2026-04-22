//! Higher-level LSP operations mapped onto MCP tools.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use lsp_types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lsp_types::request::{GotoDefinition, HoverRequest};
use lsp_types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, HoverContents, HoverParams, MarkedString, PartialResultParams,
    Position, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use serde::Serialize;

use crate::client::{LspClient, LspClientError};

/// A simplified hover result — combined plain text plus language-tagged code blocks.
#[derive(Debug, Clone, Serialize)]
pub struct HoverHit {
    pub text: String,
}

/// A single code location (file + range).
#[derive(Debug, Clone, Serialize)]
pub struct LocationHit {
    pub uri: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// A single diagnostic simplified for MCP consumers.
#[derive(Debug, Clone, Serialize)]
pub struct PublishedDiagnostic {
    pub severity: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub message: String,
    pub source: Option<String>,
}

/// Open or refresh `path` in the server, then return its hover info at `(line, col)`.
pub async fn hover(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<Option<HoverHit>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let result = client.request::<HoverRequest>(params).await?;
    Ok(result.map(|h| HoverHit {
        text: hover_to_string(&h.contents),
    }))
}

/// Open or refresh `path` and return goto-definition results at `(line, col)`.
pub async fn definition(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<Vec<LocationHit>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let result = client.request::<GotoDefinition>(params).await?;
    Ok(match result {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(loc)) => vec![location_hit(&loc)],
        Some(GotoDefinitionResponse::Array(locs)) => locs.iter().map(location_hit).collect(),
        Some(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|l| LocationHit {
                uri: l.target_uri.to_string(),
                start_line: l.target_selection_range.start.line,
                start_col: l.target_selection_range.start.character,
                end_line: l.target_selection_range.end.line,
                end_col: l.target_selection_range.end.character,
            })
            .collect(),
    })
}

/// Open or refresh `path`, wait `settle` for the server to publish diagnostics,
/// then return the current snapshot.
pub async fn diagnostics(
    client: &LspClient,
    path: &Path,
    settle: Duration,
) -> Result<Vec<PublishedDiagnostic>, LspClientError> {
    ensure_document_current(client, path).await?;
    tokio::time::sleep(settle).await;
    let uri = path_to_uri(path)?;
    let raw = client.diagnostics_for(&uri).await;
    Ok(raw
        .into_iter()
        .map(|d| PublishedDiagnostic {
            severity: d
                .severity
                .map_or_else(|| "Unknown".to_string(), |s| format!("{s:?}")),
            line: d.range.start.line,
            col: d.range.start.character,
            end_line: d.range.end.line,
            end_col: d.range.end.character,
            message: d.message,
            source: d.source,
        })
        .collect())
}

async fn ensure_document_current(client: &LspClient, path: &Path) -> Result<(), LspClientError> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let uri = path_to_uri(path)?;
    let language_id = language_id_for(path);

    let mut docs = client.opened_documents().lock().await;
    match docs.get_mut(&uri) {
        Some(entry) if entry.text == text => {
            // Nothing changed.
            Ok(())
        }
        Some(entry) => {
            entry.version += 1;
            entry.text.clone_from(&text);
            let version = entry.version;
            drop(docs);
            let params = DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier { uri, version },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
            };
            client.notify::<DidChangeTextDocument>(params).await
        }
        None => {
            docs.insert(
                uri.clone(),
                crate::client::OpenedDocument {
                    version: 1,
                    text: text.clone(),
                },
            );
            drop(docs);
            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri,
                    language_id: language_id.to_string(),
                    version: 1,
                    text,
                },
            };
            client.notify::<DidOpenTextDocument>(params).await
        }
    }
}

fn path_to_uri(path: &Path) -> Result<Uri, LspClientError> {
    crate::client::path_to_uri(path)
}

fn language_id_for(path: &Path) -> &'static str {
    match path.extension().and_then(OsStr::to_str) {
        Some("rs") => "rust",
        Some("ts" | "tsx") => "typescript",
        Some("js" | "jsx") => "javascript",
        Some("py") => "python",
        Some("go") => "go",
        _ => "plaintext",
    }
}

fn location_hit(loc: &lsp_types::Location) -> LocationHit {
    LocationHit {
        uri: loc.uri.to_string(),
        start_line: loc.range.start.line,
        start_col: loc.range.start.character,
        end_line: loc.range.end.line,
        end_col: loc.range.end.character,
    }
}

fn hover_to_string(contents: &HoverContents) -> String {
    match contents {
        HoverContents::Scalar(s) => marked_string_to_plain(s),
        HoverContents::Array(items) => items
            .iter()
            .map(marked_string_to_plain)
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(m) => m.value.clone(),
    }
}

fn marked_string_to_plain(s: &MarkedString) -> String {
    match s {
        MarkedString::String(s) => s.clone(),
        MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}
