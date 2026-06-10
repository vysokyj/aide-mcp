//! Higher-level LSP operations mapped onto MCP tools.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use lsp_types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lsp_types::request::{
    CodeActionRequest, CodeActionResolveRequest, DocumentSymbolRequest, ExecuteCommand,
    GotoDeclaration, GotoDefinition, GotoImplementation, HoverRequest, References, Rename,
    TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes, WorkspaceSymbolRequest,
};
use lsp_types::{
    CodeActionContext, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentChangeOperation,
    DocumentChanges, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    ExecuteCommandParams, GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams,
    MarkedString, PartialResultParams, Position, Range, ReferenceContext, ReferenceParams,
    RenameParams, SymbolInformation, SymbolKind, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit,
    TypeHierarchyItem, TypeHierarchyPrepareParams, TypeHierarchySubtypesParams,
    TypeHierarchySupertypesParams, Uri, VersionedTextDocumentIdentifier, WorkDoneProgressParams,
    WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolParams, WorkspaceSymbolResponse,
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
    /// Display name of the enclosing definition (function / struct /
    /// method) at this location, populated server-side from the
    /// most-recent Ready SCIP index. `None` when no SCIP index is
    /// available, the location's file isn't covered by the index, or
    /// the line falls outside any indexed definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
}

/// A node in a file's symbol tree (function, struct, method, …).
#[derive(Debug, Clone, Serialize)]
pub struct SymbolNode {
    pub name: String,
    pub kind: &'static str,
    pub detail: Option<String>,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SymbolNode>,
}

/// A flat workspace-symbol-search result.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSymbolHit {
    pub name: String,
    pub kind: &'static str,
    pub container: Option<String>,
    pub location: LocationHit,
}

/// A single entry in a type hierarchy — one supertype or subtype.
#[derive(Debug, Clone, Serialize)]
pub struct TypeHierarchyHit {
    pub name: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub uri: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Result of a type-hierarchy query — the symbol(s) at the cursor
/// position plus the requested supertypes / subtypes lists.
///
/// `supertypes` and `subtypes` are `None` when that direction wasn't
/// requested (vs. `Some(vec![])` when requested but the server returned
/// nothing). `origin` mirrors LSP's `textDocument/prepareTypeHierarchy`
/// — usually one entry, but can be empty when the cursor isn't on a
/// type, or several when overload resolution is ambiguous.
#[derive(Debug, Clone, Serialize)]
pub struct TypeHierarchyResult {
    pub origin: Vec<TypeHierarchyHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supertypes: Option<Vec<TypeHierarchyHit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtypes: Option<Vec<TypeHierarchyHit>>,
}

/// Which direction to walk a prepared type hierarchy.
#[derive(Debug, Clone, Copy)]
pub enum TypeHierarchyDirection {
    Supertypes,
    Subtypes,
    Both,
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
    /// Display name of the enclosing definition that owns this
    /// diagnostic, populated server-side from the most-recent Ready
    /// SCIP index. Same semantics as
    /// [`LocationHit::enclosing_symbol`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
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
    let params = goto_position_params(client, path, line, col).await?;
    let result = client.request::<GotoDefinition>(params).await?;
    Ok(goto_response_to_hits(result))
}

/// Open or refresh `path` and return goto-declaration results at `(line, col)`.
///
/// Distinct from [`definition`] for languages that separate forward
/// declarations from definitions — most prominently C/C++ headers and
/// TypeScript ambient `declare` blocks. For Rust the answer is usually
/// identical to `definition`, but the LSP method is wired through for
/// parity.
pub async fn declaration(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<Vec<LocationHit>, LspClientError> {
    let params = goto_position_params(client, path, line, col).await?;
    let result = client.request::<GotoDeclaration>(params).await?;
    Ok(goto_response_to_hits(result))
}

/// Open or refresh `path` and return goto-implementation results at `(line, col)`.
///
/// For trait methods and interfaces this returns every concrete
/// implementor — what `lsp_definition` on a trait method cannot
/// answer.
pub async fn implementations(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<Vec<LocationHit>, LspClientError> {
    let params = goto_position_params(client, path, line, col).await?;
    let result = client.request::<GotoImplementation>(params).await?;
    Ok(goto_response_to_hits(result))
}

async fn goto_position_params(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<GotoDefinitionParams, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    Ok(GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    })
}

/// Prepare a type hierarchy at `(line, col)` and resolve the requested
/// direction(s). LSP splits this into two requests
/// (`prepareTypeHierarchy` then `supertypes` / `subtypes`); this
/// helper does both in one round-trip from the caller's perspective.
///
/// `Both` issues one supertypes and one subtypes request per origin
/// item — the cost is one extra round-trip vs. picking a single
/// direction.
pub async fn type_hierarchy(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    direction: TypeHierarchyDirection,
) -> Result<TypeHierarchyResult, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let prepare = TypeHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let origin_items = client
        .request::<TypeHierarchyPrepare>(prepare)
        .await?
        .unwrap_or_default();

    let want_super = matches!(
        direction,
        TypeHierarchyDirection::Supertypes | TypeHierarchyDirection::Both
    );
    let want_sub = matches!(
        direction,
        TypeHierarchyDirection::Subtypes | TypeHierarchyDirection::Both
    );

    let mut supertypes = if want_super { Some(Vec::new()) } else { None };
    let mut subtypes = if want_sub { Some(Vec::new()) } else { None };

    for item in &origin_items {
        if let Some(supers) = supertypes.as_mut() {
            let params = TypeHierarchySupertypesParams {
                item: item.clone(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            };
            let result = client
                .request::<TypeHierarchySupertypes>(params)
                .await?
                .unwrap_or_default();
            supers.extend(result.iter().map(type_hierarchy_hit));
        }
        if let Some(subs) = subtypes.as_mut() {
            let params = TypeHierarchySubtypesParams {
                item: item.clone(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            };
            let result = client
                .request::<TypeHierarchySubtypes>(params)
                .await?
                .unwrap_or_default();
            subs.extend(result.iter().map(type_hierarchy_hit));
        }
    }

    Ok(TypeHierarchyResult {
        origin: origin_items.iter().map(type_hierarchy_hit).collect(),
        supertypes,
        subtypes,
    })
}

fn type_hierarchy_hit(item: &TypeHierarchyItem) -> TypeHierarchyHit {
    TypeHierarchyHit {
        name: item.name.clone(),
        kind: symbol_kind_name(item.kind),
        detail: item.detail.clone(),
        uri: item.uri.to_string(),
        start_line: item.selection_range.start.line,
        start_col: item.selection_range.start.character,
        end_line: item.selection_range.end.line,
        end_col: item.selection_range.end.character,
    }
}

fn goto_response_to_hits(result: Option<GotoDefinitionResponse>) -> Vec<LocationHit> {
    match result {
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
                enclosing_symbol: None,
            })
            .collect(),
    }
}

/// Open or refresh `path` and return the symbol references at `(line, col)`.
///
/// `include_declaration` controls whether the defining occurrence is returned
/// alongside read/write sites.
pub async fn references(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    include_declaration: bool,
) -> Result<Vec<LocationHit>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration,
        },
    };
    let result = client.request::<References>(params).await?;
    Ok(result
        .unwrap_or_default()
        .iter()
        .map(location_hit)
        .collect())
}

/// Return the full symbol tree of `path` as a hierarchy of [`SymbolNode`].
///
/// Falls back to a flat list for servers that only support the older
/// `SymbolInformation` response shape.
pub async fn document_symbols(
    client: &LspClient,
    path: &Path,
) -> Result<Vec<SymbolNode>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let result = client.request::<DocumentSymbolRequest>(params).await?;
    Ok(match result {
        None => Vec::new(),
        Some(DocumentSymbolResponse::Nested(items)) => {
            items.iter().map(symbol_node_from_nested).collect()
        }
        Some(DocumentSymbolResponse::Flat(items)) => {
            items.iter().map(symbol_node_from_flat).collect()
        }
    })
}

/// Fuzzy symbol search across the whole workspace.
pub async fn workspace_symbols(
    client: &LspClient,
    query: &str,
) -> Result<Vec<WorkspaceSymbolHit>, LspClientError> {
    let params = WorkspaceSymbolParams {
        query: query.to_string(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let result = client.request::<WorkspaceSymbolRequest>(params).await?;
    Ok(match result {
        None => Vec::new(),
        Some(WorkspaceSymbolResponse::Flat(items)) => {
            items.iter().map(workspace_hit_from_flat).collect()
        }
        Some(WorkspaceSymbolResponse::Nested(items)) => {
            items.iter().filter_map(workspace_hit_from_nested).collect()
        }
    })
}

/// Find-and-replace `old_string` with `new_string` in `file`,
/// measure the LSP diagnostic delta, and return the change classified
/// into new errors, new warnings, and resolved findings. `old_string`
/// must be unique in the file (mirrors the `Edit` tool's semantics);
/// the MCP layer errors out early if not.
///
/// Diagnostics are snapshotted from the LSP server's published
/// stream, with a time-boxed `settle` wait between the edit and the
/// after-snapshot to let the server re-analyse. Results are
/// best-effort on servers that don't finish re-analysing inside
/// `settle` — this is marked on the report so the agent knows when
/// to double-check with a full build.
pub async fn safe_edit(
    client: &LspClient,
    path: &Path,
    old_string: &str,
    new_string: &str,
    related_paths: &[std::path::PathBuf],
    settle: Duration,
) -> Result<SafeEditReport, LspClientError> {
    ensure_document_current(client, path).await?;

    let before_contents = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let matches = before_contents.matches(old_string).count();
    if matches != 1 {
        return Err(LspClientError::LspError {
            code: -32_000,
            message: format!(
                "safe_edit: `old_string` must occur exactly once in {} (found {matches})",
                path.display()
            ),
        });
    }

    let after_contents = before_contents.replacen(old_string, new_string, 1);
    apply_and_snapshot(client, path, after_contents, related_paths, settle).await
}

/// Write `after_contents` to `path`, push the change to the LSP server,
/// wait `settle`, and return the published-diagnostic delta across
/// `path` plus `related_paths` (snapshot taken just before the write,
/// then again after the settle).
///
/// Shared backend for [`safe_edit`] and the four symbolic-edit ops
/// (`replace_symbol_body` / `insert_before_symbol` /
/// `insert_after_symbol` / `delete_symbol_range`). The caller is
/// responsible for computing `after_contents` — this fn only handles
/// the apply + snapshot dance.
async fn apply_and_snapshot(
    client: &LspClient,
    path: &Path,
    after_contents: String,
    related_paths: &[std::path::PathBuf],
    settle: Duration,
) -> Result<SafeEditReport, LspClientError> {
    ensure_document_current(client, path).await?;

    let all_paths: Vec<std::path::PathBuf> = std::iter::once(path.to_path_buf())
        .chain(related_paths.iter().cloned())
        .collect();
    let before = snapshot_published_diagnostics(client, &all_paths).await?;

    let uri = path_to_uri(path)?;
    // Hold the docs lock across write + version bump + notify so a
    // concurrent ensure_document_current cannot observe the new bytes
    // on disk and race a conflicting didChange in between.
    {
        let mut docs = client.opened_documents().lock().await;
        tokio::fs::write(path, &after_contents)
            .await
            .map_err(LspClientError::from_io)?;
        if let Some(doc) = docs.get_mut(&uri) {
            doc.version += 1;
            doc.text.clone_from(&after_contents);
            doc.last_used = std::time::Instant::now();
            let version = doc.version;
            let params = DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: after_contents.clone(),
                }],
            };
            client.notify::<DidChangeTextDocument>(params).await?;
        }
    }

    tokio::time::sleep(settle).await;
    let after = snapshot_published_diagnostics(client, &all_paths).await?;

    Ok(build_safe_edit_report(path, &before, &after, settle))
}

async fn snapshot_published_diagnostics(
    client: &LspClient,
    paths: &[std::path::PathBuf],
) -> Result<Vec<DiagnosticSnapshot>, LspClientError> {
    let mut out = Vec::new();
    for p in paths {
        let uri = path_to_uri(p)?;
        let raw = client.diagnostics_for(&uri).await;
        for d in raw {
            out.push(DiagnosticSnapshot {
                file: p.display().to_string(),
                line: d.range.start.line,
                col: d.range.start.character,
                severity: d
                    .severity
                    .map_or_else(|| "Unknown".to_string(), |s| format!("{s:?}")),
                message: d.message,
                source: d.source,
                enclosing_symbol: None,
            });
        }
    }
    Ok(out)
}

fn build_safe_edit_report(
    path: &Path,
    before: &[DiagnosticSnapshot],
    after: &[DiagnosticSnapshot],
    settle: Duration,
) -> SafeEditReport {
    let before_keys: std::collections::HashSet<_> = before.iter().map(diag_key).collect();
    let after_keys: std::collections::HashSet<_> = after.iter().map(diag_key).collect();

    let new: Vec<_> = after
        .iter()
        .filter(|d| !before_keys.contains(&diag_key(d)))
        .cloned()
        .collect();
    let resolved: Vec<_> = before
        .iter()
        .filter(|d| !after_keys.contains(&diag_key(d)))
        .cloned()
        .collect();

    let new_errors: Vec<_> = new
        .iter()
        .filter(|d| d.severity == "Error")
        .cloned()
        .collect();
    let new_warnings: Vec<_> = new
        .iter()
        .filter(|d| d.severity == "Warning")
        .cloned()
        .collect();

    SafeEditReport {
        edited_file: path.display().to_string(),
        total_before: before.len(),
        total_after: after.len(),
        new_errors,
        new_warnings,
        resolved,
        unchanged_count: before_keys.intersection(&after_keys).count(),
        confidence: "best_effort".to_string(),
        settle_ms: u64::try_from(settle.as_millis()).unwrap_or(u64::MAX),
    }
}

fn diag_key(d: &DiagnosticSnapshot) -> (String, u32, u32, String, String) {
    (
        d.file.clone(),
        d.line,
        d.col,
        d.severity.clone(),
        d.message.clone(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticSnapshot {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Display name of the enclosing definition that owns this
    /// diagnostic. Same semantics as
    /// [`LocationHit::enclosing_symbol`] — populated server-side
    /// from the latest Ready SCIP index, `None` when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SafeEditReport {
    pub edited_file: String,
    pub total_before: usize,
    pub total_after: usize,
    pub new_errors: Vec<DiagnosticSnapshot>,
    pub new_warnings: Vec<DiagnosticSnapshot>,
    pub resolved: Vec<DiagnosticSnapshot>,
    pub unchanged_count: usize,
    /// `"best_effort"` — diagnostics are snapshotted from the
    /// published stream after a fixed `settle_ms` wait. On servers
    /// that analyse faster than `settle_ms`, results are reliable;
    /// on slower ones the `new_*` lists may be incomplete. Follow
    /// up with `cargo check` / `run_tests` for definitive results
    /// when the report is non-trivial.
    pub confidence: String,
    pub settle_ms: u64,
}

/// Replace the full definition of the symbol that owns `(line, col)`
/// in `path` with `new_body`. Range is resolved via LSP `documentSymbol`
/// — the deepest symbol whose `selection_range` or `range` contains
/// the position wins. The replacement covers the symbol's full
/// `range` (signature + body), so the caller must supply the complete
/// new definition.
pub async fn replace_symbol_body(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    new_body: &str,
    related_paths: &[std::path::PathBuf],
    settle: Duration,
) -> Result<SafeEditReport, LspClientError> {
    let (range, _name) = require_symbol_at(client, path, line, col).await?;
    let before = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let after = splice_range(&before, range, new_body, path)?;
    apply_and_snapshot(client, path, after, related_paths, settle).await
}

/// Insert `content` immediately before the start of the symbol that
/// owns `(line, col)`. Caller controls newlines — supply a trailing
/// `\n` if the new content should sit on its own line.
pub async fn insert_before_symbol(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    content: &str,
    related_paths: &[std::path::PathBuf],
    settle: Duration,
) -> Result<SafeEditReport, LspClientError> {
    let (range, _name) = require_symbol_at(client, path, line, col).await?;
    let before = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let start = position_to_byte_offset(&before, range.start).ok_or_else(|| invalid_pos(path))?;
    let mut after = String::with_capacity(before.len() + content.len());
    after.push_str(&before[..start]);
    after.push_str(content);
    after.push_str(&before[start..]);
    apply_and_snapshot(client, path, after, related_paths, settle).await
}

/// Insert `content` immediately after the end of the symbol that owns
/// `(line, col)`. Caller controls newlines.
pub async fn insert_after_symbol(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    content: &str,
    related_paths: &[std::path::PathBuf],
    settle: Duration,
) -> Result<SafeEditReport, LspClientError> {
    let (range, _name) = require_symbol_at(client, path, line, col).await?;
    let before = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let end = position_to_byte_offset(&before, range.end).ok_or_else(|| invalid_pos(path))?;
    let mut after = String::with_capacity(before.len() + content.len());
    after.push_str(&before[..end]);
    after.push_str(content);
    after.push_str(&before[end..]);
    apply_and_snapshot(client, path, after, related_paths, settle).await
}

/// Delete the full definition of the symbol that owns `(line, col)`.
/// Does NOT trim surrounding blank lines — the caller can clean up
/// with a follow-up `safe_edit` if needed. The reference-safety check
/// (refuse if call sites remain) lives at the MCP layer where SCIP
/// is available; this op is the raw delete.
pub async fn delete_symbol_range(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    related_paths: &[std::path::PathBuf],
    settle: Duration,
) -> Result<SafeEditReport, LspClientError> {
    let (range, _name) = require_symbol_at(client, path, line, col).await?;
    let before = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let after = splice_range(&before, range, "", path)?;
    apply_and_snapshot(client, path, after, related_paths, settle).await
}

/// Locate the deepest LSP `DocumentSymbol` whose `selection_range`
/// (and failing that, full `range`) contains `(line, col)`. Returns
/// the symbol's full `range` and its `name`.
pub async fn locate_symbol(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<Option<(Range, String)>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let response = client.request::<DocumentSymbolRequest>(params).await?;
    let symbols = match response {
        Some(DocumentSymbolResponse::Nested(syms)) => syms,
        Some(DocumentSymbolResponse::Flat(_)) | None => return Ok(None),
    };
    let position = Position {
        line,
        character: col,
    };
    Ok(find_enclosing_symbol(&symbols, position))
}

async fn require_symbol_at(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<(Range, String), LspClientError> {
    locate_symbol(client, path, line, col)
        .await?
        .ok_or_else(|| LspClientError::LspError {
            code: -32_000,
            message: format!(
                "no symbol at {}:{}:{} (LSP documentSymbol returned no enclosing node)",
                path.display(),
                line + 1,
                col + 1
            ),
        })
}

fn find_enclosing_symbol(syms: &[DocumentSymbol], position: Position) -> Option<(Range, String)> {
    for sym in syms {
        let in_range = position_in_range(position, sym.range);
        if !in_range && !position_in_range(position, sym.selection_range) {
            continue;
        }
        if let Some(children) = &sym.children {
            if let Some(deeper) = find_enclosing_symbol(children, position) {
                return Some(deeper);
            }
        }
        return Some((sym.range, sym.name.clone()));
    }
    None
}

fn position_in_range(pos: Position, range: Range) -> bool {
    let after_start = pos.line > range.start.line
        || (pos.line == range.start.line && pos.character >= range.start.character);
    let before_end = pos.line < range.end.line
        || (pos.line == range.end.line && pos.character <= range.end.character);
    after_start && before_end
}

fn splice_range(
    content: &str,
    range: Range,
    replacement: &str,
    path: &Path,
) -> Result<String, LspClientError> {
    let start = position_to_byte_offset(content, range.start).ok_or_else(|| invalid_pos(path))?;
    let end = position_to_byte_offset(content, range.end).ok_or_else(|| invalid_pos(path))?;
    if end < start {
        return Err(invalid_pos(path));
    }
    let mut out = String::with_capacity(content.len() + replacement.len() - (end - start));
    out.push_str(&content[..start]);
    out.push_str(replacement);
    out.push_str(&content[end..]);
    Ok(out)
}

fn invalid_pos(path: &Path) -> LspClientError {
    LspClientError::LspError {
        code: -32_000,
        message: format!(
            "could not map LSP position to byte offset in {} — file may have changed since the LSP query",
            path.display()
        ),
    }
}

/// Convert an LSP `Position` (0-based line + UTF-16 column) to a
/// UTF-8 byte offset in `content`. Returns `None` when `position`
/// is past the end of the file. Multi-byte chars on the target line
/// are honoured via `len_utf16` accumulation.
fn position_to_byte_offset(content: &str, position: Position) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut line_start = 0;
    let mut current_line = 0u32;
    while current_line < position.line {
        let nl = bytes.iter().skip(line_start).position(|&b| b == b'\n')?;
        line_start += nl + 1;
        current_line += 1;
    }
    if line_start > bytes.len() {
        return None;
    }
    let line_str = &content[line_start..];
    let mut utf16_offset = 0u32;
    let mut byte_in_line = 0;
    for c in line_str.chars() {
        if utf16_offset >= position.character {
            break;
        }
        if c == '\n' {
            break;
        }
        utf16_offset += u32::try_from(c.len_utf16()).unwrap_or(2);
        byte_in_line += c.len_utf8();
    }
    Some(line_start + byte_in_line)
}

/// Rename the symbol at `(line, col)` to `new_name` across the
/// whole workspace, applying the resulting [`WorkspaceEdit`] to
/// disk (and to the server's in-memory buffers). Returns a summary
/// of what changed — or `None` when the symbol is not renameable at
/// that position. Errors out if applying any edit fails partway
/// through, leaving the partial state on disk.
pub async fn rename(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
    new_name: String,
) -> Result<Option<RenameSummary>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position {
                line,
                character: col,
            },
        },
        new_name,
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let Some(edit) = client.request::<Rename>(params).await? else {
        return Ok(None);
    };
    let applied = apply_workspace_edit(client, &edit).await?;
    Ok(Some(applied))
}

/// Write every change in `edit` back to disk, keeping the LSP
/// server's in-memory buffers in sync. Handles both the legacy
/// `changes` map and the newer `document_changes` shape that
/// rust-analyzer and other LSP 3.16+ servers prefer; file-create /
/// -rename / -delete operations inside `document_changes` are
/// skipped with a warning (can be added later). Edits for each
/// file are applied in descending byte order so earlier ranges
/// remain valid after later ones shift.
pub async fn apply_workspace_edit(
    client: &LspClient,
    edit: &WorkspaceEdit,
) -> Result<RenameSummary, LspClientError> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut total_edits = 0usize;

    if let Some(changes) = edit.changes.as_ref() {
        for (uri, edits) in changes {
            apply_edits_to_file(client, uri, edits, &mut files, &mut total_edits).await?;
        }
    }

    if let Some(doc_changes) = edit.document_changes.as_ref() {
        match doc_changes {
            DocumentChanges::Edits(edits) => {
                for td_edit in edits {
                    let uri = &td_edit.text_document.uri;
                    let text_edits: Vec<TextEdit> = td_edit
                        .edits
                        .iter()
                        .map(|oe| match oe {
                            lsp_types::OneOf::Left(te) => te.clone(),
                            lsp_types::OneOf::Right(annotated) => TextEdit {
                                range: annotated.text_edit.range,
                                new_text: annotated.text_edit.new_text.clone(),
                            },
                        })
                        .collect();
                    apply_edits_to_file(client, uri, &text_edits, &mut files, &mut total_edits)
                        .await?;
                }
            }
            DocumentChanges::Operations(ops) => {
                for op in ops {
                    match op {
                        DocumentChangeOperation::Edit(td_edit) => {
                            let uri = &td_edit.text_document.uri;
                            let text_edits: Vec<TextEdit> = td_edit
                                .edits
                                .iter()
                                .map(|oe| match oe {
                                    lsp_types::OneOf::Left(te) => te.clone(),
                                    lsp_types::OneOf::Right(annotated) => TextEdit {
                                        range: annotated.text_edit.range,
                                        new_text: annotated.text_edit.new_text.clone(),
                                    },
                                })
                                .collect();
                            apply_edits_to_file(
                                client,
                                uri,
                                &text_edits,
                                &mut files,
                                &mut total_edits,
                            )
                            .await?;
                        }
                        DocumentChangeOperation::Op(other) => {
                            tracing::warn!(
                                op = ?other,
                                "workspace edit includes file create/rename/delete; skipped"
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(RenameSummary { files, total_edits })
}

async fn apply_edits_to_file(
    client: &LspClient,
    uri: &Uri,
    edits: &[TextEdit],
    files: &mut Vec<FileChange>,
    total_edits: &mut usize,
) -> Result<(), LspClientError> {
    let path = uri_to_path(uri)?;
    // Hold the docs lock from before the read until after the notify —
    // the read-modify-write plus the version bump must be atomic with
    // respect to concurrent ensure_document_current calls.
    let mut docs = client.opened_documents().lock().await;
    let before = tokio::fs::read_to_string(&path)
        .await
        .map_err(LspClientError::from_io)?;
    let after = apply_text_edits(&before, edits);
    let edit_count = edits.len();
    *total_edits += edit_count;
    tokio::fs::write(&path, &after)
        .await
        .map_err(LspClientError::from_io)?;

    if let Some(doc) = docs.get_mut(uri) {
        doc.version += 1;
        doc.text.clone_from(&after);
        doc.last_used = std::time::Instant::now();
        let version = doc.version;
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: after.clone(),
            }],
        };
        client.notify::<DidChangeTextDocument>(params).await?;
    }
    drop(docs);

    files.push(FileChange {
        path: path.display().to_string(),
        edit_count,
    });
    Ok(())
}

/// Apply a list of [`TextEdit`]s to `text` and return the result.
/// Edits are sorted so later ranges are applied first, ensuring
/// earlier ranges' byte offsets remain valid. Panics are impossible
/// as long as every range refers to valid positions in `text`; if
/// not, the original text is preserved for that edit.
fn apply_text_edits(text: &str, edits: &[TextEdit]) -> String {
    let line_offsets = compute_line_offsets(text);
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by(|a, b| {
        (b.range.start.line, b.range.start.character)
            .cmp(&(a.range.start.line, a.range.start.character))
    });
    let mut out = text.to_string();
    for edit in sorted {
        let Some(start) = byte_offset(&line_offsets, text, edit.range.start) else {
            continue;
        };
        let Some(end) = byte_offset(&line_offsets, text, edit.range.end) else {
            continue;
        };
        if start > end || end > out.len() {
            continue;
        }
        out.replace_range(start..end, &edit.new_text);
    }
    out
}

fn compute_line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

fn byte_offset(line_offsets: &[usize], text: &str, pos: Position) -> Option<usize> {
    let line_start = *line_offsets.get(pos.line as usize)?;
    // LSP position offsets are in UTF-16 code units; walk the text
    // converting char-by-char. Most real-world code is ASCII so the
    // hot path is trivial, but accented identifiers and emoji still
    // have to land at the right byte boundary.
    let mut utf16_count: u32 = 0;
    let rest = &text[line_start..];
    for (byte_idx, ch) in rest.char_indices() {
        if utf16_count == pos.character {
            return Some(line_start + byte_idx);
        }
        utf16_count += u32::try_from(ch.len_utf16()).ok()?;
        if utf16_count > pos.character {
            return Some(line_start + byte_idx + ch.len_utf8());
        }
    }
    // Position is at or past end of line — clamp to end of line.
    if utf16_count <= pos.character {
        Some(line_start + rest.len())
    } else {
        None
    }
}

fn uri_to_path(uri: &Uri) -> Result<std::path::PathBuf, LspClientError> {
    // Route through url::Url so percent-encoding is decoded — servers
    // encode spaces and non-ASCII in returned URIs (`my%20file.rs`),
    // and a naive prefix strip would produce a path that misses on
    // every subsequent I/O. Mirrors `path_to_uri` in client.rs.
    let s = uri.as_str();
    let url =
        url::Url::parse(s).map_err(|e| LspClientError::Uri(format!("invalid URI {s}: {e}")))?;
    url.to_file_path()
        .map_err(|()| LspClientError::Uri(format!("not a file:// URI: {s}")))
}

#[derive(Debug, Clone, Serialize)]
pub struct RenameSummary {
    pub files: Vec<FileChange>,
    pub total_edits: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub path: String,
    pub edit_count: usize,
}

/// Offered code action — flattened for MCP consumers so the agent
/// doesn't have to know about the two `Command | CodeAction`
/// variants LSP allows in the response.
#[derive(Debug, Clone, Serialize)]
pub struct CodeActionInfo {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `true` when the server provided a reason the action can't
    /// currently be executed (e.g. "not applicable here"). Disabled
    /// actions are still listed so the agent can see why they exist
    /// but shouldn't be applied.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_preferred: bool,
}

/// What `apply_code_action` actually did — either applied edits,
/// ran a command, or both. `applied_edit` carries the per-file
/// change counts when edits landed; `ran_command` names the command
/// that was dispatched. When both are `None` the action had no
/// effect (nothing to apply after resolve).
#[derive(Debug, Clone, Serialize)]
pub struct CodeActionApplied {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_edit: Option<RenameSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ran_command: Option<String>,
}

/// List the code actions the server offers for `range` in `path`.
/// Both `CodeAction` and bare `Command` replies are flattened into
/// a single `CodeActionInfo` shape. Returns an empty vec when no
/// actions apply.
pub async fn list_code_actions(
    client: &LspClient,
    path: &Path,
    range: Range,
) -> Result<Vec<CodeActionInfo>, LspClientError> {
    let raw = fetch_code_actions(client, path, range).await?;
    Ok(raw.iter().map(info_from_action).collect())
}

/// Request the code actions, pick the one matching `selector`
/// (title substring, case-insensitive, or exact kind), resolve it
/// if the server returned a lazy stub, then execute its edit
/// and/or command via `apply_workspace_edit` + `workspace/executeCommand`.
/// Returns `None` when no offered action matches the selector.
pub async fn apply_code_action(
    client: &LspClient,
    path: &Path,
    range: Range,
    selector: &CodeActionSelector,
) -> Result<Option<CodeActionApplied>, LspClientError> {
    let actions = fetch_code_actions(client, path, range).await?;
    let Some(picked) = actions.into_iter().find(|a| selector.matches(a)) else {
        return Ok(None);
    };
    let (title, kind, resolved) = match picked {
        CodeActionOrCommand::Command(cmd) => {
            let ran = run_command(client, &cmd).await?;
            return Ok(Some(CodeActionApplied {
                title: cmd.title,
                kind: None,
                applied_edit: None,
                ran_command: Some(ran),
            }));
        }
        CodeActionOrCommand::CodeAction(a) => (a.title.clone(), a.kind.clone(), a),
    };

    if resolved.disabled.is_some() {
        return Err(LspClientError::LspError {
            code: -32_000,
            message: format!(
                "code action {:?} is disabled: {}",
                title,
                resolved
                    .disabled
                    .as_ref()
                    .map(|d| d.reason.as_str())
                    .unwrap_or_default()
            ),
        });
    }

    let resolved = if resolved.edit.is_none() && resolved.command.is_none() {
        client.request::<CodeActionResolveRequest>(resolved).await?
    } else {
        resolved
    };

    let applied_edit = if let Some(edit) = resolved.edit.as_ref() {
        Some(apply_workspace_edit(client, edit).await?)
    } else {
        None
    };

    let ran_command = if let Some(cmd) = resolved.command.as_ref() {
        Some(run_command(client, cmd).await?)
    } else {
        None
    };

    Ok(Some(CodeActionApplied {
        title,
        kind: kind.map(|k| k.as_str().to_string()),
        applied_edit,
        ran_command,
    }))
}

async fn fetch_code_actions(
    client: &LspClient,
    path: &Path,
    range: Range,
) -> Result<Vec<CodeActionOrCommand>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier { uri },
        range,
        context: CodeActionContext::default(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let result: Option<CodeActionResponse> = client.request::<CodeActionRequest>(params).await?;
    Ok(result.unwrap_or_default())
}

fn info_from_action(action: &CodeActionOrCommand) -> CodeActionInfo {
    match action {
        CodeActionOrCommand::Command(cmd) => CodeActionInfo {
            title: cmd.title.clone(),
            kind: None,
            disabled: false,
            disabled_reason: None,
            is_preferred: false,
        },
        CodeActionOrCommand::CodeAction(a) => CodeActionInfo {
            title: a.title.clone(),
            kind: a.kind.as_ref().map(|k| k.as_str().to_string()),
            disabled: a.disabled.is_some(),
            disabled_reason: a.disabled.as_ref().map(|d| d.reason.clone()),
            is_preferred: a.is_preferred.unwrap_or(false),
        },
    }
}

async fn run_command(
    client: &LspClient,
    cmd: &lsp_types::Command,
) -> Result<String, LspClientError> {
    let params = ExecuteCommandParams {
        command: cmd.command.clone(),
        arguments: cmd.arguments.clone().unwrap_or_default(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let _ = client.request::<ExecuteCommand>(params).await?;
    Ok(cmd.command.clone())
}

/// How callers pick one code action out of the offered list.
/// Either the kind (exact match) or a case-insensitive substring
/// of the title wins — first-match in the server's own ordering.
#[derive(Debug, Clone)]
pub enum CodeActionSelector {
    Title(String),
    Kind(String),
}

impl CodeActionSelector {
    fn matches(&self, action: &CodeActionOrCommand) -> bool {
        let (title, kind) = match action {
            CodeActionOrCommand::Command(cmd) => (cmd.title.as_str(), None),
            CodeActionOrCommand::CodeAction(a) => (a.title.as_str(), a.kind.as_ref()),
        };
        match self {
            Self::Title(needle) => title
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase()),
            Self::Kind(k) => kind.is_some_and(|kind| kind.as_str() == k),
        }
    }
}

/// Rust-analyzer-specific: recursively expand the macro at
/// `(line, col)` and return the expansion text plus the macro's
/// display name. Returns `None` when the position is not inside a
/// macro invocation or the server doesn't recognise the method.
pub async fn expand_macro(
    client: &LspClient,
    path: &Path,
    line: u32,
    col: u32,
) -> Result<Option<ExpandedMacro>, LspClientError> {
    ensure_document_current(client, path).await?;
    let uri = path_to_uri(path)?;
    let params = ExpandMacroParams {
        text_document: TextDocumentIdentifier { uri },
        position: Position {
            line,
            character: col,
        },
    };
    let result = client.request::<ExpandMacroRequest>(params).await?;
    Ok(result.map(|r| ExpandedMacro {
        name: r.name,
        expansion: r.expansion,
    }))
}

/// Custom rust-analyzer LSP request for macro expansion.
///
/// Documented at
/// <https://rust-analyzer.github.io/book/contributing/lsp-extensions.html#expand-macro>
/// and emitted by rust-analyzer's "Expand macro recursively" command.
pub enum ExpandMacroRequest {}

impl lsp_types::request::Request for ExpandMacroRequest {
    type Params = ExpandMacroParams;
    type Result = Option<ExpandMacroResult>;
    const METHOD: &'static str = "rust-analyzer/expandMacro";
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExpandMacroParams {
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExpandMacroResult {
    pub name: String,
    pub expansion: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpandedMacro {
    pub name: String,
    pub expansion: String,
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
            enclosing_symbol: None,
        })
        .collect())
}

/// Cap on documents kept open per language server. Past this the
/// least-recently-used document is closed — servers hold per-document
/// state (ASTs, caches), and a long-lived busy workspace would
/// otherwise grow that set without bound.
const MAX_OPEN_DOCS: usize = 128;

async fn ensure_document_current(client: &LspClient, path: &Path) -> Result<(), LspClientError> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(LspClientError::from_io)?;
    let uri = path_to_uri(path)?;
    let language_id = language_id_for(path);

    // The docs lock is held across the notify so concurrent calls
    // cannot interleave their version bumps with out-of-order
    // didChange notifications.
    let mut docs = client.opened_documents().lock().await;
    match docs.get_mut(&uri) {
        Some(entry) if entry.text == text => {
            // Nothing changed.
            entry.last_used = std::time::Instant::now();
            Ok(())
        }
        Some(entry) => {
            entry.version += 1;
            entry.text.clone_from(&text);
            entry.last_used = std::time::Instant::now();
            let version = entry.version;
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
            let evict = if docs.len() >= MAX_OPEN_DOCS {
                docs.iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(uri, _)| uri.clone())
            } else {
                None
            };
            if let Some(old_uri) = evict {
                docs.remove(&old_uri);
                let _ = client
                    .notify::<lsp_types::notification::DidCloseTextDocument>(
                        lsp_types::DidCloseTextDocumentParams {
                            text_document: lsp_types::TextDocumentIdentifier { uri: old_uri },
                        },
                    )
                    .await;
            }
            docs.insert(
                uri.clone(),
                crate::client::OpenedDocument {
                    version: 1,
                    text: text.clone(),
                    last_used: std::time::Instant::now(),
                },
            );
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
        enclosing_symbol: None,
    }
}

fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "file",
        SymbolKind::MODULE => "module",
        SymbolKind::NAMESPACE => "namespace",
        SymbolKind::PACKAGE => "package",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::PROPERTY => "property",
        SymbolKind::FIELD => "field",
        SymbolKind::CONSTRUCTOR => "constructor",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::STRING => "string",
        SymbolKind::NUMBER => "number",
        SymbolKind::BOOLEAN => "boolean",
        SymbolKind::ARRAY => "array",
        SymbolKind::OBJECT => "object",
        SymbolKind::KEY => "key",
        SymbolKind::NULL => "null",
        SymbolKind::ENUM_MEMBER => "enum_member",
        SymbolKind::STRUCT => "struct",
        SymbolKind::EVENT => "event",
        SymbolKind::OPERATOR => "operator",
        SymbolKind::TYPE_PARAMETER => "type_parameter",
        _ => "unknown",
    }
}

fn symbol_node_from_nested(sym: &DocumentSymbol) -> SymbolNode {
    SymbolNode {
        name: sym.name.clone(),
        kind: symbol_kind_name(sym.kind),
        detail: sym.detail.clone(),
        start_line: sym.range.start.line,
        start_col: sym.range.start.character,
        end_line: sym.range.end.line,
        end_col: sym.range.end.character,
        children: sym
            .children
            .as_ref()
            .map(|c| c.iter().map(symbol_node_from_nested).collect())
            .unwrap_or_default(),
    }
}

#[allow(
    deprecated,
    reason = "SymbolInformation.deprecated field is deprecated but present"
)]
fn symbol_node_from_flat(sym: &SymbolInformation) -> SymbolNode {
    SymbolNode {
        name: sym.name.clone(),
        kind: symbol_kind_name(sym.kind),
        detail: sym.container_name.clone(),
        start_line: sym.location.range.start.line,
        start_col: sym.location.range.start.character,
        end_line: sym.location.range.end.line,
        end_col: sym.location.range.end.character,
        children: Vec::new(),
    }
}

#[allow(
    deprecated,
    reason = "SymbolInformation.deprecated field is deprecated but present"
)]
fn workspace_hit_from_flat(sym: &SymbolInformation) -> WorkspaceSymbolHit {
    WorkspaceSymbolHit {
        name: sym.name.clone(),
        kind: symbol_kind_name(sym.kind),
        container: sym.container_name.clone(),
        location: location_hit(&sym.location),
    }
}

fn workspace_hit_from_nested(sym: &WorkspaceSymbol) -> Option<WorkspaceSymbolHit> {
    let lsp_types::OneOf::Left(loc) = &sym.location else {
        // LSP allows a location hint without a range (Right variant) — skip
        // those, since our LocationHit requires a concrete range.
        return None;
    };
    Some(WorkspaceSymbolHit {
        name: sym.name.clone(),
        kind: symbol_kind_name(sym.kind),
        container: sym.container_name.clone(),
        location: location_hit(loc),
    })
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

#[cfg(test)]
mod uri_tests {
    use std::str::FromStr;

    use super::{uri_to_path, Uri};

    #[test]
    fn uri_to_path_decodes_percent_encoding() {
        let uri = Uri::from_str("file:///home/u/my%20project/caf%C3%A9.rs").unwrap();
        let path = uri_to_path(&uri).unwrap();
        assert_eq!(path, std::path::PathBuf::from("/home/u/my project/café.rs"));
    }

    #[test]
    fn uri_to_path_plain_ascii_roundtrip() {
        let uri = Uri::from_str("file:///a/b.rs").unwrap();
        assert_eq!(
            uri_to_path(&uri).unwrap(),
            std::path::PathBuf::from("/a/b.rs")
        );
    }

    #[test]
    fn uri_to_path_rejects_non_file_scheme() {
        let uri = Uri::from_str("https://example.com/a.rs").unwrap();
        assert!(uri_to_path(&uri).is_err());
    }
}

#[cfg(test)]
mod edit_tests {
    use super::{apply_text_edits, Position, TextEdit};
    use lsp_types::Range;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn edit(start: Position, end: Position, new_text: &str) -> TextEdit {
        TextEdit {
            range: Range { start, end },
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn single_edit_on_one_line() {
        let text = "let foo = 1;\nlet bar = 2;\n";
        let edits = vec![edit(pos(0, 4), pos(0, 7), "baz")];
        assert_eq!(
            apply_text_edits(text, &edits),
            "let baz = 1;\nlet bar = 2;\n"
        );
    }

    #[test]
    fn two_edits_on_one_line_applied_right_to_left() {
        // Both edits on the same line — the one earlier in the line
        // must still see its original position after the later one
        // is applied.
        let text = "alpha beta gamma\n";
        let edits = vec![
            edit(pos(0, 0), pos(0, 5), "ALPHA"),
            edit(pos(0, 11), pos(0, 16), "GAMMA"),
        ];
        assert_eq!(apply_text_edits(text, &edits), "ALPHA beta GAMMA\n");
    }

    #[test]
    fn edit_across_lines() {
        let text = "one\ntwo\nthree\n";
        let edits = vec![edit(pos(0, 0), pos(2, 0), "X\n")];
        assert_eq!(apply_text_edits(text, &edits), "X\nthree\n");
    }

    #[test]
    fn insertion_at_end_of_line() {
        // zero-width range at end of line: pure insertion.
        let text = "hello\n";
        let edits = vec![edit(pos(0, 5), pos(0, 5), " world")];
        assert_eq!(apply_text_edits(text, &edits), "hello world\n");
    }

    #[test]
    fn utf16_character_offsets_respect_multibyte_chars() {
        // "α" is 1 UTF-16 code unit but 2 UTF-8 bytes. `char 1` in
        // LSP land is after the alpha; the byte offset must skip 2.
        let text = "αxyz\n";
        let edits = vec![edit(pos(0, 1), pos(0, 2), "_")];
        assert_eq!(apply_text_edits(text, &edits), "α_yz\n");
    }

    #[test]
    fn out_of_range_edit_is_silently_dropped() {
        let text = "abc\n";
        let edits = vec![edit(pos(10, 0), pos(10, 3), "X")];
        assert_eq!(apply_text_edits(text, &edits), "abc\n");
    }
}

#[cfg(test)]
mod symbol_edit_tests {
    use super::{
        find_enclosing_symbol, position_to_byte_offset, splice_range, DocumentSymbol, Position,
        Range, SymbolKind,
    };

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn range(start: Position, end: Position) -> Range {
        Range { start, end }
    }

    fn sym(name: &str, sel: Range, full: Range, children: Vec<DocumentSymbol>) -> DocumentSymbol {
        #[allow(deprecated)]
        DocumentSymbol {
            name: name.to_string(),
            detail: None,
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: full,
            selection_range: sel,
            children: if children.is_empty() {
                None
            } else {
                Some(children)
            },
        }
    }

    #[test]
    fn position_to_byte_handles_ascii_lines() {
        let content = "fn foo() {\n    bar();\n}\n";
        // Start of line 1 ("    bar();") = byte 11
        assert_eq!(position_to_byte_offset(content, pos(1, 0)), Some(11));
        // Column 4 on line 1 ("bar()") = byte 15
        assert_eq!(position_to_byte_offset(content, pos(1, 4)), Some(15));
        // End of file: line 3 col 0
        assert_eq!(position_to_byte_offset(content, pos(3, 0)), Some(24));
    }

    #[test]
    fn position_to_byte_returns_none_past_eof() {
        let content = "a\nb\n";
        assert_eq!(position_to_byte_offset(content, pos(10, 0)), None);
    }

    #[test]
    fn position_to_byte_honours_utf16_columns() {
        // "héllo" — 'é' is one char, 1 UTF-16 unit, 2 UTF-8 bytes.
        // Column 2 (after 'h' + 'é') should land at byte 3.
        let content = "héllo\n";
        assert_eq!(position_to_byte_offset(content, pos(0, 2)), Some(3));
    }

    #[test]
    fn find_enclosing_picks_deepest_match() {
        let outer = sym(
            "Foo",
            range(pos(0, 5), pos(0, 8)),
            range(pos(0, 0), pos(10, 1)),
            vec![sym(
                "bar",
                range(pos(2, 7), pos(2, 10)),
                range(pos(2, 4), pos(4, 5)),
                vec![],
            )],
        );
        // Position inside the inner method's range — expect "bar".
        let (got_range, name) = find_enclosing_symbol(&[outer], pos(3, 8)).unwrap();
        assert_eq!(name, "bar");
        assert_eq!(got_range, range(pos(2, 4), pos(4, 5)));
    }

    #[test]
    fn find_enclosing_falls_back_to_outer_when_no_child_matches() {
        let outer = sym(
            "Foo",
            range(pos(0, 5), pos(0, 8)),
            range(pos(0, 0), pos(10, 1)),
            vec![sym(
                "bar",
                range(pos(2, 7), pos(2, 10)),
                range(pos(2, 4), pos(4, 5)),
                vec![],
            )],
        );
        // Position outside the child's range but inside the outer's.
        let (_, name) = find_enclosing_symbol(&[outer], pos(6, 0)).unwrap();
        assert_eq!(name, "Foo");
    }

    #[test]
    fn splice_range_replaces_inside_content() {
        let content = "alpha beta gamma\n";
        let r = range(pos(0, 6), pos(0, 10));
        let out = splice_range(content, r, "BETA", std::path::Path::new("x")).unwrap();
        assert_eq!(out, "alpha BETA gamma\n");
    }

    #[test]
    fn splice_range_can_delete() {
        let content = "alpha beta gamma\n";
        let r = range(pos(0, 5), pos(0, 10));
        let out = splice_range(content, r, "", std::path::Path::new("x")).unwrap();
        assert_eq!(out, "alpha gamma\n");
    }
}

#[cfg(test)]
mod scip_field_tests {
    //! Wire tests for the `enclosing_symbol` field that the server
    //! enriches after the LSP call returns. The structs themselves
    //! never set the field — `None` is the construction-time
    //! default and must serialize as a missing JSON key so MCP
    //! clients that don't know about the field aren't surprised by
    //! a `"enclosing_symbol":null` either.

    use super::{DiagnosticSnapshot, LocationHit, PublishedDiagnostic};

    #[test]
    fn published_diagnostic_skips_enclosing_symbol_when_none() {
        let d = PublishedDiagnostic {
            severity: "Error".into(),
            line: 0,
            col: 0,
            end_line: 0,
            end_col: 1,
            message: "x".into(),
            source: None,
            enclosing_symbol: None,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert!(!json.as_object().unwrap().contains_key("enclosing_symbol"));
    }

    #[test]
    fn published_diagnostic_emits_enclosing_symbol_when_set() {
        let d = PublishedDiagnostic {
            severity: "Error".into(),
            line: 0,
            col: 0,
            end_line: 0,
            end_col: 1,
            message: "x".into(),
            source: None,
            enclosing_symbol: Some("Foo::process".into()),
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(
            json.as_object().unwrap().get("enclosing_symbol"),
            Some(&serde_json::Value::String("Foo::process".into())),
        );
    }

    #[test]
    fn diagnostic_snapshot_skips_enclosing_symbol_when_none() {
        let s = DiagnosticSnapshot {
            file: "a/b/c.rs".into(),
            line: 0,
            col: 0,
            severity: "Error".into(),
            message: "x".into(),
            source: None,
            enclosing_symbol: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert!(!json.as_object().unwrap().contains_key("enclosing_symbol"));
    }

    #[test]
    fn location_hit_skips_enclosing_symbol_when_none() {
        let h = LocationHit {
            uri: "file:///a/b.rs".into(),
            start_line: 0,
            start_col: 0,
            end_line: 0,
            end_col: 1,
            enclosing_symbol: None,
        };
        let json = serde_json::to_value(&h).unwrap();
        assert!(!json.as_object().unwrap().contains_key("enclosing_symbol"));
    }
}
