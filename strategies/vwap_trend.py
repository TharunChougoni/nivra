"""VWAP trend following (Zarattini and Aziz, "Volume Weighted Average Price
(VWAP): The Holy Grail for Day Trading Systems", SSRN 4631351, 2023;
Berkowitz, Logue and Noser, "The Total Cost of Transactions on the NYSE",
JF 1988, for VWAP as the session's fair-price benchmark).

Rule: trade with the side of VWAP the price holds. Buy names trading
between `min_above_bps` and `max_above_bps` above session VWAP (in trend,
not stretched), with a positive lookback move, at or above the market
median and in the upper half of their range. Skip the first
`avoid_open_minutes`. The stop sits `stop_below_vwap_bps` under VWAP,
where the trend is wrong; the target is a volatility multiple with a
floor, and must clear `edge_margin` times the round-trip cost. Exit by
`hold_ms` or `flat_minute` IST. Long only.
"""
import math
from decimal import ROUND_CEILING, ROUND_FLOOR, Decimal, InvalidOperation

IST_MS = 19800000
DAY_MS = 86400000
BPS = Decimal(10000)


def dec(value):
    try:
        out = Decimal(str(value))
    except (InvalidOperation, TypeError, ValueError):
        return None
    return out if out.is_finite() else None


def num(row, name):
    value = row.get(name) if isinstance(row, dict) else None
    if type(value) in (int, float) and math.isfinite(value):
        return float(value)
    return None


def to_tick(price, tick, rounding):
    return ((price / tick).to_integral_value(rounding=rounding) * tick).quantize(tick)


def ist_cutoff_ms(at_ms, minute):
    """Epoch ms of `minute` minutes after IST midnight on at_ms's IST date."""
    day = (at_ms + IST_MS) // DAY_MS * DAY_MS
    return day + minute * 60000 - IST_MS


def round_trip_bps(costs, spread_bps):
    total = Decimal(0)
    for name, times in (('buy_bps', 1), ('sell_bps', 1), ('slippage_bps', 2)):
        total += (dec(costs.get(name)) or Decimal(0)) * times
    return total + spread_bps


def entry(obs, p, key, rationale, target, stop):
    """A sized buy at the ask with tick-rounded target, stop and time exit, or None."""
    quote = obs.get('quotes', {}).get(key)
    inst = obs.get('instruments', {}).get(key)
    if not isinstance(quote, dict) or not isinstance(inst, dict):
        return None
    ask, bid, tick = dec(quote.get('ask')), dec(quote.get('bid')), dec(inst.get('tick'))
    if not ask or not bid or not tick or tick <= 0 or ask <= 0 or bid <= 0 or ask < bid:
        return None
    if target is None or stop is None:
        return None
    max_price = to_tick(ask * (1 + dec(p['headroom_bps']) / BPS), tick, ROUND_CEILING)
    target = to_tick(target, tick, ROUND_FLOOR)
    stop = to_tick(stop, tick, ROUND_FLOOR)
    if not (stop < bid and target > max_price and stop > 0):
        return None
    cash = dec(obs.get('cash')) or Decimal(0)
    budget = min(dec(p['order_rupees']), cash)
    units = int(budget // max_price)
    ask_units = quote.get('ask_units')
    if type(ask_units) is int:
        units = min(units, ask_units)
    if units < 1:
        return None
    at = obs['at_ms']
    exit_ms = min(at + int(p['hold_ms']), ist_cutoff_ms(at, int(p['flat_minute'])))
    return {
        'instrument_key': key, 'side': 'buy', 'units': units, 'rupees': None,
        'max_price': str(max_price), 'rationale': rationale,
        'exit_intent': {'target': str(target), 'stop': str(stop), 'time': exit_ms},
    }


def spread_bps(obs, key):
    quote = obs.get('quotes', {}).get(key) or {}
    ask, bid = dec(quote.get('ask')), dec(quote.get('bid'))
    if not ask or not bid or ask <= 0 or bid <= 0 or ask < bid:
        return None, None
    return (ask - bid) / ((ask + bid) / 2) * BPS, ask


def run_select(obs, select):
    """Shared driver: gates time and slots, then lets `select` propose entries."""
    try:
        if not isinstance(obs, dict):
            return {'actions': []}
        # Params are strings or integers: the replay report allows no floats.
        p = {k: float(v) if isinstance(v, str) else v
             for k, v in (obs.get('params') or {}).items()}
        obs = {**obs, 'params': p}
        at = obs.get('at_ms')
        actions = []
        if type(at) is int and at < ist_cutoff_ms(at, int(p['last_entry_minute'])):
            held = obs.get('positions') or {}
            slots = int(p['max_positions']) - len(held)
            for key, action in select(obs, p):
                if slots <= 0 or len(actions) >= 32:
                    break
                if key in held or key not in obs.get('instruments', {}):
                    continue
                if action is not None:
                    actions.append(action)
                    slots -= 1
    except (ValueError, KeyError, TypeError, AttributeError, ArithmeticError):
        actions = []
    return {'actions': actions}




def select(obs, p):
    picks = []
    for row in obs.get('prospects') or []:
        key, f = row.get('key'), row.get('features') or {}
        minutes = num(f, 'minutes_since_open')
        price, vwap = num(f, 'price'), num(f, 'session_vwap')
        rng, move = num(f, 'range_position'), num(f, 'move_bps')
        rel, vol = num(f, 'market_relative_bps'), num(f, 'volatility_bps')
        if None in (minutes, price, vwap, rng, move, rel, vol) or vol <= 0 or vwap <= 0:
            continue
        above = (price / vwap - 1) * 10000
        if minutes < p['avoid_open_minutes'] or not p['min_above_bps'] <= above <= p['max_above_bps']:
            continue
        if move <= 0 or rel < 0 or rng < p['min_range_position']:
            continue
        spread, ask = spread_bps(obs, key)
        if spread is None or spread > p['max_spread_bps']:
            continue
        swing = Decimal(str(vol * math.sqrt(p['hold_samples'])))
        target_bps = max(swing * dec(p['target_vol_multiple']), dec(p['min_target_bps']))
        if target_bps < dec(p['edge_margin']) * round_trip_bps(obs.get('costs') or {}, spread):
            continue
        stop = Decimal(str(vwap)) * (1 - dec(p['stop_below_vwap_bps']) / BPS)
        picks.append((rel, key, ask, target_bps, stop))
    for _, key, ask, target_bps, stop in sorted(picks, reverse=True):
        yield key, entry(obs, p, key, 'above vwap trend', ask * (1 + target_bps / BPS), stop)


def on_observation(obs):
    return run_select(obs, select)
