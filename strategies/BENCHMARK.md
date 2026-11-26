# Bar strategy benchmark

Every `archive_bars_v1` strategy in this directory, run through the archive
bar engine that research jobs use (`nivra_research::archive_bars`). Signals
fill at the next observed open with the schedule's slippage (at least one
paise) and fees. Each split starts with ₹10,00,000 and no positions. Python
state, such as rolling history, carries from training into test.

Most of these strategies lose money after costs. On the test split, 8 of the
14 lose money. Only 4 of the 14 make money in both splits: `rsi2_reversion`,
`volatility_targeted_trend`, `bollinger_daily_reversion` and
`high_52w_proximity`. Every minute strategy loses money in training. Treat
the winners as leads for more tests, not as edges.

## Data and command

The data is a local copy of the production archive, with NSE cash
equities only:

- Daily bars: 2,814 instruments, 2025-10-17 to 2026-11-25 (273 sessions).
- Minute bars: 10 large caps, 2025-10-17 to 2026-10-16.
- Input hashes (`input_bars_sha256`): daily `36dbe23fdf91…`, minute
  `ac386c11e5aa…`. Two runs produced identical results.

| Unit | Training | Test | Costs |
| --- | --- | --- | --- |
| `day` | 2025-10-17 to 2026-05-31 | 2026-06-01 to 2026-11-25 | [`costs/nse-delivery.json`](costs/nse-delivery.json) |
| `minute` | 2025-10-17 to 2026-06-30 | 2026-07-01 to 2026-10-16 | [`costs/nse-intraday.json`](costs/nse-intraday.json) |

The delivery schedule approximates an NSE delivery trade. Buy costs are
12 bps (0.1% STT, 0.015% stamp duty, exchange and SEBI fees). Sell costs are
10.5 bps. Each fill also pays ₹20 for brokerage or the DP charge, and the
fill price carries 5 bps of slippage. The intraday schedule uses 1 bp to
buy, 3 bps to sell (0.025% STT on the sell side) and ₹24 per order (₹20
brokerage plus GST), also with 5 bps of slippage.

```sh
printf '%s' "$ARCHIVE_PASSWORD" > /tmp/archive-password && chmod 600 /tmp/archive-password
target/debug/nivra-tools bar-bench \
  --archive-url postgresql://postgres@127.0.0.1:55440/nivra_archive \
  --password-file /tmp/archive-password --unit day \
  --from 2025-10-17 --test-from 2026-06-01 --to 2026-11-25 \
  --costs strategies/costs/nse-delivery.json --output day.md --json day.json
target/debug/nivra-tools bar-bench \
  --archive-url postgresql://postgres@127.0.0.1:55440/nivra_archive \
  --password-file /tmp/archive-password --unit minute \
  --from 2025-10-17 --test-from 2026-07-01 --to 2026-10-16 \
  --costs strategies/costs/nse-intraday.json --output minute.md --json minute.json
```

Each daily strategy takes 15 to 20 s and each minute strategy about 13 s.
The bench runs them in parallel.

## Daily bars, NSE delivery costs

The leaderboard is sorted by test return. Return is mark-to-market equity at
the end of the split over starting capital, so it counts positions still
open at the end. Sharpe uses daily equity returns, annualized with √252.
Turnover is the traded notional on both sides divided by capital. The fees
column holds the schedule's fees only. Slippage is in the fill prices.

| Strategy | Test return | Test Sharpe | Test max DD | Test turnover | Test trades | Test fees ₹ | Train return | Train Sharpe | Train max DD | Train trades | Open at test end |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `rsi2_reversion` | +11.50% | 1.37 | 8.16% | 48.8x | 287 | 66639 | +3.54% | 0.51 | 9.86% | 113 | 10 |
| `volatility_targeted_trend` | +5.66% | 1.65 | 3.46% | 1.6x | 23 | 3017 | +1.40% | 0.44 | 3.59% | 8 | 15 |
| `low_volatility` | +4.82% | 2.55 | 2.75% | 2.3x | 16 | 3719 | -0.33% | -0.08 | 5.59% | 18 | 20 |
| `bollinger_daily_reversion` | +3.15% | 0.52 | 9.56% | 21.7x | 124 | 29631 | +10.00% | 1.42 | 4.73% | 35 | 10 |
| `high_52w_proximity` | +2.95% | 0.53 | 8.35% | 8.2x | 61 | 12070 | +5.06% | 1.89 | 1.58% | 9 | 15 |
| `equal_weight_baseline` | +1.51% | 0.30 | 9.00% | 1.0x | 0 | 1559 | -5.08% | -0.40 | 17.04% | 0 | 20 |
| `time_series_momentum` | -0.27% | -0.01 | 6.94% | 3.4x | 46 | 6134 | +0.78% | 0.25 | 3.72% | 10 | 19 |
| `ma_crossover` | -4.74% | -0.54 | 14.65% | 3.9x | 27 | 5846 | +14.33% | 1.99 | 5.78% | 3 | 15 |
| `donchian_breakout` | -6.51% | -0.56 | 16.14% | 4.1x | 19 | 5654 | -24.02% | -2.98 | 27.66% | 20 | 10 |
| `cross_sectional_momentum` | -9.76% | -0.88 | 16.80% | 6.0x | 46 | 8977 | +20.88% | 1.73 | 6.44% | 28 | 15 |
| `volume_breakout` | -15.20% | -1.56 | 18.93% | 19.4x | 115 | 26661 | -21.44% | -1.96 | 21.82% | 137 | 10 |
| `short_term_reversal` | -17.91% | -1.91 | 21.25% | 40.1x | 233 | 54711 | +5.91% | 0.57 | 16.83% | 242 | 10 |

## Minute bars, NSE intraday costs

| Strategy | Test return | Test Sharpe | Test max DD | Test turnover | Test trades | Test fees ₹ | Train return | Train Sharpe | Train max DD | Train trades | Open at test end |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `minute_gap_fade` | +2.59% | 2.43 | 1.02% | 18.6x | 46 | 5930 | -6.65% | -1.76 | 7.71% | 140 | 0 |
| `minute_opening_range_breakout` | -4.59% | -2.41 | 5.81% | 31.1x | 82 | 10148 | -9.64% | -2.32 | 10.31% | 182 | 0 |
| `minute_intraday_momentum` | -4.77% | -6.60 | 5.00% | 61.8x | 164 | 20233 | -15.37% | -6.45 | 15.67% | 386 | 0 |

## Before and after costs

This is the same bench with a zero-cost schedule. The gap between the two
runs is not exactly the cost, because costs change position sizes and
which buys cash can cover.

| Strategy | Train gross | Train net | Test gross | Test net |
| --- | ---: | ---: | ---: | ---: |
| `rsi2_reversion` | +7.46% | +3.54% | +20.51% | +11.50% |
| `volatility_targeted_trend` | +1.62% | +1.40% | +6.04% | +5.66% |
| `low_volatility` | +0.20% | -0.33% | +5.30% | +4.82% |
| `bollinger_daily_reversion` | +11.40% | +10.00% | +7.17% | +3.15% |
| `high_52w_proximity` | +5.48% | +5.06% | +4.56% | +2.95% |
| `equal_weight_baseline` | -4.87% | -5.08% | +1.72% | +1.51% |
| `time_series_momentum` | +1.05% | +0.78% | +0.51% | -0.27% |
| `ma_crossover` | +14.59% | +14.33% | -3.96% | -4.74% |
| `donchian_breakout` | -23.32% | -24.02% | -6.67% | -6.51% |
| `cross_sectional_momentum` | +21.80% | +20.88% | -8.58% | -9.76% |
| `volume_breakout` | -15.77% | -21.44% | -10.66% | -15.20% |
| `short_term_reversal` | +15.29% | +5.91% | -10.50% | -17.91% |
| `minute_gap_fade` | -2.39% | -6.65% | +4.09% | +2.59% |
| `minute_opening_range_breakout` | -4.14% | -9.64% | -2.06% | -4.59% |
| `minute_intraday_momentum` | -4.31% | -15.37% | +0.25% | -4.77% |

## How to read it

- **Costs decide the high-turnover strategies.** `short_term_reversal`
  makes +15.3% gross in training and +5.9% net. In test it loses 10.5%
  gross and 17.9% net. `minute_intraday_momentum` gets close to breaking
  even in test before costs, but trades 62 times its capital and loses
  4.8% after costs. `rsi2_reversion` keeps a profit after paying ₹66,639
  in fees on ₹10 lakh, but only because the test window happened to suit it.
- **One window, short samples.** The test split holds about 120 sessions.
  At that length, the standard error of an annualized Sharpe ratio is
  about 1.4, so no result here is statistically different from zero.
- **Training is partly warm-up.** Lookbacks of 100 to 130 bars leave most
  daily strategies in cash for the first five months of training. Training
  fell for the 1/N baseline (−5.1%), so holding cash flatters training
  returns against it.
- **Regimes flip the ranking.** Cross-sectional momentum, short-term
  reversal and the moving-average crossover rank near the top in training
  and near the bottom in test. Only the four strategies named at the top
  of this page are positive in both splits.
- **The windows are short.** The archive holds about 13 months, so the
  momentum and 52-week-high windows are 5 to 12 months instead of the
  published 12.
- **Missing from the model:** spread, queue position, depth, NSE
  circuit locks, survivorship (the universe is today's instruments) and
  corporate actions. Bars are unadjusted. Every daily strategy restarts a
  name's history after an overnight jump of more than 35%, and the archive
  has 12 such opens. The bar ABI opens and closes whole positions and
  cannot resize them.
- **Minute evidence is thin.** The minute universe is 10 large caps,
  so a strategy holds at most five names at a time.

Re-run this page with the same split after you change a strategy. If you
tune on the test split, choose a later untouched window to assess it again,
as [research](../docs/RESEARCH.md#backtest-years-of-archived-bars)
describes.
