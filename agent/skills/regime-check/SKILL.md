---
name: regime-check
description: How to segment backtest and paper strategy results by market regime (trend, volatility, breadth, gap) using the brief's regime and index context.
---

# Regime Check

Nivra attaches a deterministic intraday `regime` summary to lane briefs when enough evidence is present. Strategies that only earn in one regime can lose steadily when conditions shift.

## Regime Fields in the Brief

`brief.regime` (when non-null) reports:
- `source`: `"index"` (derived from configured `index_feeds`) or `"market"` (derived from scanner minute returns)
- `label`: `<trend>/<volatility>/<breadth>/<gap>`, or `"unknown"` (when `< 200` instruments or `< 15` minutes by default)
  - Trend: `up_trend` (`session_return >= 0.004`), `down_trend` (`<= -0.004`), `range` (between)
  - Volatility: `low_vol` (`realised_volatility < 0.0005`), `normal_vol`, `high_vol` (`>= 0.002`)
  - Breadth: `weak` (`< 0.4`), `neutral`, `strong` (`>= 0.6`)
  - Gap: `gap` (`|opening_gap| >= 0.005`), `no_gap`
- Numeric metrics: `session_return`, `opening_gap`, `realised_volatility`, `breadth`, `instrument_count`, `minute_count`

## How to Run a Regime Split

1. **Tag tape dates with regimes**: Use `brief.regime`, `decisions`, `intraday`, and prior session `notes/` to identify each replay date's market regime (trend direction and volatility level).
2. **Inspect per-date and per-trade P&L**: From the `backtest` response (`tape.dates`, `trades`, `summary`), group closed trades by session date and regime bucket rather than trusting the aggregate `net_pnl` alone.
3. **Check regime concentration**: Flag a strategy if:
   - Most net P&L comes from a single trend or gap day while `range` or `down_trend` days bleed after costs.
   - `high_vol` sessions trigger stop-outs or slippage losses not seen in `low_vol`/`normal_vol`.
4. **Report regime fit**: Document in `notes/` which regimes the strategy was tested on, which regimes remain untested, and when the decision lane should stand down (`strategy_select` with `null`) on `regime_changed`.
