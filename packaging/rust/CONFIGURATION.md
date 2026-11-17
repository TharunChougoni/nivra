# Current paper configuration setup

These templates describe source configuration, not an installed session. They
contain no credentials and must never be copied over active or original config.
Current setup uses `paper-current.example.json` and
`planner-current.example.json`. The retired dated shell installer is not shipped.
A sanitized legacy delivery fixture lives under `fixtures/rust/` for tests only;
its home and filesystem identifiers are synthetic. Account/scope aliases and
paper-policy values are retained solely for legacy validator compatibility,
not as broker account identifiers, balances or an operational configuration.

## Explicit fresh paper installation

An ordinary update continues the existing account. A fresh account requires a
separate explicit operator mandate, including its new capital and preservation
of the original account as an archive. It is not a migration or an updater retry.
Use the current Rust command:

```sh
nivra setup --paper ORIGINAL_PAPER_CONFIG \
  --digest-model-config APP_DIGEST_MODEL_CONFIG \
  --owner-model-config APP_OWNER_MODEL_CONFIG \
  --config-directory NEW_CONFIG_DIRECTORY \
  --state-directory NEW_STATE_DIRECTORY \
  --capital PAPER_CAPITAL --confirm-fresh-paper
```

Replace `PAPER_CAPITAL` with the reviewed paper capital.

Both output paths must be absolute, absent, outside original config/book/session
namespaces, and have existing parents. The source config must be private and
contain the verified target session/predecessor pair. Setup retains its exact
account/broker identity, market quota journal identity, fees, market/dashboard
credentials, history and public-host/alert bindings. It never opens credentials,
journals or books. The explicit model paths bind the current template's Luna
Digest and Grok owner; setup admits no alternative model profile. Verify their
exact-model entitlement with readiness before any later paper activation.

Setup writes private `paper.json`, `planner.json`, packaged prompts and `setup.json`
with source/policy hashes and `activated:false`. It prepares empty state/session
directories without creating a financial book or model usage namespace. Only
current owner planning is configured; independent Decide, legacy handoff gates,
post-close roles and predecessor policy schedules are not imported. A failed or
existing output refuses; retain its artifacts and inspect it instead of retrying
over it. Setup does not stop services, switch deployment pointers or install units.

Stop and disable the original paper service before making the prepared config
active. Preserve all old config, books, failed artifacts and release evidence.
Install the verified release with the existing updater against the original
stopped book, using its operator-stopped guards. Then point the canonical paper
unit and updater's `paper_config` at the new stable `paper.json`; leave the paper
service stopped. Future releases use that same updater/config and never rerun
setup. Starting the fresh account is a separate paper-session action; setup and
installation do not authorize it. Do not change the shared quota journal or grant
approvals to make readiness pass.

## Continuing the existing account

1. Prepare a new private configuration directory outside every original book.
   Copy both examples to new filenames. Copy this revision's complete `prompts/`
   beside `planner.json`. Do not replace an existing prompt directory. The full
   directory includes historical prompts for decoding; owner Digest uses
   `data_owner.md`, and only the persistent Orchestrator owns decisions.
2. Replace `${CONFIG_DIR}` with that directory's absolute path. Replace every
   `${ORIGINAL_*}` from reviewed original config/book evidence. Device/inode
   placeholders must become JSON integers, not strings. Retain the existing
   account, broker read-scope, journal, book, session/predecessor, sessions root
   and history bindings. Dates must match the verified exchange calendar.
   Retain original configured capital; actual continuing cash/holdings/fees come
   from the original ledger. Never initialize a replacement account as part of
   continuation; the explicit fresh-account workflow above is a separate mandate.
3. Replace `/REVIEWED/APP_*` paths with this application's own private model,
   market and dashboard grant locations. These are placeholders, not paths to
   discover by reading other applications' caches. Review exact-model approvals
   and provider entitlement separately; templates do not prove either.
4. Review all allowances, scanner/feed bounds, costs, tools and timeouts. The
   example costs match `paper-costs.fragment.json`; they are a proposed policy,
   not permission to change the original book's fees. Owner defaults to the
   explicit Grok profile; Sol is selectable only after its own binding/approval
   is reviewed. No automatic fallback exists. No price/cost estimate is invented
   for a model whose pricing evidence is absent.
5. Validate the completed planner offline with
   `nivra planner config-check --planner NEW_PLANNER --session SESSION`.
   Review original authority, self-check and preflight separately before
   applying. These templates neither apply policy nor start, resume, unhalt,
   reset usage or roll over a book.
6. Choose disposition explicitly: retain archived books read-only with original
   configs, or continue the original book through reviewed `config-apply`.
   Preserve halted/stopped state. Keep predecessor configs, journals, failed
   artifacts and immutable release rollback evidence unchanged.
   Any installation or paper start needs the operator's separate authorization.

## Compatibility inventory

| Input/path | Current treatment |
| --- | --- |
| `decision_owner`, `decide` or `handoff_gate` key | Removed; the persistent orchestrator is the only decision owner. A planner file that still carries one refuses with `planner_config_invalid` until it is rewritten and applied with `config-apply` |
| Absent `workspace` | No owner workspace tools; sealed policy hash unchanged. Enable with a reviewed `workspace` block and `config-apply` |
| `workspace.network` | Default off and omitted from serialized policy. Explicit opt-in outbound network for allowlisted scanner scripts only; the scorer sandbox refuses it |
| Absent `web_tools` | No orchestrator web tools and no outbound call; sealed policy hash unchanged. Enable with a reviewed `web_tools` block and `config-apply`. See `web-tools.fragment.json` |
| Enabled Operations/Strategy/post-close | Refused under owner policy; historical records remain readable |
| Orchestrator plus old review-role blocks | Refused as `orchestrator_and_legacy_roles_conflict`; do not grow compatibility shims to reuse a clashing config |
| Old `max_handoff_instruments`/context schema | Retained for sealed historical policy and tool bounds; new template uses factual tools-only defaults and no handoff gate |
| Dated planner schedule/predecessor fields | Retained as explicit reviewed authority evidence, not a silent active-config conversion |
| Retired dashboard tuning keys | Decode to preserve original config/hash interpretation; omit from new configuration, current React runtime ignores them |
| `prior_config`/cost transition proofs | Only exact reviewed original-binding transitions; never bypass stopped/halted/open-book guards |
| Five-asset release manifest and `/api/status` | Delivery compatibility only; independent of active planner policy. Alias removal requires an installed-updater inventory |

The source fixtures validate the template schemas and owner admission without
opening credential files, sockets or an existing book. Host preparation, actual
entitlements, authority validation and operating qualification remain separate
work.
