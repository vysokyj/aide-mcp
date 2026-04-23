//! Higher-level LSP operations mapped onto MCP tools.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use lsp_types::notification::{DidChangeTextDocument, DidOpenTextDocument};
use lsp_types::request::{
    CodeActionRequest, CodeActionResolveRequest, DocumentSymbolRequest, ExecuteCommand,
    GotoDefinition, HoverRequest, References, Rename, WorkspaceSymbolRequest,
};
use lsp_types::{
    CodeActionContext, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentChangeOperation,
    DocumentChanges, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    ExecuteCommandParams, GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams,
    MarkedString, PartialResultParams, Position, Range, ReferenceContext, ReferenceParams,
    RenameParams, SymbolInformation, SymbolKind, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit, Uri,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceEdit, WorkspaceSymbol,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
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

    let all_paths: Vec<std::path::PathBuf> = std::iter::once(path.to_path_buf())
        .chain(related_paths.iter().cloned())
        .collect();
    let before = snapshot_published_diagnostics(client, &all_paths).await?;

    let after_contents = before_contents.replacen(old_string, new_string, 1);
    tokio::fs::write(path, &after_contents)
        .await
        .map_err(LspClientError::from_io)?;

    let uri = path_to_uri(path)?;
    let mut docs = client.opened_documents().lock().await;
    if let Some(doc) = docs.get_mut(&uri) {
        doc.version += 1;
        doc.text.clone_from(&after_contents);
        let version = doc.version;
        drop(docs);
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
    } else {
        drop(docs);
        // File wasn't open yet; ensure_document_current at next read.
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
    let before = tokio::fs::read_to_string(&path)
        .await
        .map_err(LspClientError::from_io)?;
    let after = apply_text_edits(&before, edits);
    let edit_count = edits.len();
    *total_edits += edit_count;
    tokio::fs::write(&path, &after)
        .await
        .map_err(LspClientError::from_io)?;

    let mut docs = client.opened_documents().lock().await;
    if let Some(doc) = docs.get_mut(uri) {
        doc.version += 1;
        doc.text.clone_from(&after);
        let version = doc.version;
        drop(docs);
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
    let s = uri.as_str();
    let stripped = s
        .strip_prefix("file://")
        .ok_or_else(|| LspClientError::Uri(format!("not a file:// URI: {s}")))?;
    Ok(std::path::PathBuf::from(stripped))
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
