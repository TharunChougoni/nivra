"""Volume-climax reversal (Campbell, Grossman and Wang, "Trading Volume and
Serial Correlation in Stock Returns", QJE 1993; Avramov, Chordia and Goyal,
"Liquidity and Autocorrelations in Individual Stock Returns", JF 2006).

Rule: a sharp drop on heavy volume that is not shared by the market is
often liquidity selling, which reverses. Buy names whose lookback move is
at most `-min_drop_bps`, whose market-relative move is at most
`-min_relative_drop_bps`, on relative volume >= `min_rel_volume`, at the
bottom of their range (`range_position` <= `max_range_position`) and below
VWAP. Skip the first `avoid_open_minutes`. The target recovers
`target_retrace` of the drop, capped at VWAP; the stop is a volatility
multiple with a floor. The target must clear `edge_margin` times the
round-trip cost. Long only.
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
    """A buy at the ask with tick-rounded target, stop and time exit, or None.

    The runtime sizes it: the stop sets the risk per share, and the cash
    sleeve's policy (`obs.sizing`) sets the risk budget and position cap.
    """
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
    at = obs['at_ms']
    exit_ms = min(at + int(p['hold_ms']), ist_cutoff_ms(at, int(p['flat_minute'])))
    return {
        'instrument_key': key, 'side': 'buy',
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
            book = obs['book']
            slots = int(book['max_positions']) - int(book['open_positions'])
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
        rvol = num(f, 'rel_volume_tod')
        if None in (minutes, price, vwap, rng, move, rel, vol, rvol) or vol <= 0:
            continue
        if minutes < p['avoid_open_minutes'] or move > -p['min_drop_bps']:
            continue
        if rel > -p['min_relative_drop_bps'] or rvol < p['min_rel_volume']:
            continue
        if rng > p['max_range_position'] or price >= vwap:
            continue
        spread, ask = spread_bps(obs, key)
        if spread is None or spread > p['max_spread_bps']:
            continue
        retrace = Decimal(str(-move)) * dec(p['target_retrace'])
        target = min(ask * (1 + retrace / BPS), Decimal(str(vwap)))
        gap_bps = (target / ask - 1) * BPS
        if gap_bps < dec(p['edge_margin']) * round_trip_bps(obs.get('costs') or {}, spread):
            continue
        swing = Decimal(str(vol * math.sqrt(p['hold_samples'])))
        stop_bps = max(swing * dec(p['stop_vol_multiple']), dec(p['min_stop_bps']))
        picks.append((rvol * -move, key, ask, target, stop_bps))
    for _, key, ask, target, stop_bps in sorted(picks, reverse=True):
        yield key, entry(obs, p, key, 'volume climax reversal', target,
                         ask * (1 - stop_bps / BPS))


def on_observation(obs):
    return run_select(obs, select)
