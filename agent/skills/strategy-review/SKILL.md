---
name: strategy-review
description: Post-session checklist for the review lane when auditing a newly accepted or active strategy_actions_v1 strategy.
---

# Strategy Review Checklist

Run this checklist when reviewing a session with a newly accepted or active `strategy_actions_v1` strategy. You are read-only: use `list`, `read`, `portfolio`, `decisions`, `evidence`, `intraday`, `session`, and `research_result` to verify facts; never claim a trade or policy change.

## 1. Verify Replay Provenance and Acceptance

- Check `list` and `read` on workspace `notes/` for the strategy's recorded `source_sha256`, `report_sha256`, `params`, assumed `paper_costs`, and trial count.
- Verify the accepted replay met the acceptance bars: >= 10 closed trades, `net_pnl > 0` after costs, `worst_drawdown` <= 300 bps of capital, >= 3 distinct `tape_dates`, strategy failure rate <= 100 bps, and 0 `open_positions` at tape end.

## 2. Compare Live Paper Execution Against Replay

- Inspect `brief.fills`, `brief.positions`, `decisions`, and `portfolio`: compare live closed-trade count, fees, realized P&L, and exit reasons (target, stop, time) against the replay `summary`.
- Check for `execution_refusals` (such as `stale_quote`, `price_limit`, `insufficient_cash`, `insufficient_depth`) or strategy failures that indicate bad `tick` rounding, tight `max_price` headroom, or oversized orders.

## 3. Check Regime Fit and Concentration

- Note `brief.regime` (`label`, `session_return`, `realised_volatility`, `breadth`, `source`).
- Verify whether today's regime was represented in the accepted `tape_dates`, and check if P&L was driven by one outlier trade or consistent across entries.

## 4. Write Concrete Next-Session Memory

- Summarize verified **Worked**, **Failed**, and **Open Research Questions** (e.g., untested regimes, execution refusals, exit-horizon tuning) so the next decision turn and `after_close` research turn act on facts.
