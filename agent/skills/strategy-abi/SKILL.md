---
name: strategy-abi
description: How to write a strategy_actions_v1 Python strategy (observation schema, action shape, exits, Decimal pricing, ticks, statelessness) and meet acceptance bars.
---

# `strategy_actions_v1` Strategy ABI

Write self-contained Python 3 scripts (no network, no OS/file access; run in a persistent `python3 -I -S -B` worker). Define a top-level `on_observation(obs)` function receiving one observation dict and returning `{"actions": [...]}` (no stdout JSON printing, no file reads). Modules must pass the AST import allowlist and model security review. Use `decimal.Decimal` for all prices and rupee amounts; never emit `NaN` or `Infinity`.

## Observation Fields

- `abi`: `"strategy_actions_v1"`
- `at_ms`: integer epoch milliseconds of this scan
- `params`: parameter object passed to the strategy
- `prospects`: up to 20 ranked prospects (`key`, `rank`, `score`, `features` — core scanner features; absent means missing, never `0`)
- `quotes`: depth quotes <= 2 s old (`bid`, `ask`, `bid_units`, `ask_units`, `source`, `receipt`)
- `positions`: held positions keyed by instrument key (`units`, `entry`, `mark`, `marked_at_ms`, `exit_intent`)
- `instruments`: allowlisted instruments present above (`symbol`, `tick`). Only trade keys in `instruments`
- `book`: the cash sleeve the strategy trades from: `sleeve` (`"cash"`), `equity` and `cash` (decimal strings), `open_positions`, `max_positions`, `realised_today` (net realised P&L on this IST date) and `drawdown_bps` (how far equity is below the sleeve's allocated capital)
- `sizing`: the cash sleeve's policy (`risk_per_trade_bps`, `max_position_bps`, `max_positions`, `min_order_rupees`). The runtime sizes every buy with it
- `costs`: `paper_costs` schedule (`per_order`, `buy_bps`, `sell_bps`, `slippage_bps`, `rounding_increment`)

## Action Shape (at most 32 actions per call)

```json
{
  "instrument_key": "NSE_EQ|INE000000001",
  "side": "buy",
  "max_price": "100.50",
  "conviction": "0.8",
  "rationale": "short reason",
  "exit_intent": {"target": "102.00", "stop": "99.50", "time": 1795579200000}
}
```

- `buy`: never set `units` (a buy with units is refused with `action_strategy_buy_units`). Set a `max_price` decimal string and an `exit_intent` with a `stop` below `max_price` (`action_stop_required`, `size_stop_not_below_price`). The runtime sizes the buy: the most shares whose loss to the stop, with fees, fits `sizing.risk_per_trade_bps` of sleeve equity, within `sizing.max_position_bps`, the sleeve's cash and the displayed ask depth. A wider stop buys fewer shares. Optional `conviction` (decimal string in (0, 1]) scales the risk budget down for a marginal signal. Refusals: `sleeve_positions_full`, `size_below_lot`, `sleeve_cash_insufficient`, `size_below_min_order`. Add slippage headroom above `ask` and align prices to `instruments[key]["tick"]` (e.g., `(p // tick) * tick`). `exit_intent.time` must be > `at_ms` (or `null`); if both `target` and `stop` are set, `target > stop`. Ensure target distance clears round-trip `costs`. Read `book` to adapt: open no more than `book.max_positions - book.open_positions` names, and enter less often in drawdown.
- `sell`: set `units` (<= held `units`), `max_price` (at or below `bid` with slippage headroom), and `exit_intent: {"target": null, "stop": null, "time": null}`.
- `revise_exit`: on a held key only; set `units: null`, `max_price: null`, and a non-empty `exit_intent`.

## Acceptance thresholds for quote-tape backtests

Defaults (overridable by planner `strategy_acceptance`):
- `min_trades`: >= 10 closed trades (`too_few_trades`)
- `min_net_pnl`: > `"0"` net P&L after costs (`net_pnl_too_low`)
- `max_drawdown_bps`: <= 300 bps of replay capital (`drawdown_too_deep`)
- `min_tape_dates`: >= 3 distinct session dates with strategy calls (`too_few_tape_dates`)
- `max_failure_bps`: <= 100 bps of calls (`too_many_strategy_failures`)
- `open_positions`: 0 at end of tape (`open_positions_at_end`) — set `exit_intent.time` or sell before session close so replays end flat.
