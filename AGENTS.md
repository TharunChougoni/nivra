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
