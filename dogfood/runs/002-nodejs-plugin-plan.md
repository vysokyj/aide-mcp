# Task 002: Node.js/TypeScript plugin plan (4th attempt)

Date: 2026-04-24
Slug: `nodejs-plugin-plan`

## Prompt (identical for both agents)

```
Investigate what it would take to add Node.js/TypeScript language support to aide-mcp. Identify:

(1) the LanguagePlugin trait contract and where plugins are registered,
(2) how existing language plugins (Rust, Java) are structured — files touched, tools pinned, toolchain detection,
(3) which LSP server(s) and SCIP indexer would be needed for Node/TS and how they'd be pinned per the "tool binaries are always pinned" invariant,
(4) concrete list of files to create/modify and their responsibilities.

Read-only research — do not write any code. Return a concise plan that a follow-up implementation task could execute from. Cite every claim as [path:line]. End with the required ```metrics block.
```

## Ground truth

Spot-checked citations on aide's output:

- `crates/aide-lang/src/plugin.rs:66` — `pub trait LanguagePlugin: Send + Sync {` ✓
- `crates/aide-lsp/src/ops.rs:982` — `fn language_id_for(path)` maps ts/tsx/js/jsx ✓

Both agents converged on the same plan again: one new
`crates/aide-lang/src/languages/node.rs` (`NodePlugin`), add `pub mod node;` to
`languages/mod.rs`, push `Arc::new(NodePlugin)` into `Registry::builtin()`.
`Source::DirectUrl` + `ArchiveFormat::TarGz` + `custom_install` (JDT-LS
pattern) for three pinned npm tarballs: `typescript-language-server`,
`typescript`, `scip-typescript`. `dap() -> None`. Node runtime on `$PATH`.

## Metrics

|                 | vanilla | aide  |
|-----------------|---------|-------|
| tool_calls      |  27     |  27   |
| aide_calls      |  —      |   9   |
| fallback_calls  |  —      |  18   |
| rule_violations |  —      |   0   |
| wall_s_measured | 196     | 1178  |
| output_kB_est   |  95     |  70   |
| false_leads     |   0     |   0   |
| correct         |   ✓     |   ✓   |
| completeness    |   5     |   5   |
| confidence      |  high   | high  |

## Vanilla result (summary)

Full plan with 27-step tool trail, every claim cited. Names `NodePlugin`,
recommends `package.json` detection, reuses `install_jdtls_wrapper` as
template for the npm-tarball wrapper. Calls out that `extract_tar_gz`
already handles npm tarballs (no new `ArchiveFormat` variant needed) and
that `aide-lsp/src/ops.rs:982-991` already maps TS/JS extensions.

## Aide result (summary)

Same plan, equally correct. Tool trail explicitly labels each call `[aide]`
vs `Read(...)`: 9 `project_*` calls (`project_detect`, `project_ls×2`,
`project_grep×6`) backed by 18 `Read` calls. Adds an "Aide value note"
section explaining which aide tool paid off (SCIP-tagged enclosing-symbol
hits from `project_grep` for the eight `registry.detect` callsites).
Matches vanilla on risk flagging (tsserver path, scip-typescript CLI
shape).

## Verdict

**Winner:** vanilla
**Reason:** Both plans are correct and equivalently complete. Same tool
call count (27). But aide's wall-clock was 6× vanilla (1178 s vs 196 s)
for equivalent output. Aide finally demonstrated proper compliance
(non-zero aide usage, rule_violations metric present, Coverage gaps
section present, labelled tool trail), but the speed cost on an
architecture-mapping task is steep. Vanilla wins on efficiency; aide wins
on compliance recovery vs run 001.

**Delta:** `aide − vanilla: ΔT=0 calls, ΔW=+982 s, ΔB=-25 kB`
(aide same call count, dramatically slower wall clock, slightly leaner output)

## Follow-up change

- Commit: `none` (research-only)
- Files touched: none

## Coverage gaps (from aide agent)

- none — every non-aide call had an aide alternative the agent should
  have used (and this run used the aide alternatives).

## Notes

### Compliance — run 001 lessons applied

All three compliance gaps from run 001 are closed in this run:
- `rule_violations: 0` is physically present in the metrics block.
- `## Coverage gaps` section is present (with explicit "none" formulation).
- Tool trail has `[aide]` / `Read(...)` prefixes on every entry.

Agent also added an "Aide value note" justifying *why* aide tools were the
right choice for specific sub-questions — this is exactly the kind of
post-hoc reasoning the benchmark wants to surface.

### The wall-clock surprise

Aide agent self-estimated 90 s but the measured wall-clock was 1178 s —
a 13× underestimate. Vanilla also underestimated (240 s estimate vs 196 s
measured) but in the other direction and much less dramatically. Either:
- `project_grep` / `project_ls` are materially slower per-call than
  `Grep` / `Glob` (LSP / SCIP round-trips), or
- the agent's wall-clock estimate is wildly decoupled from actual
  execution time.

This delta is worth investigating before the next run — possibly add
`read_exec_log` sampling to the aide agent prompt so it reports *measured*
per-call durations rather than estimates.

### First aide attempt was interrupted

The first `dogfood-aide` spawn in this session appeared to hang (no visible
progress) and the user interrupted it. Retry worked cleanly. Possible
explanations: cold rust-analyzer / SCIP indexer on a fresh session, a
long-running aide tool blocking the agent's first turn, or genuine agent
stall. The vanilla agent in the same parallel pair completed in ~196 s
uneventfully, so the stall was aide-side. The ~1178 s retry duration is
long but *not* a stall — every tool call produced progress.

### Task-shape implication (unchanged from run 001)

Architecture-mapping tasks ("read five files and produce a plan") are
still not where aide shines even when the agent uses aide tools properly.
The substantive win aide had — SCIP-tagged enclosing symbols from
`project_grep` on `registry.detect` callsites — was a small multiplier on
a small sub-question. For the next run the skill should pick a task where
aide is *designed* to win: symbol-tracing, cross-file reference walks
(`lsp_references` / `scip_references`), or diagnostics-driven
investigations (`lsp_diagnostics`).
