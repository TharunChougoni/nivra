# Strategy library seeds

The research lane uses these scripts as references and fallbacks. Every
build embeds this directory and seeds it read-only into the lane workspace
under `reference/strategies/`. The planner's `strategy_execution.fallback`
list names the action strategies the paper controller may run, labelled as
fallback, while no accepted version can run. See
[fallback strategies](../docs/RESEARCH.md#fallback-strategies). Each
`<name>.py` is self-contained and uses the standard library only. Its
`<name>.params.json` file holds its parameters. All strategies are long
only, because the paper book holds cash equity.

- **Action strategies** (`strategy_actions_v1`) run on each live or
  replayed observation. See
  [Action strategies](../docs/RESEARCH.md#action-strategies).
- **Bar strategies** (`archive_bars_v1`) run over archived daily or minute
  bars. See [archived bars](../docs/RESEARCH.md#backtest-years-of-archived-bars).
  [BENCHMARK.md](BENCHMARK.md) holds their results on real NSE data.

## Action strategies

These strategies size each entry at `order_rupees`, capped by cash and the
displayed ask depth. Each one holds at most `max_positions`, opens nothing
after `last_entry_minute` IST, and sets the time exit to `hold_ms` or
`flat_minute` (15:15 IST), whichever comes first. Every target must clear
`edge_margin` times the round-trip cost from `obs.costs`, plus the spread.
Params are strings or integers because the replay report allows no floats.

The observation has no session high or low and no bar history. In these
strategies, "range" and "band" mean the scanner's lookback window
(`range_position`, `move_bps`, `volatility_bps`), not a session range or a
20-bar moving-average band.

| Name | Idea | Key params | Should work | Should not work |
| --- | --- | --- | --- | --- |
| `opening_range_breakout` | Crabel ORB: after the opening range, buy names at the top of their range, above VWAP, on heavy relative volume | `range_minutes`, `entry_window_minutes`, `min_range_position`, `min_move_bps`, `min_rel_volume` | Trend days with news or sector flow; liquid names with a real volume surge | Choppy, range-bound sessions; false breakouts on thin volume |
| `vwap_reversion` | Fade a stretch below session VWAP, measured in volatility units; target VWAP | `entry_z`, `stop_z`, `max_relative_drop_bps`, `avoid_open_minutes` | Mid-session, mean-reverting liquid large caps | Trend days and news-driven moves that never revisit VWAP |
| `relative_strength` | Cross-sectional intraday momentum: buy the strongest names vs the market median move | `min_relative_bps`, `min_rel_volume`, `max_move_bps` | Days with sector rotation and persistent leaders | Sharp reversals and market-wide risk-off |
| `bollinger_reversion` | Bollinger / RSI(2)-style: buy a drop of more than `band_z` volatility bands at the bottom of the range | `band_z`, `max_range_position`, `target_band_z`, `stop_band_z` | Quiet tapes where overshoots snap back within minutes | Falling markets and stocks with a fundamental reason to drop |
| `order_book_imbalance` | Cao, Hansch and Wang; Cont, Kukanov and Stoikov: buy a bid-heavy book (total and at the touch) above VWAP, for a short hold | `min_imbalance`, `hold_ms`, `min_target_bps` | Liquid names where displayed depth is genuine | Spoofed or iceberg depth; spreads too wide for a small edge |
| `circuit_magnet` | Cho, Russell, Tiao and Tsay magnet effect: buy strong names 100 to 400 bps under the upper price band, target just short of the band | `min_circuit_bps`, `max_circuit_bps`, `band_buffer_bps`, `min_move_bps` | Banded small and mid caps on news with buyers racing the lock | F&O names (no fixed band), and reversals off the band |
| `volume_climax_reversal` | Campbell, Grossman and Wang: buy a sharp, stock-specific drop on heavy volume; target half the drop, capped at VWAP | `min_drop_bps`, `min_relative_drop_bps`, `min_rel_volume`, `target_retrace` | Forced or liquidity selling in otherwise sound names | Drops on real news, and market-wide selloffs |
| `vwap_trend` | Zarattini and Aziz VWAP trend: buy names holding a little above VWAP with positive momentum; stop just under VWAP | `min_above_bps`, `max_above_bps`, `stop_below_vwap_bps` | Steady trend days | Chop around VWAP, which stops out repeatedly |

[BENCHMARK.md](BENCHMARK.md#action-strategies-on-real-quote-tapes) replays them
over archived quote tapes with `action-bench`
(`strategies/action-bench.plan.json`).

Check every action strategy against empty, malformed, untradable and
trigger observations:

```sh
python3 strategies/check_actions.py
```

Replay one offline:

```sh
nivra strategy-lab replay-strategy --source strategies/vwap_reversion.py \
  --parameters strategies/vwap_reversion.params.json \
  --manifest manifest.json --events events.jsonl --output report.json
```

## Bar strategies

A bar strategy's params file is
`{"abi":"archive_bars_v1","unit":"day"|"minute","params":{...}}`. The
research lane takes the `params` object. Each strategy works out its own
cash and holdings, because the engine does not report fills, from
`capital_rupees` and a `cost_bps` allowance. Daily strategies trade names
with at least ₹10 crore of 20-day average traded value (₹25 crore for
reversal) and a price of at least ₹20. When an overnight jump is larger
than `max_gap`, a daily strategy restarts that name's history, because the
jump is probably an unadjusted corporate action. Minute strategies go flat
from 15:14 IST.

| Name | Unit | Idea (source) |
| --- | --- | --- |
| `equal_weight_baseline` | day | Baseline, not a family: buy the 20 most-traded names at each split's start and hold them (DeMiguel, Garlappi and Uppal 2009) |
| `time_series_momentum` | day | Hold each of the 30 most liquid names while its own 105-bar return is positive (Moskowitz, Ooi and Pedersen 2012) |
| `cross_sectional_momentum` | day | Hold the top 15 by 105-bar return skipping the last 21 bars, monthly (Jegadeesh and Titman 1993) |
| `short_term_reversal` | day | Hold the 10 worst 5-bar losers, weekly (Jegadeesh 1990; Lehmann 1990) |
| `high_52w_proximity` | day | Hold the 15 names closest to their high over the available history (up to 250 bars), monthly (George and Hwang 2004) |
| `low_volatility` | day | Hold the 20 names with the lowest 60-bar volatility, monthly (Ang et al. 2006; Baker, Bradley and Wurgler 2011) |
| `ma_crossover` | day | Enter on a 20/100-bar SMA golden cross and exit on the death cross (Brock, Lakonishok and LeBaron 1992; Faber 2007) |
| `donchian_breakout` | day | Enter on a 55-bar high and exit on a 20-bar low, the Turtle rules (Donchian; Faith 2007) |
| `rsi2_reversion` | day | Buy when RSI(2) is below 10 and the close is above its 100-bar average; exit above the 5-bar average (Connors and Alvarez 2008) |
| `bollinger_daily_reversion` | day | Buy below the 20-bar lower band (2σ) in an uptrend; exit at the middle band (Bollinger 2001) |
| `volatility_targeted_trend` | day | Trend filter on the top 100 names, each sized at entry to a 15% volatility target (Moreira and Muir 2017; Harvey et al. 2018) |
| `volume_breakout` | day | Buy at least 3× volume on a 20-bar high close near the day's top; hold 10 bars (Gervais, Kaniel and Mingelgrin 2001) |
| `minute_opening_range_breakout` | minute | Buy a close above the 15-minute opening range on above-average opening volume; stop at the range low (Crabel 1990; Zarattini, Barbon and Aziz 2024) |
| `minute_gap_fade` | minute | Buy a 0.75% to 4% gap down at the open; exit when the gap fills, at the stop or at 15:14 (Berkman et al. 2012; Caporale and Plastun 2017) |
| `minute_intraday_momentum` | minute | When the first half hour is up, buy the names that are up at 15:00 and sell at 15:28 (Gao, Han, Li and Zhou 2018) |

Benchmark every bar strategy against an archive (see
[BENCHMARK.md](BENCHMARK.md) for the exact runs):

```sh
target/debug/nivra-tools bar-bench --archive-url postgresql://USER@HOST:PORT/DB \
  --password-file PASSWORD_FILE --unit day \
  --from 2025-10-17 --test-from 2026-06-01 --to 2026-11-25 \
  --costs strategies/costs/nse-delivery.json --output day.md --json day.json
```
