"""Offline scores-only port of the cost-aware momentum research scorer.

argv[1] is the sandbox input file: {"params": {...}, instrument: {feature: number}}.
Feature names are the core Feature enum's snake_case JSON names. Supply every
parameter from cost_aware_momentum.params.json (the documented default policy);
this file needs no adjacent files inside the sandbox. `params` is reserved and
is never emitted. Ineligible instruments are omitted, as allowed by the sandbox.
Book imbalance and both top quantities are required depth evidence; imbalance
has no weight in the original research formula. RelVolumeTod is the prepared
power-curve proxy, not measured same-minute historical relative volume.
"""
import json
import math
import statistics
import sys


FEATURES = (
    'price', 'move_bps', 'market_relative_bps', 'traded_value',
    'circuit_distance_bps', 'volatility_bps', 'range_position',
    'best_bid', 'best_ask', 'best_bid_qty', 'best_ask_qty',
    'spread_fraction', 'book_imbalance', 'session_vwap',
    'rel_volume_tod', 'minutes_since_open',
)
PARAMETERS = (
    'min_price', 'min_traded_value', 'min_circuit_distance_bps',
    'max_spread_bps', 'min_depth_multiple', 'order_notional',
    'per_order_fee', 'buy_bps', 'sell_bps', 'slippage_bps',
    'edge_margin', 'horizon_samples', 'vol_capture', 'min_rel_volume',
    'max_extension_bps', 'avoid_open_minutes', 'avoid_close_minutes',
    'session_minutes', 'w_rel_strength', 'w_vwap', 'w_rel_volume',
    'w_range', 'w_extension_penalty', 'weak_tape_vol_multiple',
    'normalization_floor_bps', 'max_strength_term', 'max_log_rel_volume',
)


def number(value):
    return type(value) in (int, float) and math.isfinite(value)


def valid_features(f):
    return (isinstance(f, dict)
            and all(name in f and number(f[name]) for name in FEATURES)
            and all(f[name] > 0 for name in (
                'price', 'best_bid', 'best_ask', 'best_bid_qty',
                'best_ask_qty', 'session_vwap', 'rel_volume_tod'))
            and f['best_ask'] >= f['best_bid']
            and f['volatility_bps'] >= 0 and f['traded_value'] >= 0
            and f['circuit_distance_bps'] >= 0 and f['minutes_since_open'] >= 0
            and f['spread_fraction'] >= 0
            and 0 <= f['range_position'] <= 1
            and -1 <= f['book_imbalance'] <= 1)


def score_one(f, p, regime_up):
    price = f['price']
    # Use both the supplied fraction and displayed prices conservatively.
    spread = max(f['spread_fraction'] * 10000,
                 (f['best_ask'] - f['best_bid']) /
                 ((f['best_ask'] + f['best_bid']) / 2) * 10000)
    vol = f['volatility_bps']
    vwap_bps = (price / f['session_vwap'] - 1) * 10000
    if (price < p['min_price']
            or f['traded_value'] < p['min_traded_value']
            or f['circuit_distance_bps'] < p['min_circuit_distance_bps']
            or spread > p['max_spread_bps']
            or f['best_ask_qty'] * f['best_ask'] < p['min_depth_multiple'] * p['order_notional']
            or f['minutes_since_open'] < p['avoid_open_minutes']
            or f['minutes_since_open'] > p['session_minutes'] - p['avoid_close_minutes']
            or f['rel_volume_tod'] < p['min_rel_volume']
            or f['market_relative_bps'] <= 0
            or (not regime_up and f['market_relative_bps'] < p['weak_tape_vol_multiple'] * vol)
            or vwap_bps <= 0):
        return None
    expected = p['vol_capture'] * vol * math.sqrt(p['horizon_samples'])
    cost = (2 * p['per_order_fee'] / p['order_notional'] * 10000
            + p['buy_bps'] + p['sell_bps'] + 2 * p['slippage_bps'] + spread)
    if expected < p['edge_margin'] * cost:
        return None
    scale = max(vol, p['normalization_floor_bps'])
    return ((expected - cost) / 100
            + p['w_rel_strength'] * min(f['market_relative_bps'] / scale, p['max_strength_term'])
            + p['w_vwap'] * min(vwap_bps / scale, p['max_strength_term'])
            + p['w_rel_volume'] * min(max(math.log(f['rel_volume_tod']), 0), p['max_log_rel_volume'])
            + p['w_range'] * f['range_position']
            - p['w_extension_penalty'] * max(0, f['move_bps'] - p['max_extension_bps']) / 100)


def score(snapshot):
    p = snapshot['params']
    if (not isinstance(p, dict) or set(p) != set(PARAMETERS)
            or not all(number(v) and v >= 0 for v in p.values())
            or any(p[k] <= 0 for k in ('order_notional', 'normalization_floor_bps', 'session_minutes'))
            or p['avoid_open_minutes'] + p['avoid_close_minutes'] > p['session_minutes']):
        raise ValueError('invalid scorer params')
    rows = {k: f for k, f in snapshot.items() if k != 'params' and valid_features(f)}
    if not rows:
        return {}
    regime_up = statistics.median(f['move_bps'] for f in rows.values()) > 0
    out = {}
    for key, features in rows.items():
        value = score_one(features, p, regime_up)
        if value is not None and math.isfinite(value):
            out[key] = value
    return out


if __name__ == '__main__':
    with open(sys.argv[1], encoding='utf-8') as source:
        print(json.dumps(score(json.load(source)), allow_nan=False))
