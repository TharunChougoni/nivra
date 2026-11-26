---
name: backtest-methodology
description: "Backtest discipline for Nivra research: quote tape vs bar backtests, train/OOS splits, walk-forward checks, costs always on, lookahead/survivorship avoidance, and trial-count control."
---

# Backtest Methodology

## Quote Tape vs Bar Backtests

- **Action strategies (`strategy_actions_v1`)**: Use `backtest` with `strategy: true` over an `archive_window` dataset. This replays real recorded depth quotes (`quotes`), enforces the 2 s freshness gate, whole shares, ticks, depth consumption, and `paper_costs`, and returns net P&L and acceptance verdicts. Use `quotes` when you need to inspect raw archived receipt coverage for specific keys.
- **Ranker scripts (`scripts/<name>.py`)**: Use `run` on small inline feature JSON to verify finite outputs, and `backtest` (`strategy` false/omitted) for ranking diagnostics. Ranker forward marks are hypothetical returns, not executable fills.
- If a strategy replay hits `strategy_replay_wall_budget` or tape caps, narrow `archive_daily_window` (IST minutes, e.g., `{"from_minute": 570, "to_minute": 630}`). Pin `tape.archive_window.as_of_receipt_id` when reproducing comparisons.

## Splits, Walk-Forward, and Tape Dates

- Set `training_from_ms`/`training_to_ms` strictly before `selection_from_ms`/`selection_to_ms`. Never fit thresholds on the same window used for selection.
- Span at least **3 distinct NSE session dates** (`min_tape_dates: 3`) in the selection/replay window.
- Validate walk-forward: verify parameters chosen on earlier sessions still hold positive net P&L on held-out later sessions rather than tuning across the whole archive window.

## Costs Always On

- Every strategy evaluation uses `paper_costs` (`per_order`, `buy_bps`, `sell_bps`, `slippage_bps`, `rounding_increment`).
- Reject setups whose expected move or target distance is smaller than round-trip spread + slippage + brokerage.

## Bias and Multiple-Testing Discipline

- **No lookahead**: Use only `observation` data at `at_ms`. Never assume future closes, session highs/lows, or unscored future quotes.
- **No survivorship bias**: Test across the full scanner/prospect stream rather than hand-picking post-hoc winners.
- **Deflated Sharpe / trial budget**: Searching dozens of parameter combinations inflates apparent Sharpe and net P&L from noise alone. Keep strategies simple (<= 2-3 parameters), record every trial count and `source_sha256` under `notes/`, and require wider net P&L margin and consistent win/payoff structure as trial count grows. Discard brittle parameter spikes.
