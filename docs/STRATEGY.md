# Current paper mandate

One continuing paper account: long-only NSE cash equities/ETFs plus buy-only
long-premium Nifty weekly options. The objective is to maximise profit; there is
no profitability claim. Futures, stock options, naked short options and live
orders remain out of scope.

The reviewed `decision_owner` policy is the current supported path: one
persistent orchestrator owns paper action, size and holding decisions. Luna
compiles current factual digests without selecting actions or vetoing owner
evaluation. Legacy Luna/Grok handoff policies remain decodable for archive and
reviewed migration, but cannot schedule independent runtime decisions. Its
cadence and evidence eligibility are described in [the model harness](MODEL_HARNESS.md);
adoption uses the explicit [policy migration](OPERATIONS.md#reviewing-the-decision-owner-migration).

Deterministic code validates every intent and commits simulated fills. No fixed
position-count, exposure, entry-count, daily-loss or drawdown caps apply to the
model-led book. Available cash including fees remains binding.

## Execution gates

Paper only; live disabled. No leverage/shorting. Use today's validated allowlist,
verified identities/ticks/whole shares, held units for sells, operator controls and
original complete quote receipts **≤2 seconds** old at execution. Model/data or
validation failures permit no new trades; ordinary model failures do not halt the
book. Corrupt identity/journal authority remains an integrity incident.

The cash universe requires normal-market NSE EQ securities/ETFs, lot size 1, valid
ISIN, unique reviewed broker mapping and no suspension. Missing dated evidence
excludes or blocks with a reason. History/scanner age never substitutes for fill
freshness. Provider tick, calendar and source caveats are recorded in
[market-source research](reference/MARKET_SOURCE_REVIEW.md).

The option gate is separate and additive: only the bounded current-week Nifty
shortlist resolved from the contract API and the BOD join is tradeable. Each
option order must be a Call/Put on the configured Nifty underlying, a positive
multiple of the contract lot, buy-to-open only, and a sell must be against
same-key held units. No option is opened on or held past its expiry; expiry day
is an explicit flatten-at-bid path, and an unsettled expiry halts the book
instead of carrying a dead key into the next session. Option premiums are never
run through the cash liquidity, traded-value or circuit screens.

## Fees

The example paper policy applies **₹0.01 per order**, **5 bps on buys**, **7 bps
on sells**, and **5 bps adverse slippage per side**, with adverse tick rounding and
upward fee rounding to paise. The configured policy is included in model evidence.
These are simulation assumptions, not certified broker charges;
see [fee research](reference/KOTAK_FEE_REVIEW.md).

`paper_costs` remains the cash schedule. Options use a separate optional
`paper_costs_by_segment` entry for `NSE_FO` (for example ₹0.01 per order, 0 bps
turnover, 10 bps adverse slippage and a ₹0.05 rounding increment). Long options
spend `premium × units` plus the F&O fee, never index notional, and there is no
margin ledger. Sells settle at the bid. Enabling options requires both the
`NSE_FO` fee schedule and an admitted `NSE_FO|` quote-key prefix.

New shared-engine fills retain `expected_price`: the original executable ask for
buys and bid for sells. `slippage_bps` is adverse signed execution difference
relative to that reference, including tick rounding, rounded to eight decimal
places (decimal midpoint-to-even). This is reporting precision, not a cost
assumption or a cash adjustment. Legacy fills omit both fields; no reference is
inferred for them. Paper and chronological replay use the same fill path.
Attribution arithmetic failure records `slippage_bps: null` (omitted in JSON),
without blocking the fill.

`realised_net_pnl_by_fill` in dashboard/session and offline replay reports aligns
with the fill list: buys are null; sells report proceeds minus their sell fee and
FIFO acquisition cost including buy fees. A partial lot allocation rounds to
1e-8 rupees, midpoint-to-even; its final exit takes the exact remaining cost.
Scale-ins remain separate FIFO lots. Unknown opening lots or arithmetic failures
report null for that instrument thereafter. This derived report never changes
fills, position entry averages, cash or sealed portfolio serialization.

Runtime config and offline paper replay manifests require explicit `paper_costs`:

```json
{"paper_costs":{"per_order":"0.01","buy_bps":"5","sell_bps":"7",
"slippage_bps":"5","rounding_increment":"0.01"}}
```

These reuse the shared fee arithmetic: `per_order` is simulated brokerage;
`buy_bps`/`sell_bps` aggregate all assumed turnover charges (including STT,
exchange, GST and stamp), rather than claiming individually certified statutory
rates. The example expresses the requested Kotak Trade Free Youth paper assumptions,
including a margin over estimated statutory charges; it is not broker certification.
No extra charge is added elsewhere. Missing configuration fails with
`paper_costs_required`; no policy is inferred. Values must be nonnegative exact decimal strings/integers;
side fees are at most 10000 bps, slippage is below 10000 bps, and an optional
rounding increment must be positive (`null` means exact fees). The slippage
assumption affects execution prices once, with adverse tick rounding.
New fills retain their exact cost policy; legacy fills without a stored policy keep
their recorded fee and cash movements, while their fee calculation remains unknown.
Resume refuses a changed or missing book policy (`paper_cost_policy_changed`).
A legacy book without a stored policy cannot be resumed merely by supplying one;
it requires the explicit reviewed `paper_cost_transition` evidence described in
[operations](OPERATIONS.md#paper-cost-configuration). That transition must bind
the original config and source journal, may carry positions only from a closed,
non-halted model-led source, and applies the new policy to the successor's new
fills. A new session may select new configured costs, including on carried
holdings, without changing prior fills. Original legacy JSON remains unchanged on
round trip. Genuine captured-day price/P&L agreement and account-specific fee
qualification remain unverified.

Operational allowances and deadlines protect resources and are configured
separately from trading risk. An allowance is not a target to consume.

The archived momentum strategy, tightening reviewer, fixed exits and
deterministic 15:15 flatten are not a fallback. See
[replay](PAPER_REPLAY.md) for the offline flow.

## Strategy laboratory

The opt-in [strategy sandbox](STRATEGY_SANDBOX.md) documents spec-driven shadow
rankings, scores-only Python/WASM execution, recorded replay and typed read-only
pipeline tools. Its proposals and approval records never activate a session.

## Offline market regimes

`sadr_core::regime` provides a deterministic classifier, without runtime
wiring. `compute_inputs` accepts aligned eligible-instrument prior-close/open/
current price slices and one-minute proxy median returns. It excludes invalid
price triples and minute returns, reports retained counts, computes median session
return and gap, breadth strictly above prior close, and population volatility.
Mismatched price lengths are errors; callers own eligibility and minute alignment.
All returns use decimal fractions, not percentages or bps.

`RegimeConfig` has serde defaults: `trend_return=0.004` defines the symmetric
open range band; equality selects up/down trend. `vol_low=0.0005` is inclusive
normal and `vol_high=0.002` inclusive high. `breadth_weak=0.4` and
`breadth_strong=0.6` include their respective boundaries. Absolute opening gap
`>= gap=0.005` is a gap day. Minimum retained samples are `min_instruments=200`
and `min_minutes=15`. Validation requires finite ordered thresholds, breadth
within [0,1], positive trend/gap, nonnegative low volatility, at least one
instrument and two minutes. Invalid configuration/inputs or thin samples classify
as `Unknown`, whose key is `unknown`. Known keys have four components, for
example `up_trend/high_vol/strong/gap` or `range/normal_vol/neutral/no_gap`.
Strategy performance storage and regime-based runtime selection are not
implemented.
