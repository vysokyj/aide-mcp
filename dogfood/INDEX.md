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

## Aggregate

- Tasks run: 1
- Aide wins: 0
- Vanilla wins: 1
- Ties: 0
- Mean Δ tool_calls (aide − vanilla): -14
- Mean Δ wall_s (aide − vanilla): -6
- Mean Δ output_kB (aide − vanilla): -40
