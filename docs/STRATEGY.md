# Paper mandate

Nivra runs one continuing paper account. It trades long-only NSE cash equities
and ETFs, and buy-only long-premium Nifty weekly options. The objective is
profit. Futures, stock options, short options and live orders are out of scope.

Today a model in the pi decision lane chooses each paper action, size and
holding period. Only a completed turn with a validated `TradePlan` reaches the
paper plan. [pi agent lanes](AGENT.md) describes triggers, budgets and
evidence. Change the policy through the reviewed config-apply flow in
[operations](OPERATIONS.md). The planned design has models write strategies
that code executes. See [direction](DIRECTION.md).

## Execution gate

Deterministic code validates every intent and commits simulated fills. The
model-led book has no position-count, exposure, entry-count, daily-loss or
drawdown caps. Available cash, including fees, is always binding.

An action executes only when all of these hold:

- The instrument is on today's validated allowlist, with a verified identity,
  tick size and whole-share or whole-lot quantity.
- A sell is against held units of the same key. There is no shorting or leverage.
- The original quote receipt is no more than **2 seconds** old at execution.
  Scanner or history age never substitutes for this check.
- Operator controls allow trading.

A model, data or validation failure blocks new trades but does not halt the
book. No deterministic strategy replaces a failed or unavailable model.

### Cash universe

The cash universe is normal-market NSE EQ securities and ETFs with lot size 1,
a valid ISIN, one reviewed broker mapping and no suspension. Missing dated
evidence excludes the instrument or blocks with a reason. Provider tick,
calendar and source caveats are in
[market-source research](reference/MARKET_SOURCE_REVIEW.md).

### Nifty options

The option gate is separate from the cash gate. Only the current-week Nifty
shortlist, resolved from the contract API and the BOD join, is tradeable. Each
option order must:

- be a Call or Put on the configured Nifty underlying,
- be a positive multiple of the contract lot,
- open with a buy, and close with a sell against held units of the same key.

No option is opened on its expiry day or held past it. On expiry day the
runtime sells at the bid. An expiring position that cannot be sold before the
cutoff halts the book. Cash liquidity, traded-value and circuit screens do not
apply to option premiums.

Enabling options needs an `NSE_FO` fee entry and an `NSE_FO|` quote-key prefix.

The option universe prepares from the Nifty spot on the index feed
(`index_feeds` must include the underlying key). The spot must be no more than
two seconds old by its original receipt. Until a fresh point arrives, the status
stays `waiting_for_spot`. One lot is the premium times the contract lot size, so
a small book can afford only low premiums.

## Fees

The runtime config and offline paper replay manifests need an explicit
`paper_costs` object. A missing policy fails with `paper_costs_required`. The
example policy is:

```json
{"paper_costs":{"per_order":"0.01","buy_bps":"5","sell_bps":"7",
"slippage_bps":"5","rounding_increment":"0.01"}}
```

| Field | Meaning |
| --- | --- |
| `per_order` | Simulated brokerage per order, in rupees |
| `buy_bps`, `sell_bps` | All assumed turnover charges per side, including STT, exchange, GST and stamp. At most 10000 |
| `slippage_bps` | Adverse slippage per side, applied once to the execution price. Below 10000 |
| `rounding_increment` | Upward fee rounding step. `null` means exact fees |

Values are non-negative exact decimals. Prices round adversely to the tick.
These are simulation assumptions, not broker charges. See
[fee research](reference/KOTAK_FEE_REVIEW.md).

Options use a separate `paper_costs_by_segment` entry for `NSE_FO`, for example
₹0.01 per order, 0 bps turnover, 10 bps slippage and a ₹0.05 rounding step. A
long option spends `premium × units` plus the F&O fee. There is no margin ledger.

Resume refuses a changed policy with `paper_cost_policy_changed`. A new
session may choose new costs. A legacy book without a stored policy needs a
reviewed `paper_cost_transition`. See [operations](OPERATIONS.md).

### Fill reporting

Each new fill records `expected_price` (the ask for a buy, the bid for a sell)
and `slippage_bps`, the adverse difference from it. `realised_net_pnl_by_fill`
in reports gives null for buys. For a sell it gives proceeds minus fees and the
FIFO cost of the closed lots. It never changes fills or cash.

## Research

Strategies today only rank scanner prospects. See [research](RESEARCH.md) for
the sandboxed workspace, rankers, replay, registration and research jobs.
