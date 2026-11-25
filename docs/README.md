# Documentation

These guides describe the code on `main`. The planned design is in
[direction](DIRECTION.md). Future work is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

| Guide | Read it to |
| --- | --- |
| [Direction](DIRECTION.md) | Understand where Nivra is going and why |
| [Architecture](ARCHITECTURE.md) | Learn the crates, the runtime loop and the durable state |
| [Strategy](STRATEGY.md) | See the paper mandate, the execution gate, fees and the option scope |
| [Agent lanes](AGENT.md) | Configure the decision, gate, research and review lanes, with their triggers, tools and budgets |
| [Research](RESEARCH.md) | Write, sandbox, replay and register strategies |
| [Operations](OPERATIONS.md) | Set up, run, stop, resume, roll over, update and recover a paper book |
| [Configuration](../packaging/rust/CONFIGURATION.md) | Fill in the paper config templates |
| [Market data](MARKET_DATA.md) | Understand the Upstox feed, index context and history download |
| [Dashboard](DASHBOARD.md) | Use the read-only dashboard and its API |
| [Development](DEVELOPMENT.md) | Build, run the gate, keep builds fast and release |
| [Testing](TESTING.md) | Follow the test rules and the test budget |

Reference notes: [fees](reference/KOTAK_FEE_REVIEW.md),
[market sources](reference/MARKET_SOURCE_REVIEW.md),
[shared replay](reference/SHARED_REPLAY.md).
