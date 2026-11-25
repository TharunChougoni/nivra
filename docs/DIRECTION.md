# Direction: algorithms first, models as researchers

Nivra is moving from per-trade model decisions to **model-written algorithms
that code executes**. This page describes the target design and why. The
[issues](https://github.com/TharunChougoni/nivra/issues) track each step. Until
a step lands, the other guides describe what the code does today.

## Why

On 25 November 2026, the model-led book made one round trip in a full session,
from 52 decision turns and 177 entry triggers. A replay of those triggers
against the recorded quotes after paper costs gave these results:

- Buying every trigger lost money: a median of −49 bps over a 15-minute hold,
  and only 35% of trades won.
- Liquid names, and names the gate passed to the decision lane, were positive
  on average but negative at the median. The edge came from a few large winners.
- Adding 60 s of model latency did not change the result.

The edge comes from *which* signals are traded and *how exits are set*. Code
can apply that kind of rule instantly and consistently, and a backtest after
costs can check it. A model judging each trade, about a minute behind the
signal, adds cost and noise without an edge we can measure.

## Target design

| Part | Role |
| --- | --- |
| Research lane | Writes strategies (entry rule, size, stop, target, time exit) after the close on the day's recorded quotes, and backtests each one after costs. |
| Acceptance | A deterministic after-cost replay accepts or rejects a strategy and registers accepted versions, hash-pinned, in the strategy library. Paper trading does not need a multi-day shadow period first. |
| Strategy executor | The paper controller runs the active strategy on every scan. It sends that strategy's actions through the same execution gate as model plans: identity, cash, fees and quotes no more than 2 s old. |
| Decision lane | Supervises. It picks the active strategy at the open and when the regime changes, and handles what rules cannot: Nifty options, exceptions and held positions in unusual states. |
| Gate lane | Removed from the cash entry path once the executor handles entries. |
| Review lane | Reviews the day after the seal and leaves an agenda for research. |

Deterministic code still owns validation, credentials, cash and fees, fills and
protection. Paper only: nothing here enables live orders.

## Order of work

Tracked in [#44](https://github.com/TharunChougoni/nivra/issues/44):

1. Record exact refusal reasons and give models the cost rates (#28, #37).
2. Fix the option spot source so the Nifty options mandate can load (#38).
3. Run research after the close, every day, and record the quote tape it needs (#39).
4. Extend the strategy contract from scores-only rankers to entry and exit rules (#40).
5. Add the deterministic acceptance check and automatic registration (#41).
6. Add the strategy executor to the paper controller (#42).
7. Move the decision lane to supervision, and remove the gate from cash entries (#43).

Capital stays at ₹5,000 for paper trading. Costs are proportional (brokerage
percentages are below a ₹20 cap at this order size), so the bar is to beat
about 25–40 bps per round trip, including the spread.
