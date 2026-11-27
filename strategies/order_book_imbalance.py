"""Order-book imbalance (Cao, Hansch and Wang, "The Information Content of an
Open Limit-Order Book", J. Futures Markets 2009; Cont, Kukanov and Stoikov,
"The Price Impact of Order Book Events", J. Financial Econometrics 2014).

Rule: `book_imbalance` is total bid quantity over total bid plus ask
quantity across the displayed book. Buy names where it is at least
`min_imbalance`, the touch is also bid-heavy (`bid_units` >= `ask_units`),
the price is at or above session VWAP and the lookback move is not
negative. Skip the first `avoid_open_minutes`. The edge is small and short
lived: the hold is `hold_ms`, the target and stop are multiples of the
lookback volatility over `hold_samples`, with floors, and the target must
clear `edge_margin` times the round-trip cost. Long only.
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
        imbalance, move = num(f, 'book_imbalance'), num(f, 'move_bps')
        vol = num(f, 'volatility_bps')
        if None in (minutes, price, vwap, imbalance, move, vol) or vol <= 0:
            continue
        if minutes < p['avoid_open_minutes'] or imbalance < p['min_imbalance']:
            continue
        if price < vwap or move < 0:
            continue
        quote = obs.get('quotes', {}).get(key) or {}
        bid_units, ask_units = quote.get('bid_units'), quote.get('ask_units')
        if type(bid_units) is not int or type(ask_units) is not int or bid_units < ask_units:
            continue
        spread, ask = spread_bps(obs, key)
        if spread is None or spread > p['max_spread_bps']:
            continue
        swing = Decimal(str(vol * math.sqrt(p['hold_samples'])))
        target_bps = max(swing * dec(p['target_vol_multiple']), dec(p['min_target_bps']))
        stop_bps = max(swing * dec(p['stop_vol_multiple']), dec(p['min_stop_bps']))
        if target_bps < dec(p['edge_margin']) * round_trip_bps(obs.get('costs') or {}, spread):
            continue
        picks.append((imbalance, key, ask, target_bps, stop_bps))
    for _, key, ask, target_bps, stop_bps in sorted(picks, reverse=True):
        yield key, entry(obs, p, key, 'bid-heavy order book',
                         ask * (1 + target_bps / BPS), ask * (1 - stop_bps / BPS))


def on_observation(obs):
    return run_select(obs, select)
