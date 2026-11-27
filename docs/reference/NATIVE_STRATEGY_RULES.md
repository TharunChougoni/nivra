# Native strategy rules — proposal

Status: proposal, 27 November 2026. Not built. The current path is the
persistent Python worker described in [research](../RESEARCH.md#in-the-paper-controller).

## The idea

A strategy has two parts. The decision says what to buy or sell and with which
stop, target and time exit. Execution checks cash, fees and quote age, fills,
and runs the exits. Execution is the same for every strategy and already lives
in Rust. Only the decision differs, and today it is always a Python script.

Most shipped strategies are the same shape: a few standard features compared
against thresholds. A rule format that the Rust engine evaluates directly would
run those with no process, no serialization and no review of arbitrary code.
Python stays for ideas the format cannot express.

## What a rule looks like

```json
{
  "abi": "strategy_rules_v1",
  "entry": {
    "all": [
      {"feature": "minutes_since_open", "gte": 20},
      {"feature": "range_position", "lte": 0.1},
      {"feature": "move_bps", "lte": {"times": [-2, "volatility_band_bps"]}},
      {"feature": "spread_bps", "lte": 12}
    ],
    "rank_by": "move_bps",
    "max_new": 2
  },
  "exit": {
    "target_bps": {"max": [{"times": [1.5, "volatility_band_bps"]}, 40]},
    "stop_bps": {"max": [{"times": [1.0, "volatility_band_bps"]}, 30]},
    "hold_ms": 900000,
    "flat_minute": 915
  },
  "size": {"order_rupees": 1000, "max_positions": 2},
  "edge": {"target_over_round_trip_cost": 2.0}
}
```

- Conditions are `all`/`any` trees over named features with `gte`, `lte`,
  `between` and small arithmetic (`times`, `plus`, `max`, `min`) on features
  and constants.
- Features are the ones the controller already maintains per key: price, move,
  relative move, volatility band, range position, session VWAP distance, book
  imbalance, relative volume, spread, minutes since open, circuit distance.
- Exits compile to the existing `exit_intent` (target, stop, time).

## Why

- **Speed.** Evaluating 20 prospects against a rule tree is microseconds inside
  the controller, so a fresh tick can turn into an action with no worker round
  trip.
- **Switching.** The decision lane switches rules instantly; nothing to start.
- **Safety.** A rule cannot read files, open sockets or loop. The review gate
  only checks that features exist and values are in range.
- **Research.** Research can sweep rule parameters cheaply in replay and on
  shadow books, since a shadow rule costs almost nothing.

## Boundaries

- Rules cannot keep arbitrary state between calls. If a strategy needs memory
  (for example a custom indicator over its own history), it stays in Python or
  the feature set grows to provide it.
- New features are added in Rust once and become available to every rule and
  to Python scripts through the same observation.
- Both kinds of strategy produce the same actions and pass the same execution
  gate, acceptance thresholds and library rules.

## Steps when this is picked up

1. Port three shipped strategies to rules and prove identical actions on the
   archived tapes with the action-bench.
2. Add the evaluator in `nivra-core` (pure, no I/O) and a `strategy_rules_v1`
   ABI beside `strategy_actions_v1` in the library and the chooser.
3. Let research author rules, with a schema-checked tool, and shadow them.
4. Measure tick-to-gate latency and CPU against the Python worker.
