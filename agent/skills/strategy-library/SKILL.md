---
name: strategy-library
description: How research finds a strategy the executor can run today. Replay the shipped reference action strategies over recent sessions, then your own variants, and let acceptance register what passes.
---

# Strategy library: evaluate and register reference strategies

The executor runs one `strategy_actions_v1` strategy on each scan. It runs an
accepted library version when one can run on this build. Otherwise it runs a
configured fallback, which is labelled `fallback:<name>`. Your job is to put an
accepted version in the library.

## Where the strategies are

- `reference/strategies/` in the workspace holds the shipped strategies. It is
  read-only and refreshed with each release.
  - Action strategies (`strategy_actions_v1`): `opening_range_breakout`,
    `vwap_reversion`, `relative_strength`, `bollinger_reversion`,
    `order_book_imbalance`, `circuit_magnet`, `volume_climax_reversal`,
    `vwap_trend`. Each `<name>.py` has a `<name>.params.json` file shaped
    `{"version":1,"params":{...}}`.
  - Bar strategies (`archive_bars_v1`, params with `"abi":"archive_bars_v1"`)
    cannot run on the executor. Use them only for regime research.
  - `README.md` gives each strategy's idea and when it should and should not
    work. `BENCHMARK.md` gives the bar results on real NSE data.
- The brief's `strategies` block lists accepted versions (`eligible`), the
  configured `fallback` names and what runs now (`active_strategy_id`,
  `active_basis`, `active_blocked_reason`). On a `strategy_health` decision
  turn, check the executor's `strategy_health` facts and the current regime
  before keeping, switching, or standing down the active version.

## Procedure

1. Read `reference/strategies/README.md`. Then read the brief's `regime` and
   the `task.recent_archive` window: `sessions`, `selection_from_ms` and
   `selection_to_ms`. If the window is `unavailable`, report the reason and
   stop.
2. Pick the references whose "should work" column fits the recent sessions.
   Start with the configured fallbacks.
3. For each reference, `read` its `.py` and `.params.json`. Call `backtest`
   with:
   - `source`: the script text, unchanged.
   - `params`: the `params` object from the params file.
   - `dataset_id`: `recent_archive`.
   - `selection_from_ms` and `selection_to_ms`: from `task.recent_archive`.
   - `training_from_ms` and `training_to_ms`: both `selection_from_ms - 1`.
   - `hypothesis`: one sentence on why it should work in these sessions.
4. Read the report's `acceptance`. A `verdict` of `accepted` means the
   version is registered. Its `strategy_id` is in the result. A `rejected`
   verdict names each bar it missed: trades, net P&L, drawdown, tape dates,
   failures or open positions.
5. When no reference passes, write a variant under `scripts/`. Change one idea
   at a time, for example the entry threshold, the hold time or the cost
   margin. Replay it the same way. Read the `strategy-abi` skill first. Never
   loosen a cost margin below the round-trip cost.
6. Write a note under `notes/` for each strategy you replay: the name, the
   source sha256, the params, the sessions, the trades, the net P&L and the
   verdict.

## Rules

- Never change the acceptance thresholds and never claim that a strategy is
  accepted without a report that says so.
- Each replay counts as a trial. Report how many you ran, so that a pass
  after many trials is read with care.
- End with the strategies that passed, the one you would run today and why,
  and what is still uncertain. The decision lane selects the strategy. You
  do not.
