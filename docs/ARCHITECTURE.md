# Architecture

Nivra is a Cargo workspace plus a TypeScript pi agent package under `agent/`.
The `nivra` binary runs the paper controller and serves the dashboard. Nivra has
no live order path.

## Crates

| Crate | Owns |
| --- | --- |
| `nivra-core` | Decimal finance, fees, instruments and calendar, quotes, paper execution and replay |
| `nivra-store` | SQLite ledger, durable journals and writer bindings |
| `nivra-cli` | The `nivra` and `nivra-analysis` binaries: controller, scanner, pi lanes, history, dashboard, recovery and updater |
| `nivra-research` | Research jobs, leases, the bubblewrap sandbox worker and result contracts |
| `nivra-sandbox` | Strategy and replay containment API, with no CLI |
| `nivra-tools` | Release assembly, repository checks and offline replay tools |

The paper controller and offline replay share one financial engine,
`nivra-core`. Research cannot place, size or promote a trade.

## Runtime flow

1. The controller resolves the committed session pointer and the original book
   and journal. It fences competing writers and keeps stopped or halted state.
2. It prepares the dated NSE universe from Upstox and the previous session's
   history. Coverage gaps stay explicit. Held positions are protected before
   lower-priority scanning.
3. It rotates quote batches and projects scanner prospects and historical
   features. A scanner observation is not a fill.
4. A deterministic trigger engine starts a pi decision lane turn from eligible
   evidence. Triggers include a fill, position risk, an entry candidate, a
   schedule and the pre-close turn. An optional gate lane screens entry
   candidates first. Rust builds the brief and publishes the lane's tools. Pi
   owns the provider call, conversation, tool loop and usage report.
5. Only a completed turn with a validated proposal reaches the paper plan head.
   Deterministic execution then rechecks identity, held units, cash, fees and
   the original quote receipt, which must be at most 2 seconds old. An accepted
   plan is not a fill.
6. The controller seals the dated book. The review lane runs once after the
   seal, and its redacted text becomes the review memory for the next session.

Model and network work runs outside financial transactions, and protection
keeps running while a model works. A provider, schema or timeout failure ends
that event with no new trade. Corruption or lost writer authority is an
integrity incident.

## Where strategies will run

Today a model proposes each paper trade. The planned design has models write
strategies that a deterministic executor runs on every scan. See
[direction](DIRECTION.md).

## Durable state

| State | Where |
| --- | --- |
| Fills, scanner checkpoints, planner events and plans, halt history, review memory | The original book's SQLite database, in WAL mode with `synchronous=FULL` |
| Original receipts used for execution or protection | The original book's SQLite database, committed with the portfolio and excluded from quote compaction when the market archive is enabled |
| Bulk REST quote bodies and normalized observations | TimescaleDB when configured. The book holds ordered receipt hashes and times. Without the archive, the original quote bodies stay in the book. |
| REST quote batches awaiting archive recovery | The session's bounded `archive-spool.sqlite` |
| Lane trigger batches, turn receipts, plan outcomes, usage and sessions | The book's `model/agent/` directory |
| Inputs, outputs and receipts | Hashed artifacts. Namespace, device and inode pins prevent journal resets. |
| The active dated session | `sessions_root/current.json` |
| Credentials | Private application files, outside Git and model evidence |

Research workers receive registered, hash-pinned inputs. They never get
financial writer ownership or credentials.

## Module map

| Path under `rust/nivra-cli/src/runtime/` | Owns |
| --- | --- |
| `wide/controller.rs` | The dated paper loop |
| `agent/` | The pi agent client, lane runner, trigger engine and tool bridge |
| `harness/` | Lane tool schemas, argument checks, the data-tool executor and the research ranker runner |
| `planner.rs`, `planner/` | Paper policy shape and hash, the trade plan validator and the brief's call-time evidence |
| `redecision.rs` | The durable plan head: claims, settlement and recovery |
| `web_tools/` | Opt-in web search, fetch and news tools for the decision and research lanes |
| `web/` | Dashboard API and the embedded React bundle |

The freshness limit is `nivra_core::market::MAX_FINANCIAL_AGE` (2 seconds).
Quote receipt timestamps are archived unchanged.

The web tools have no broker credential or order path. The research sandbox
keeps the network off unless `workspace.network` enables it. The ranker
sandbox always refuses network.

See [operations](OPERATIONS.md) and [pi agent lanes](AGENT.md).
