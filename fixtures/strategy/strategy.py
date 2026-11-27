# Fixture strategy_actions_v1 strategy for the replay test. It acts by step
# (seconds since params.start_ms) so each action's fill and fee can be checked
# by hand: two entries, an exit revision, a gate refusal and a planner refusal.
from decimal import Decimal


def on_observation(obs):
    p = obs["params"]
    at = obs["at_ms"]
    step = (at - p["start_ms"]) // 1000
    actions = []
    if step in (0, 3):
        for key, quote in sorted(obs["quotes"].items()):
            if key in obs["positions"]:
                continue
            ask = Decimal(quote["ask"])
            tick = Decimal(obs["instruments"][key]["tick"])
            # One tick under the ask for B, so the execution gate refuses it.
            limit = ask - tick if key.endswith("B") else ask
            if step == 0:
                size = {"units": p["units"], "rupees": None}
            else:
                size = {"units": None, "rupees": p["rupees"]}
            actions.append({
                "instrument_key": key, "side": "buy", **size, "max_price": str(limit),
                "rationale": "fixture entry",
                "exit_intent": {"target": str(ask + Decimal(p["target"])),
                                "stop": str(ask - Decimal(p["stop"])),
                                "time": at + p["hold_ms"]},
            })
    elif step == 1:
        for key, held in sorted(obs["positions"].items()):
            actions.append({
                "instrument_key": key, "side": "revise_exit", "units": None,
                "rupees": None, "max_price": None, "rationale": "tighten the target",
                "exit_intent": {"target": p["revised_target"],
                                "stop": held["exit_intent"]["stop"], "time": None},
            })
    elif step == 5:
        actions.append({
            "instrument_key": "NSE_EQ|TESTA", "side": "sell", "units": 9,
            "rupees": None, "max_price": "99.95", "rationale": "already flat",
            "exit_intent": {},
        })
    return {"actions": actions}
