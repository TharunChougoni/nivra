# Working rules for Nivra

## Dev mode

Nivra is in active development and trades on paper only. Optimise for speed.
Take reasonable risks: make breaking changes, reshape schemas, replace on-disk
formats and start a fresh paper book when that is faster than staying
compatible. Note a breaking change in `CHANGELOG.md` and give the operator
the command or step that moves an installed host forward. Old journals,
books and artifacts are useful history, not untouchable evidence. Move or
delete them when they get in the way, after the owner agrees for anything on
the paper host.

Read [README.md](README.md) and the [documentation index](docs/README.md)
when you need context. Track future work in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues) and update a
guide when you change the behaviour it describes.

## The two hard rules

- **Paper only.** No live orders and no live broker activation without a
  separate, explicit live mandate from the repository owner.
- **Secrets stay private.** Never print, log or commit tokens, `.env*`, auth
  state, account payloads or `data/`. Host inventory (addresses, SSH targets,
  service and config paths) lives only in the git-ignored `.vm/` directory.
  Use only the application's own credentials.

## Design defaults

These describe how Nivra works today. Change them when a better design needs
it, and update the guide that describes them.

- Registered strategies generate paper actions; models research, write and
  select strategies ([direction](docs/DIRECTION.md)).
- Deterministic code owns validation, cash, fees, fills and protection.
- Executable quotes must be fresh (`MAX_FINANCIAL_AGE`, 2 s) and keep their
  original receipt time.
- Never invent instrument identifiers, lots, holidays or prices.

## Configuration follows the owner's intent

Most behaviour is switched on in config, not in code. A feature that ships
off in config has not shipped. When the owner asks for a behaviour:

1. Find every switch that gates it: the paper config, the planner, the agent
   settings, systemd units and environment. Search the code for the setting
   that guards the path (for example `strategy_execution.enabled`,
   `history_preparation_enabled`, `candidate_compute.strategy`, `research_env`,
   `market_archive`). A default of off in code is not the owner's choice.
2. Change the live config on the paper host to match, using the documented
   procedure: write a new file, run `nivra planner config-check`, apply with
   `config-apply`, keep a backup named `*.pre-<reason>`, and record the change
   in `.vm/`.
3. Verify the behaviour on the host, not in the file: read the dashboard or
   `/api/v1/summary` and confirm the feature reports itself active. Say which
   keys you changed.
4. When you add a setting, add it to `packaging/rust/CONFIGURATION.md`, the
   example configs and the changelog upgrade steps in the same change. If the
   owner wants the feature now, turn it on in the host config in the same
   deploy.

If a request conflicts with a guard such as the acceptance thresholds, do what
the owner asked, keep the guard visible (label fallbacks, log the override),
and say so.

## Code and verification

The application is a Cargo workspace. The pi agent is a separate TypeScript
package under `agent/`. Commit and push directly to `main`. Use a branch or
worktree when isolation helps, and delete it after it lands. Create release
tags only when asked.

Before pushing, run the gate. It takes about a minute when `target/` is warm:

```sh
cargo fmt --all
cargo build --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Run `target/debug/nivra-tools test-budget` when you add tests. Raise the
limit if a change needs more tests. Integration tests may use local services
such as a Postgres container. Mark them `#[ignore]` so `cargo test` stays fast
and self-contained, and document how to run them.

Build speed:

- Use all cores. Keep `target/` and don't `cargo clean`.
- Skip `cargo run -p nivra-tools`, which recompiles with different features.
  Run the binary that is already built.
- Build musl only for a release.

### Codebase structure

The workspace is layered. A crate may depend only on crates below it:

| Crate | Owns | Depends on |
| --- | --- | --- |
| `nivra-core` | Domain types and pure rules: money, instruments, execution, fees, strategy contracts | nothing in the workspace |
| `nivra-store` | Persistence: SQLite paper books, the TimescaleDB market archive, research job queues | core |
| `nivra-sandbox` | Bounded Python execution and the pinned research environment | core |
| `nivra-research` | The research worker: archive bar jobs and sandboxed runs | core, store, sandbox |
| `nivra-cli` | The `nivra` binary: controller, lanes, dashboard, updater, commands | all of the above |
| `nivra-tools` | Developer tools: release checks, test budget, benches | all of the above |

- Put a rule in the lowest crate that can own it. Domain logic goes in
  `nivra-core` as pure functions. I/O stays in `nivra-store` or at the edge in
  `nivra-cli`.
- In `nivra-cli/src/runtime/`, one directory per area (`wide/`, `agent/`,
  `market_archive/`, `harness/`, `web/`). `area.rs` holds the public surface,
  `area/*.rs` the parts. Split by responsibility so a typical change touches
  one file. Treat about 800 lines as the point to split.
- Tests sit next to the code: `area/tests.rs`, or `area/tests/*.rs` named for
  what they test. Integration tests that need a service are `#[ignore]`d.
- Market data lives only in the TimescaleDB archive. Paper books hold ledger
  events and archive references, not bulk quote bodies.
- The dashboard source is `rust/nivra-cli/src/runtime/web/v2/src/`. Text for
  the owner lives in `labels.ts` in plain language.

### Making changes

- Name the data shape first, then write the logic. Parse external data at the
  boundary and pass typed values inside.
- Make the smallest change that solves the problem. Delete instead of
  deprecating: migrate every caller and remove the old path in the same
  change. No compatibility shims in dev mode.
- For a bug, write the failing test first, then the fix. Test behaviour a user
  or the book can observe, not internals. Wall-clock assertions are only
  regression bounds with generous limits.
- No `cfg!(test)` branches or test-only bypasses in production code. No
  swallowed errors: return a snake_case failure code.
- Do not grow a function's argument list past about seven; group the values
  into a struct. Collapse one-caller pass-through wrappers.
- Update the guide that describes the behaviour, the changelog with upgrade
  steps, and `rust/nivra-tools/retired-names.txt` when a retired name goes.
- Commit in small units that each pass the gate. Merge parallel work one
  branch at a time and run the gate after each.

### Dashboard frontend

After frontend changes, build the bundle and commit source, lockfile and
bundle together:

```sh
npm ci --prefix rust/nivra-cli/src/runtime/web/v2
npm run build --prefix rust/nivra-cli/src/runtime/web/v2
npm test --prefix rust/nivra-cli/src/runtime/web/v2
```

### Pi agent

After changes under `agent/`:

```sh
npm ci --prefix agent
npm run build --prefix agent
npm test --prefix agent
```

Commit `agent/src`, `agent/test` and the lockfile. `agent/dist` is built on
the run host and stays out of Git.

## Releases

A push to `main` does not deploy. When the owner asks for a release, the
version in `Cargo.toml`, the changelog heading and the annotated tag `vX.Y.Z`
must match on one commit. Never move a published tag. Ship fixes as the next
patch. See [development](docs/DEVELOPMENT.md).
