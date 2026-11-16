# Prompt inventory

Two prompt layers, both part of the sealed trading-policy hash:

- **System prompt** — the standing role. The host prepends `objective.md`, then
  the role prompt. Under the reviewed owner policy the digest role gets
  `data_owner.md` and the owner decision role gets `orchestrator.md`.
- **Trigger prompt** — the short per-turn user template. Defaults live in
  `orchestrator_model.rs` (`SessionTrigger::default_prompt`) and may be
  overridden per trigger in the planner config. A trigger carries facts, notes,
  the factual digest and the strict output contract; it never replaces the
  standing role.

## Files

| File | Layer | Selected by | Decision |
| --- | --- | --- | --- |
| `objective.md` | system prefix | every model role | keep |
| `data_owner.md` | system | owner-policy digest (Luna) | keep |
| `orchestrator.md` | system | owner-policy decision owner | keep; documents owner research, optional workspace and web tools |
| `data.md` | none | no longer read by the binary | shipped only for the fixed release list |
| `decision.md` | none | no longer read by the binary | shipped only for the fixed release list |
| `supervisor.md` | system fallback | legacy supervisor only; not selected by the owner policy | keep for migration/archive |
| `decision_evidence_digest.md` | none | no longer read by the binary | shipped only for the fixed release list |
| `profit_under_uncertainty.md` | none | no longer read by the binary | shipped only for the fixed release list |
| `review.md` | none | no longer read by the binary | shipped only for the fixed release list |
| `scorer.md` | system | strategy scorer role | keep |
| `operations.md` | none | no longer read by the binary | shipped only for the fixed release list |
| `strategy.md` | none | no longer read by the binary | shipped only for the fixed release list |

The owner policy does **not** append `decision_evidence_digest.md` or
`profit_under_uncertainty.md`; `data_owner.md` and `orchestrator.md` carry their
own evidence rules. The retired post-close `operations` and `strategy` review
roles were the only readers of `review.md`, `operations.md` and `strategy.md`.
Those files stay only because the release bundle and the installed updater
validate the fixed 12-file list. The owner's session review uses its own
compiled prompt.

## Trigger templates

`SessionTrigger::ALL` — `session_open`, `resume`, `change`, `pre_close`,
`post_fill`, `failure_streak`, `idle`,
`decision`, `compaction`, `handoff`. The owner decision turn uses `decision`.
Placeholders are validated against the facts each trigger supplies; an unknown
placeholder fails policy validation instead of rendering empty. The `decision`
template no longer carries a `decision_evidence` placeholder because the owner
digest does not produce that legacy field.

## Contract

- Content hashes bind these files into the sealed policy; the path does not.
- A present `prompts/` directory must be complete. Missing, empty, unreadable,
  oversized or invalid UTF-8 files refuse configuration loading; there is no
  partial-directory fallback.
- Change prompts only through suspend → `config-apply --confirm-paper-review` →
  audited resume. The controller never swaps default files during a turn.
- Releases carry the same files as `default-prompts.json` plus its SHA-256
  asset. The updater validates the fixed file list, non-empty UTF-8 text and the
  per-file size bound before retaining it.
- The runtime embeds these files directly; `setup` installs from this directory.
