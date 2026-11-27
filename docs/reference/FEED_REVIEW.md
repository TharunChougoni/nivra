# Feed review — 27 November 2026

Dated measurements taken on the paper host, plus the vocabulary correction they
needed. This is a review, not current policy. Current runtime behaviour is in
[market data](../MARKET_DATA.md), and the rethink is tracked in
[issue 53](https://github.com/TharunChougoni/nivra/issues/53).

## The vocabulary was wrong

LTPC is not the name of the socket. It is one of three subscription modes on one
Upstox V3 socket, and the mode is chosen per key. The wire format in
[MarketDataFeed.proto](../../rust/nivra-cli/src/runtime/market_feed/MarketDataFeed.proto)
defines all three variants. Nivra decodes only the first.

| Variant | Field | Carries | Depth |
| --- | --- | --- | --- |
| `ltpc` | `ltp`, `ltt`, `ltq`, `cp` | Last price, its trade time, its size, previous close | None |
| `firstLevelWithGreeks` | `ltpc` + `firstDepth` (+ greeks, `vtt`, `oi`, `iv`) | The above plus one level: best bid, best ask, size at each | One level |
| `fullFeed.marketFF` | `ltpc` + `marketLevel` (+ OHLC, `atp`, `vtt`, `oi`, `tbq`, `tsq`, `iep`, `rp`, `ieq`) | The above plus the five level book and aggregate buy/sell quantity | Five levels |

`Quote` needs bid, ask, `bid_units` and `ask_units`. `firstLevelWithGreeks`
carries exactly that. Five levels is more than the gate can use.

## What the runtime does today

- One connection, every cash universe key plus the configured index keys, all in
  `ltpc` mode. The feed's own subscription limit is 5,000 keys
  (`market_feed::subscription_keys`). The host subscribes 2,766 plus index keys.
- The decoder implements the `ltpc` variant only. Frames carrying `fullFeed` are
  skipped as unknown fields.
- Executable depth comes from REST `GET /v2/market-quote/quotes`, for the
  40-name shortlist, every `depth_poll_ms` (8,000 ms).
- `subscription_plan.rs` holds `ltpc_core_keys: 1900` and `full_keys: 100`, and
  its own header says it is not consumed by the feed. **The 1,900 is dead code,
  not a provider limit.** It was read as a ceiling during the rethink and it is
  not one.
- The feed reports itself as `screening_only: true` with `depth_mode: "rest"`.

## Measured on the paper host

Feed health, one sample, 27 November 2026:

| Reading | Value |
| --- | ---: |
| Mode | `ltpc`, one connection of one allowed |
| Messages | 540 per second |
| Newest message age | 85 ms |
| Cached keys | 2,767 |
| Ticks since start | 1,827,656 |
| Invalid frames | 0 |
| Reconnects | 0 |

Depth coverage in the archive, trailing 12 hours of the session:

| Reading | Value |
| --- | ---: |
| Instruments with an execution-purpose depth receipt | 817 |
| Instruments with none | 1,949 |
| Median gap between receipts, where present | 9.0 s |
| 90th percentile gap | 79.3 s |
| Instruments with a median gap under 2 s | 0 |
| Rows by purpose | 88k depth, 257k metadata, 1k scanner, 379 research |

Strategy execution on the same day:

| Reading | Value |
| --- | ---: |
| Calls | 320 |
| Failures | 0 |
| Actions | 1 |
| Outcome | `stale_quote` |
| Quote age at the gate | 3,164 ms |
| Receipt age at the gate | 3,156 ms |
| Gate limit | 2,000 ms |
| Apply lag (`at` minus `observed_at`) | 2,980 ms |

Replay bench over two sessions and part of a third, 54 replays, 4,069 calls per
variant: only `bollinger_reversion` traded, two round trips, +₹10.18 on ₹1,000
clips. Seven strategies and all their variants placed no order and no refused
order. Every run was rejected for `too_few_trades` and `too_few_tape_dates`.

## Where the seconds go

| Stage | Cost on the host | Source |
| --- | --- | --- |
| Broker receipt to the next rank pass | up to 2,000 ms | loop sleeps `min(tick_ms, 2000)`; host sets no `tick_ms` |
| Strategy call | 90 ms | persistent worker, measured after the v0.16.0 change |
| Worker reply to the book transaction | up to 2,000 ms | the reply is read on the next loop pass |
| Depth read to the next depth read | 8,000 ms | `depth_poll_ms` |
| Median gap achieved per instrument | 9,000 ms | archive measurement above |

The feed is not in that table. The loss is in the loop cadence and the depth
poll, both of which are settings.

## Capacity and cost

Rest reads, from `READ_QUOTA_CAPS`, set to 90 percent of the published standard
API limits of 50 per second, 500 per minute and 2,000 per 30 minutes:

| Window | Nivra cap | Published |
| --- | ---: | ---: |
| Per second | 45 | 50 |
| Per minute | 450 | 500 |
| Per 30 minutes | 1,800 | 2,000 |

Keys per request: Nivra caps at 200 (`quote_keys.max_keys`). The provider
advertises the full quote endpoint at up to 500 instruments. Raising the cap is
a config change, not a code change, and it halves or better the number of
requests a pass needs.

Push capacity, from the documented matrix encoded in `subscription_plan.rs`:

| Layout | Keys |
| --- | ---: |
| One connection, LTPC only | 5,000 |
| One connection, full only | 2,000 |
| One connection, mixed | 2,000 LTPC + 1,500 full |
| Two connections | the above per connection |

There is no paid market-data tier. The two-tier table on the provider's
rate-limit page is order placement, 10 per second normally and 50 per second for
SEBI-registered algos. That registration is exchange paperwork, not a purchase,
and it does not apply to market data reads.

## Options

| Option | Change | Gets | Costs |
| --- | --- | --- | --- |
| A. Tune the poll | config only: `tick_ms` 250, `depth_poll_ms` to the loop cadence, `max_keys` 500 | reply wait drops from 2 s to 250 ms; depth cadence from 8 s to about 2 s | still polling; depth stays REST-bound |
| B. Decode pushed depth | decode `firstLevelWithGreeks` (or `marketFF`), subscribe promoted keys in that mode on connection two | real bid/ask on push, 85 ms transport, no REST depth at all | decoder, subscription mode plumbing, one more connection to supervise |
| C. Synthetic quotes | build a `Quote` from the last price plus a spread taken from the archive | every strategy can act on all 2,766 names | fills stop being executable; P&L needs a separate line |
| D. Leave it | none | nothing | 7 of 8 strategies stay silent |

B removes the measured 9 second gap. A alone does not, because 40 keys per 2
seconds still leaves each name refreshed every two seconds at best and only for
40 names. C should only ever ship as a labelled second P&L line, never mixed
with executable fills.

## Open questions before choosing

1. What modes this account may subscribe, and how the provider counts a mixed
   subscription against the pools. One probe on the host answers it.
2. Whether `firstDepth` is delivered for every depth change or only on trade,
   which decides the real per-name freshness for quiet stocks.
3. Whether promotion should be strategy-led (the strategy nominates keys) or
   ranker-led (the controller promotes the top of the projection). The bench
   suggests ranker-led will still starve strategies whose trigger is not the
   top of the ranking.

## Do not repeat

- Do not treat 1,900 as a provider limit. It is unused code.
- Do not split the stream to fit 2,766 keys. The LTPC pool is 5,000.
- Do not read the archive for live quotes. It stores the same receipts, later.
- Do not raise the 2 second gate. It is the only thing keeping fills honest.
