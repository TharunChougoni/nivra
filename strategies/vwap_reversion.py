"""VWAP mean reversion (Berkowitz, Logue and Noser, "The Total Cost of
Transactions on the NYSE", 1988, for VWAP as the intraday fair-value anchor;
the fade-the-stretch rule is the standard intraday reversion setup).

Rule: measure the stretch from session VWAP in units of lookback volatility
scaled to the hold: z = (price / vwap - 1) * 10000 / (volatility_bps *
sqrt(hold_samples)). Buy when z <= -`entry_z` and the stock is not
collapsing with the market (`market_relative_bps` >= -`max_relative_drop_bps`).
Target is VWAP itself; stop is `stop_z` further stretches below the entry,
floored at `min_stop_bps`. The distance to VWAP must clear `edge_margin`
times the round-trip cost. Skip the first `avoid_open_minutes`, when VWAP is
still unstable. Exit by `hold_ms` or `flat_minute` IST, whichever first.
Long only: the paper book holds cash equity.
"""
import json
import math
import sys
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


def run(select):
    """Shared driver: gates time and slots, then lets `select` propose entries."""
    try:
        with open(sys.argv[1], encoding='utf-8') as source:
            obs = json.load(source)
        # Params are strings or integers: the replay report allows no floats.
        p = {k: float(v) if isinstance(v, str) else v
             for k, v in (obs.get('params') or {}).items()}
        obs['params'] = p
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
    except (OSError, ValueError, KeyError, TypeError, AttributeError, ArithmeticError):
        actions = []
    print(json.dumps({'actions': actions}))



def select(obs, p):
    picks = []
    for row in obs.get('prospects') or []:
        key, f = row.get('key'), row.get('features') or {}
        minutes = num(f, 'minutes_since_open')
        price, vwap = num(f, 'price'), num(f, 'session_vwap')
        vol, rel = num(f, 'volatility_bps'), num(f, 'market_relative_bps')
        if None in (minutes, price, vwap, vol, rel) or vwap <= 0 or vol <= 0:
            continue
        if minutes < p['avoid_open_minutes'] or rel < -p['max_relative_drop_bps']:
            continue
        swing = vol * math.sqrt(p['hold_samples'])
        z = (price / vwap - 1) * 10000 / swing
        if z > -p['entry_z']:
            continue
        spread, ask = spread_bps(obs, key)
        if spread is None or spread > p['max_spread_bps']:
            continue
        target = Decimal(str(vwap))
        gap_bps = (target / ask - 1) * BPS
        if gap_bps < dec(p['edge_margin']) * round_trip_bps(obs.get('costs') or {}, spread):
            continue
        stop_bps = max(Decimal(str(swing)) * dec(p['stop_z']), dec(p['min_stop_bps']))
        picks.append((z, key, ask, target, stop_bps))
    for _, key, ask, target, stop_bps in sorted(picks):
        yield key, entry(obs, p, key, 'vwap reversion', target, ask * (1 - stop_bps / BPS))


run(select)
