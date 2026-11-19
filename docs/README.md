# Documentation

These guides describe the current Nivra source snapshot. Historical
investigation notes, dated incident write-ups and completed plans are not kept
here. The initial snapshot is described in [CHANGELOG](../CHANGELOG.md);
future work belongs in [GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

Start with the root [README](../README.md) for the project overview and quick
start.

## Core guides

| Guide | Description |
| --- | --- |
| [Architecture](ARCHITECTURE.md) | Crate boundaries, runtime flow, durable state and lane separation |
| [Operations](OPERATIONS.md) | Configuration, startup, recovery, releases and operator safety procedures |
| [Strategy](STRATEGY.md) | Paper mandate, execution gates, fees and Nifty option scope |
| [Lane tools](LANE_TOOLS.md) | Lane triggers, tool boundary, plan binding, evidence and budgets |
| [pi agent lanes](AGENT.md) | Current pi service: protocol, lanes, tools, budgets, verification and operations |
| [Dashboard](DASHBOARD.md) | Read-only API, React workspace and captured-data projections |
| [Testing](TESTING.md) | Behavioral test strategy and the current gate |
| [Development](DEVELOPMENT.md) | Build, release workflow and immutable-tag policy |
| [Build Performance](BUILD_PERFORMANCE.md) | Portable build guidance and CI shape |

## Setup and operation

| Guide | Description |
| --- | --- |
| [Paper configuration](../packaging/rust/CONFIGURATION.md) | Current setup templates, fresh-install workflow and compatibility inventory |
| [Upstox market feed](UPSTOX_MARKET_FEED.md) | V3 screening feed, index context and fallback behavior |
| [Research worker](RESEARCH_WORKER.md) | Registered research jobs, leases and sandbox containment |
| [Strategy sandbox](STRATEGY_SANDBOX.md) | Scores-only strategies, recorded replay and read-only evidence |
| [Historical candles](HISTORY_DOWNLOAD.md) | Read-only history download, receipts and coverage limits |

## Reference

| Reference | Description |
| --- | --- |
| [Fee research](reference/KOTAK_FEE_REVIEW.md) | Dated public fee components and simulation caveats |
| [Market-source research](reference/MARKET_SOURCE_REVIEW.md) | Dated tick, calendar and source findings |
| [Shared replay](reference/SHARED_REPLAY.md) | Lower-level shared-engine quote replay format |

Repository working rules are in [AGENTS.md](../AGENTS.md).
