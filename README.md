# Nivra

**Paper trading for Indian markets with model-written strategies.**

[![CI](https://github.com/TharunChougoni/nivra/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/TharunChougoni/nivra/actions/workflows/ci.yml)
[![Rust 1.95](https://img.shields.io/badge/rust-1.95-orange)](rust-toolchain.toml)

Nivra runs a paper book on NSE cash equities and ETFs, plus buy-only Nifty
weekly options. Model lanes research the market and decide what to trade.
Deterministic Rust code validates every action and simulates fills against real
quotes. Live orders are disabled.

## Direction

Nivra runs **algorithms first** when `strategy_execution` is enabled: models
research, write and backtest strategies after the close, a deterministic check
accepts and registers strategies that beat costs on recorded quotes, and the
paper controller executes the active strategy on eligible quote updates while the decision
lane supervises and handles options and exceptions. When `strategy_execution`
is off or no strategy is active, the decision lane plans each trade. See
[direction](docs/DIRECTION.md) and [#44](https://github.com/TharunChougoni/nivra/issues/44).

## How it works

![Nivra architecture](docs/assets/architecture.svg)

- **Controller** (Rust): prepares the universe and history, scans quotes, runs
  the trigger engine, and executes validated paper actions.
- **Model lanes** (pi agent, TypeScript): decision, gate, research and review
  turns, each with its own budget and tools.
- **Market archive**: TimescaleDB keeps the original candle and quote
  receipts across sessions. Candle history, pre-open preparation, daily
  research and past-session charts read from it.
- **Execution gate**: identity, cash after fees, held units, and an original
  quote receipt no more than **two seconds** old. No ambiguous retries and no
  broker failover.
- **Research sandbox**: no-network Python strategies, replayed after costs
  through the same fill and fee engine.
- **Dashboard**: authenticated, read-only React workspace.

## Quick start

Requirements: Linux x86_64 and Rust 1.95.0 (pinned). Node 22.19 or later runs
the pi agent. Node 24 builds the dashboard. A paper book that needs candle
history, pre-open preparation, daily research or past-session charts also
needs a PostgreSQL database with the TimescaleDB extension.

```sh
git clone https://github.com/TharunChougoni/nivra.git
cd nivra
cargo build --workspace --all-targets --locked
target/debug/nivra self-check
```

These are offline checks. To run a paper book, follow
[configuration](packaging/rust/CONFIGURATION.md) and
[operations](docs/OPERATIONS.md).

## Documentation

Start at the [documentation index](docs/README.md). The working rules for
contributors and agents are in [AGENTS.md](AGENTS.md).

## Repository layout

| Path | Responsibility |
| --- | --- |
| `rust/nivra-core` | Decimal finance, fees, instruments and calendar, quotes, paper execution, replay |
| `rust/nivra-store` | SQLite ledger, durable journals and the TimescaleDB market archive |
| `rust/nivra-cli` | Controller, scanner, agent lanes, history, dashboard, recovery, updater |
| `rust/nivra-research` | Research jobs, leases and sandbox worker |
| `rust/nivra-sandbox` | Strategy sandbox and replay containment |
| `rust/nivra-tools` | Release assembly and repository checks |
| `agent/` | pi agent package (lanes, prompts, tools bridge) |

Nivra is experimental. Paper fills are simulations and do not show that any
strategy is profitable.
