# Current paper configuration setup

`paper-current.example.json` lists the paper runtime inputs.
`agent-settings.example.json` shows settings for the three pi lanes. Replace the
paths with the installed application paths. Keep any credentials in the
application's private files. `nivra setup` writes the planner file: `enabled`,
`model_led`, the `agent` settings and the discovery defaults.

## Explicit fresh paper installation

A fresh paper account requires a separate operator mandate and an explicit
starting capital. Preserve the original account and its evidence as an archive.

1. Prepare the pi settings JSON with `mode` set to `paper`. The example
   configures decision, research, and review lanes. Confirm access to each
   configured model through its provider account.
   [Pi lane configuration](../../docs/AGENT.md#prepare-and-verify)
2. Provide a private paper base config with the exchange-verified session date
   and predecessor. For a first book, supply the reviewed account, fee, market
   and dashboard credential paths, history directory, and read quota journal
   path, device, inode, and scope. The source book and session directories can
   be absent. A paper start still requires the bound quota journal.
3. Choose two unused absolute paths with existing parent directories. Keep them
   outside the original config, book, and session directories.
4. Run setup with the reviewed starting capital.

```sh
nivra setup --paper PAPER_BASE_CONFIG \
  --agent-settings AGENT_SETTINGS_JSON \
  --config-directory NEW_CONFIG_DIRECTORY \
  --state-directory NEW_STATE_DIRECTORY \
  --capital PAPER_CAPITAL --confirm-fresh-paper
```

Setup writes private `paper.json`, `planner.json`, and `setup.json` with policy
hashes and `activated:false`. It creates empty state directories. It leaves
the original book and quota journal untouched and does not open credential files.
If setup fails after writing files, retain them for inspection.

Before an authorized start, authenticate the configured providers in the
application's pi directory. The `agent-check` fixture uses the
decision and research settings and runs offline. Run `preflight` and
`readiness` on the prepared installation. Readiness checks local pi files and
the recorded allowance.
It reports `model_roles_unknown` and `agent_provider_not_checked` when provider
authentication was not verified offline. Check authentication in the application's
pi directory. Keep the original service stopped before making the new config
active.

## Continuing the existing account

1. Prepare a new private config directory. Preserve the existing paper config,
   book, session pointer, history, fees, and market quota journal binding.
2. Create a planner config with `enabled` and `model_led` set to `true` and the
   pi settings under `agent`. Keep the reviewed scanner, market feed, and
   admissible tool settings. Use the application's own market and dashboard
   credential files.
3. Run `nivra planner config-check --planner NEW_PLANNER --session SESSION` and
   verify self-check and preflight.
4. Use the reviewed policy transition for an eligible open book. An authorized
   successor uses the configured policy on normal rollover from a sealed book.
   Preserve stopped or halted books and any failed artifacts.
   [Review a planner policy change](../../docs/OPERATIONS.md#reviewing-a-planner-policy-change)

## Configuration compatibility

| Input/path | Current treatment |
| --- | --- |
| Any key outside the pi policy shape (for example a removed model role, owner-task, event, usage or prompt block) | Refused with `planner_config_invalid`. Rewrite the planner file and apply it with `config-apply` |
| Absent `workspace` | No lane workspace tools; sealed policy hash unchanged. Enable with a reviewed `workspace` block and `config-apply` |
| `workspace.network` | Default off and omitted from serialized policy. Explicit opt-in outbound network for allowlisted scanner scripts only; the ranker sandbox refuses it |
| Absent `web_tools` | No lane web tools and no outbound call; sealed policy hash unchanged. Enable with a reviewed `web_tools` block and `config-apply`. See `web-tools.fragment.json` |
| `max_handoff_instruments`, `context` and `tools` | Lane tool and brief bounds; defaults are omitted from serialized policy |
| Dated planner schedule/predecessor fields | Retained as explicit reviewed authority evidence, not a silent active-config conversion |
| Retired dashboard tuning keys | Decode to preserve original config/hash interpretation; omit from new configuration, current React runtime ignores them |
| `prior_config`/cost transition proofs | Only exact reviewed original-binding transitions; never bypass stopped/halted/open-book guards |
| Five-asset release manifest and `/api/status` | Delivery compatibility only; independent of active planner policy. Alias removal requires an installed-updater inventory |

The source fixtures validate the template schemas and pi paper admission without
opening credential files, sockets or an existing book. Host preparation, actual
entitlements, authority validation and operating qualification remain separate
work.
