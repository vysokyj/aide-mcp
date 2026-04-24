# aide-mcp

**AIDE** — **A**rtificial **I**ntelligence **D**evelopment **E**nvironment.
Contracted from *AI* + *IDE*, dropping the redundant *Integrated* (aide-mcp
is headless — nothing to integrate into a GUI). Also French for *help*, which
fits the role: IDE-grade tools for AI agents, delivered as a pure MCP server
in Rust.

aide-mcp closes the gap between agents that work with `grep`/`read` primitives
and the structured code intelligence a real IDE provides. It spawns language
servers, builds SCIP indexes against committed snapshots, runs and debugs the
project, and exposes git read operations — all through a single MCP server
over stdio.

## Capabilities

**Project bootstrap**
- `project_detect` — report which supported languages live at a project root.
- `project_setup` — download the LSP server, SCIP indexer, and debug adapter
  binaries for detected languages into `~/.aide/bin/`; idempotent.

**LSP proxy** — live working-tree code intelligence (per-workspace server kept warm)
- `lsp_hover`, `lsp_definition`, `lsp_references`
- `lsp_document_symbols`, `lsp_workspace_symbols`
- `lsp_diagnostics`

**SCIP index** — stable snapshots keyed by commit SHA (built in-process; no daemon)
- `index_commit`, `index_status`, `work_last_known_state`
- `scip_documents`, `scip_symbols`, `scip_references`

**Exec** — project lifecycle against the working tree
- `run_project`, `run_tests`, `install_package`, `read_exec_log`

**DAP** — debug adapter proxy over stdio
- `dap_launch`, `dap_terminate`
- `dap_set_breakpoints`, `dap_continue`, `dap_pause`
- `dap_step_over`, `dap_step_in`, `dap_step_out`
- `dap_stack_trace`, `dap_scopes`, `dap_variables`, `dap_evaluate`

**Git read ops** (libgit2 via `git2`)
- `git_status`, `git_log`, `git_diff`, `git_blame`

## Supported languages

- **Rust** — rust-analyzer (LSP), scip-rust (SCIP), codelldb (DAP). All three
  auto-install on `project_setup`.
- **Java / Maven** and **Java / Gradle** — JDT-LS (LSP, auto-installed from
  the Eclipse snapshot tarball via a generated wrapper; Lombok is fetched
  and wired in as a javaagent), scip-java (SCIP; still expects a system
  install via coursier), `java-debug` adapter (DAP).
- **Node.js / TypeScript** — `typescript-language-server` (LSP) bundled
  with a pinned `typescript` runtime via the npm registry;
  `@sourcegraph/scip-typescript` (SCIP). All three auto-install on
  `project_setup`; `node` itself is a system prerequisite. DAP
  (vscode-js-debug) deferred.
- **Python** — `pyright-langserver` (LSP, Microsoft) and
  `@sourcegraph/scip-python` (SCIP), both auto-installed from the npm
  registry. `python3` and `node` are system prerequisites. `pip` /
  `pytest` are invoked through `python3 -m` so they honour whichever
  virtualenv the caller has activated. DAP (`debugpy`) deferred.

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

1. Call `project_setup` with the absolute path to your project. On first run
   this downloads the LSP / SCIP / DAP binaries for each detected language
   into `~/.aide/bin/` (e.g. rust-analyzer + codelldb for a Rust project,
   JDT-LS + Lombok for a Java project).
2. Call `lsp_hover`, `git_status`, … as needed.

## Workspace layout

```
crates/
  aide-core/      shared paths + config (~/.aide/ layout, TOML config)
  aide-install/   binary installer (GitHub releases, gzip/tar.gz/zip)
  aide-lang/      LanguagePlugin trait + built-ins (Rust, Java Maven/Gradle, Node/TS, Python)
  aide-lsp/       stdio LSP client + per-workspace pool + ops
  aide-dap/       Debug Adapter Protocol client over stdio
  aide-git/       libgit2-backed read ops + commit export
  aide-proto/     shared primitives (framing, indexer schema)
  aide-scip/      scip protobuf loader + query helpers
  aide-mcp/       MCP server binary (rmcp 1.5); owns the in-process indexer
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

MIT — see [LICENSE](LICENSE).
