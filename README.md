# aide-mcp

IDE-grade tools for AI agents, delivered as a pure MCP server in Rust.

aide-mcp closes the gap between agents that work with `grep`/`read` primitives
and the structured code intelligence a real IDE provides. It spawns language
servers, indexes code, and exposes git read operations — all through a single
MCP server over stdio.

## Capabilities (today)

**Project bootstrap**
- `project_detect` — report which supported languages live at a project root.
- `project_setup` — download the LSP server, SCIP indexer, and debug adapter
  binaries for detected languages into `~/.aide/bin/`; idempotent.

**LSP proxy** (keeps a per-workspace language server warm)
- `lsp_hover`, `lsp_definition`, `lsp_references`
- `lsp_document_symbols`, `lsp_workspace_symbols`
- `lsp_diagnostics`

**Git read ops** (libgit2 via `git2`)
- `git_status`, `git_log`, `git_diff`, `git_blame`

## Planned

- `aide-indexer` daemon: builds SCIP indexes post-commit, keyed by commit SHA.
- SCIP query tools (`scip_symbol_at`, semantic diff between commits).
- `exec_run` / `exec_test` / `pkg_install` project lifecycle tools.
- DAP (debug adapter) proxy: set breakpoints, step, inspect variables.

## Supported languages

- **Rust** — rust-analyzer (LSP), scip-rust (SCIP), codelldb (DAP, planned).

Languages are added one at a time via the `LanguagePlugin` trait in
`aide-lang`. Each plugin declares which binaries to fetch, how to run/test,
and how to install packages.

## Quick start

```bash
cargo build --release
./target/release/aide-mcp
```

aide-mcp speaks MCP over stdio. Point an MCP-capable client at the binary,
for example Claude Code:

```json
{
  "mcpServers": {
    "aide": {
      "command": "/absolute/path/to/aide-mcp"
    }
  }
}
```

Then from the agent:

1. Call `project_setup` with the absolute path to your project. This downloads
   rust-analyzer to `~/.aide/bin/rust-analyzer` on first run.
2. Call `lsp_hover`, `git_status`, … as needed.

## Workspace layout

```
crates/
  aide-core/      shared paths + config (~/.aide/ layout)
  aide-install/   binary installer (GitHub releases, gzip)
  aide-lang/      LanguagePlugin trait + per-language specs
  aide-lsp/       stdio LSP client + per-workspace pool + ops
  aide-git/       libgit2-backed read ops
  aide-mcp/       MCP server binary (rmcp 1.5)
```

## Environment

- `AIDE_HOME` — override the root for downloaded tools and state. Defaults to
  `$HOME/.aide`. Used mainly by tests and sandboxed invocations so they do not
  clobber `$HOME`, which rustup/cargo still need.
- `RUST_LOG` — standard `tracing-subscriber` filter. `aide_lsp=debug` shows
  each LSP request/response; `aide_lsp=trace` adds full message bodies.

## MCP SDK

Built on [`rmcp`](https://crates.io/crates/rmcp) 1.5 — the official Anthropic
Rust MCP SDK. stdio transport only for now.

## License

MIT OR Apache-2.0
