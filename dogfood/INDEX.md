# Dogfood log — aide-mcp vs vanilla

Running tally of paired agent experiments. Vanilla = no `mcp__aide__*`.
Aide = preferring `mcp__aide__*`. Vanilla is authoritative for code
changes; aide is reference-only for scoring. See
[SKILL.md](../.claude/skills/dogfood/SKILL.md) for the workflow and
[TEMPLATE.md](TEMPLATE.md) for per-run record shape.

## Runs

| #   | Slug | Vanilla calls / s / kB | Aide calls / s / kB | Verdict | Notes |
|-----|------|------------------------|----------------------|---------|-------|
| 001 | [nodejs-plugin-plan](runs/001-nodejs-plugin-plan.md) | 28 / 158 / 95 | 14 / 152 / 55 | vanilla (agent failure) | Aide agent used 0 aide calls, omitted `rule_violations` + Coverage gaps. Architecture-mapping task doesn't fit aide — but skipping aide entirely is not the intent. |
| 002 | [nodejs-plugin-plan](runs/002-nodejs-plugin-plan.md) | 27 / 196 / 95 | 27 / 1178 / 70 | vanilla (efficiency) | Aide compliance recovered (9 aide calls, rule_violations=0, Coverage gaps present) but wall-clock was 6× vanilla for equivalent output. First aide spawn of the session stalled and was interrupted — retry completed cleanly. |
| 003 | [aidepaths-blast-radius](runs/003-aidepaths-blast-radius.md) | 11 / 55 / 10 | 14 / 75 / 8 | tie | Both correct. Aide used `scip_callers` + `impact_of_change` to answer (1)–(3) in one call pair; edge on (4) via explicit env-var isolation. Vanilla 3 fewer calls, 20 s faster. First aide spawn stalled on permission prompts — fixed by replacing 9 individual `mcp__aide__*` allow entries with the server wildcard `mcp__aide`. |

## Aggregate

- Tasks run: 3
- Aide wins: 0
- Vanilla wins: 2
- Ties: 1
- Mean Δ tool_calls (aide − vanilla): -3.7
- Mean Δ wall_s (aide − vanilla): +332
- Mean Δ output_kB (aide − vanilla): -22.3
