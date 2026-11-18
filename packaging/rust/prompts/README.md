# Prompt inventory

The live paper controller runs pi lanes, which read their own prompts from
`agent/prompts/decision.md`, `agent/prompts/research.md` and
`agent/prompts/review.md` in the built agent package (or from the directory in
`planner.agent.prompts`). A pi paper planner does not load this directory: a
sibling `prompts/` folder is ignored and its bytes do not enter the sealed policy.

This directory is the retained prompt set for the earlier Rust planner. That
planner is not on the live path; the retained backend still produces digest turns
for offline `paper-replay` and for archived-policy evidence. Its prompt layers
are:

- **System prompt** — the standing role. The host prepends `objective.md`, then
  the role prompt. The retained digest role gets `data_owner.md` and the retained
  owner decision role gets `orchestrator.md`.
- **Trigger prompt** — the short per-turn user template. Defaults live in
  `orchestrator_model.rs` (`SessionTrigger::default_prompt`) and may be
  overridden per trigger in the legacy planner config. A trigger carries facts,
  notes, the factual digest and the strict output contract; it never replaces the
  standing role.

## Files

| File | Layer | Selected by | Decision |
| --- | --- | --- | --- |
| `objective.md` | system prefix | every retained planner role | keep |
| `data_owner.md` | system | retained digest role (formerly Luna) | keep for offline replay/archive |
| `orchestrator.md` | system | retained owner decision role | keep for offline replay/archive |
| `data.md` | none | no longer read by either backend | shipped only for the fixed release list |
| `decision.md` | none | no longer read by either backend | shipped only for the fixed release list |
| `supervisor.md` | system fallback | retained supervisor role; not selected by the owner policy | keep for migration/archive |
| `decision_evidence_digest.md` | none | no longer read by either backend | shipped only for the fixed release list |
| `profit_under_uncertainty.md` | none | no longer read by either backend | shipped only for the fixed release list |
| `review.md` | none | no longer read by either backend | shipped only for the fixed release list |
| `scorer.md` | system | retained strategy scorer role | keep |
| `operations.md` | none | no longer read by either backend | shipped only for the fixed release list |
| `strategy.md` | none | no longer read by either backend | shipped only for the fixed release list |

The retained owner policy does **not** append `decision_evidence_digest.md` or
`profit_under_uncertainty.md`; `data_owner.md` and `orchestrator.md` carry their
own evidence rules. The retired post-close `operations` and `strategy` review
roles were the only readers of `review.md`, `operations.md` and `strategy.md`.
Those files stay only because the release bundle and the installed updater
validate the fixed 12-file list. A pi paper planner needs none of these files.

## Trigger templates

`SessionTrigger::ALL` — `session_open`, `resume`, `change`, `pre_close`,
`post_fill`, `failure_streak`, `idle`,
`decision`, `compaction`, `handoff`. The retained owner decision turn uses
`decision`. Placeholders are validated against the facts each trigger supplies;
an unknown placeholder fails policy validation instead of rendering empty. These
templates apply to the retained legacy planner only; pi lanes use their own lane
prompts and the Rust-built brief.

## Contract

- For the retained legacy planner, content hashes bind these files into the
  sealed policy; the path does not. Live pi lanes record each lane prompt's
  SHA-256 in the turn journal instead of sealing the text into policy.
- A present `prompts/` directory beside a legacy planner JSON must be complete.
  Missing, empty, unreadable, oversized or invalid UTF-8 files refuse
  configuration loading; there is no partial-directory fallback.
- Legacy prompt changes use suspend → `config-apply --confirm-paper-review` →
  audited resume. The controller never swaps default files during a turn.
- Releases still carry these files as `default-prompts.json` plus its SHA-256
  asset. The updater validates the fixed file list, non-empty UTF-8 text and the
  per-file size bound before retaining it in the immutable release directory.
- The binary embeds `objective.md`, `data_owner.md`, `orchestrator.md`,
  `supervisor.md` and `scorer.md` for the retained backend. `setup` does not copy
  this directory; a pi paper configuration does not use it.
