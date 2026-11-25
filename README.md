# Nivra

**Paper trading for Indian markets with model-written strategies.**

[![CI](https://github.com/TharunChougoni/nivra/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/TharunChougoni/nivra/actions/workflows/ci.yml)
[![Rust 1.95](https://img.shields.io/badge/rust-1.95-orange)](rust-toolchain.toml)

Nivra runs a paper book on NSE cash equities and ETFs, plus buy-only Nifty
weekly options. Model lanes research the market and decide what to trade.
Deterministic Rust code validates every action and simulates fills against real
quotes. Live orders are disabled.

## Direction

Nivra is moving to **algorithms first**. Models write and backtest strategies
after the close. A deterministic check accepts the strategies that beat costs
on recorded quotes, and the paper controller runs the active strategy on every
scan. The decision lane supervises, choosing the strategy and handling options
and exceptions. See [direction](docs/DIRECTION.md) and the tracking issue
[#44](https://github.com/TharunChougoni/nivra/issues/44).

Today, a model decision lane still plans each trade. The guides describe that
current behavior.

## How it works

![Nivra architecture](docs/assets/architecture.svg)

- **Controller** (Rust): prepares the universe and history, scans quotes, runs
  the trigger engine, and executes validated paper actions.
- **Model lanes** (pi agent, TypeScript): decision, gate, research and review
  turns, each with its own budget and tools.
- **Execution gate**: identity, cash after fees, held units, and an original
  quote receipt no more than **two seconds** old. No ambiguous retries and no
  broker failover.
- **Research sandbox**: no-network Python strategies, replayed after costs
  through the same fill and fee engine.
- **Dashboard**: authenticated, read-only React workspace.

## Quick start

Requirements: Linux x86_64 and Rust 1.95.0 (pinned). Node 22.19 or later runs
the pi agent. Node 24 builds the dashboard.

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
| `rust/nivra-store` | SQLite ledger and durable journals |
| `rust/nivra-cli` | Controller, scanner, agent lanes, history, dashboard, recovery, updater |
| `rust/nivra-research` | Research jobs, leases and sandbox worker |
| `rust/nivra-sandbox` | Strategy sandbox and replay containment |
| `rust/nivra-tools` | Release assembly and repository checks |
| `agent/` | pi agent package (lanes, prompts, tools bridge) |

Nivra is experimental. Paper fills are simulations and do not show that any
strategy is profitable.
