---
name: autoresearch
description: "The autonomous research loop: one falsifiable idea per turn, one strategy, one frozen evaluation, the keep rule and the trial allowance. Load it when the brief kind is autoresearch_proposal or autoresearch_interpretation."
---

# Autonomous research loop

Nivra runs a research loop from the seal until 08:30 IST, and all day when the
exchange is closed. Rust owns the record, the clock and the evaluator. You own
one hypothesis and one editable strategy per turn. A recorded rejection is a
good outcome.

## What the host freezes

The campaign in the brief fixes the dates, the universe, the ₹5,000 research
capital, the paper costs, the training and scored (selection) split and the
comparator. You cannot change them. `backtest` in an autonomous turn takes
only the idea id, the workspace script path, its params and the stage.

## A proposal turn

1. Read `research_history`. Look for the incumbent of the cell you work on,
   earlier losers with the same mechanism and recorded findings.
2. Work on the brief's `slot`:
   - `mutate_incumbent`: change exactly one thing in an incumbent and name it
     as `parent_experiment_id` with comparator `parent`.
   - `revisit_failure`: take a recorded failure and fix its cause with a
     distinct mechanism, not a parameter nudge.
   - `new_family`: a new mechanism in a family with no incumbent. Families in
     `closed_families` had three scored non-improvements; leave them.
3. Call `idea_record` before you write code: the mechanism, what would
   falsify it, the family (`trend`, `reversal`, `breakout` or a new slug) and
   the regime you expect (`trend`, `range` or `unknown`, decided only from
   information available at decision time).
4. Write the strategy at `scripts/<name>.py` and its params under `params/`.
   For stage `bars` implement `on_bars(at_ms, split, bars, params, state)`
   from the `archive_bars_v1` contract. For stage `quote_replay` follow
   `strategy-abi`.
5. Call `backtest` once. Stage `bars` queues one bounded job and the verdict
   reaches your next turn: end the turn. Stage `quote_replay` replays recorded
   quotes inside the call and returns the verdict; a replay that passes
   acceptance registers in the strategy library as every replay does.

## The keep rule

The host keeps an experiment only when, on identical dates, universe, costs
and capital:

- its net P&L after costs on the scored split is strictly positive,
- it beats uninvested cash, its pinned parent and the cell incumbent by a
  strictly positive margin (ties keep the incumbent),
- its drawdown stays within the acceptance limit, and
- a quote replay ends flat. A bar run marks open holdings at the last close.

A keep replaces the cell incumbent. Everything else is recorded and stays
searchable.

## The trial allowance

Every scored variant spends one trial of the campaign allowance (60 by
default), including failed and abandoned runs. Changing a parameter after you
saw a score is a new trial. An identical submission returns the earlier record
and spends nothing. The day also has ceilings on new variants, turns, model
calls and cost. When the allowance or the day's budget is spent, the loop
waits; it never resets the count because a date or a book rolled over.

## An interpretation turn

The verdict in the brief is final. Record what it teaches with
`finding_record`: the observation, where it holds, its limitation and the
next test. Negative and inconclusive findings are as useful as positive ones.
When the status is `invalid` and `repair_allowed` is true, fix the cause in a
new script version and call `backtest` once more with the same idea id. Keep
a short note under `notes/` that cites the experiment and finding ids.

## Learned procedures

When a procedure worked on two distinct experiments, draft it at
`skills-proposed/<name>/SKILL.md` with `name`, `description` and
`findings: <ids>` frontmatter. A review-lane turn approves or rejects it and
the host installs approved revisions under `skills/`. Shipped skill names are
reserved. Never write a performance claim as a procedure.
