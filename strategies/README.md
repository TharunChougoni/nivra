# Strategy library seeds

Reference and fallback `strategy_actions_v1` scripts for the research lane.
Each `<name>.py` is self-contained (standard library only) and each
`<name>.params.json` is a replay parameters file. See
[Action strategies](../docs/RESEARCH.md#action-strategies) for the ABI.

All four are long only, size each entry at `order_rupees` (capped by cash and
displayed ask depth), keep at most `max_positions`, open nothing after
`last_entry_minute` IST, and set the time exit to `hold_ms` or
`flat_minute` (15:15 IST), whichever comes first. Every target must clear
`edge_margin` times the round-trip cost from `obs.costs` plus the spread.
Params are strings or integers because the replay report allows no floats.

The observation has no session high/low or bar history. "Range" and "band"
mean the scanner's lookback window (`range_position`, `move_bps`,
`volatility_bps`), not a session or 20-bar moving-average band.

| Name | Idea | Key params | Should work | Should not work |
| --- | --- | --- | --- | --- |
| `opening_range_breakout` | Crabel ORB: after the opening range, buy names at the top of their range, above VWAP, on heavy relative volume | `range_minutes`, `entry_window_minutes`, `min_range_position`, `min_move_bps`, `min_rel_volume` | Trend days with news or sector flow; liquid names with a real volume surge | Choppy, range-bound sessions; false breakouts on thin volume |
| `vwap_reversion` | Fade a stretch below session VWAP, measured in volatility units; target VWAP | `entry_z`, `stop_z`, `max_relative_drop_bps`, `avoid_open_minutes` | Mid-session, mean-reverting liquid large caps | Trend days and news-driven moves that never revisit VWAP |
| `relative_strength` | Cross-sectional intraday momentum: buy the strongest names vs the market median move | `min_relative_bps`, `min_rel_volume`, `max_move_bps` | Days with sector rotation and persistent leaders | Sharp reversals and market-wide risk-off |
| `bollinger_reversion` | Bollinger / RSI(2)-style: buy a drop of more than `band_z` volatility bands at the bottom of the range | `band_z`, `max_range_position`, `target_band_z`, `stop_band_z` | Quiet tapes where overshoots snap back within minutes | Falling markets and stocks with a fundamental reason to drop |

`cost_aware_momentum.py` is a ranker (scores only), not an action strategy.

Run one offline:

```sh
nivra strategy-lab replay-strategy --source strategies/vwap_reversion.py \
  --parameters strategies/vwap_reversion.params.json \
  --manifest manifest.json --events events.jsonl --output report.json
```
