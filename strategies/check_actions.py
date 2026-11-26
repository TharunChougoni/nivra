"""Check every strategy_actions_v1 script in this directory.

Run: python3 strategies/check_actions.py

Each strategy runs as the sandbox runs it (`python3 -I -S -B`). Empty,
malformed and untradable observations must print `{"actions": []}` and exit
0. A trigger observation, built to meet the strategy's own rule, must yield
exactly one well-formed buy whose target is above `max_price`, whose stop
is below the bid and whose time exit is in the future.
"""
import json
import pathlib
import subprocess
import sys
import tempfile
from decimal import Decimal

HERE = pathlib.Path(__file__).resolve().parent
KEY = 'NSE_EQ|INE000000001'
AT_MS = 1795066200000  # 2026-11-19 11:00 IST
COSTS = {'per_order': '0.01', 'buy_bps': '5', 'sell_bps': '7', 'slippage_bps': '5',
         'rounding_increment': '0.01'}
BASE = {
    'price': 500.0, 'move_bps': 0.0, 'market_relative_bps': 0.0, 'traded_value': 5e7,
    'circuit_distance_bps': 1000.0, 'volatility_bps': 20.0, 'range_position': 0.5,
    'best_bid': 499.95, 'best_ask': 500.0, 'best_bid_qty': 500.0, 'best_ask_qty': 500.0,
    'spread_fraction': 0.0001, 'book_imbalance': 0.5, 'session_vwap': 500.0,
    'rel_volume_tod': 1.0, 'minutes_since_open': 105.0,
}
# Feature overrides that satisfy each strategy's entry rule at price 500.
TRIGGERS = {
    'opening_range_breakout': {'minutes_since_open': 30.0, 'range_position': 0.95,
                               'move_bps': 80.0, 'session_vwap': 495.0,
                               'rel_volume_tod': 2.0},
    'vwap_reversion': {'session_vwap': 510.0, 'volatility_bps': 5.0},
    'relative_strength': {'market_relative_bps': 120.0, 'move_bps': 150.0,
                          'session_vwap': 495.0, 'rel_volume_tod': 1.5},
    'bollinger_reversion': {'move_bps': -200.0, 'range_position': 0.05,
                            'market_relative_bps': -150.0},
    'order_book_imbalance': {'book_imbalance': 0.8, 'move_bps': 20.0,
                             'session_vwap': 499.0},
    'circuit_magnet': {'move_bps': 300.0, 'range_position': 0.95, 'session_vwap': 490.0,
                       'market_relative_bps': 250.0, 'circuit_distance_bps': 250.0},
    'volume_climax_reversal': {'move_bps': -300.0, 'market_relative_bps': -250.0,
                               'rel_volume_tod': 3.0, 'range_position': 0.05,
                               'session_vwap': 510.0},
    'vwap_trend': {'session_vwap': 498.0, 'move_bps': 60.0, 'market_relative_bps': 30.0,
                   'range_position': 0.7},
}


def observation(params, features, bid='499.95', ask='500.00', bid_units=900):
    return {
        'abi': 'strategy_actions_v1', 'at_ms': AT_MS, 'params': params,
        'prospects': [{'key': KEY, 'rank': 1, 'score': 1.0, 'features': features}],
        'quotes': {KEY: {'bid': bid, 'ask': ask, 'bid_units': bid_units, 'ask_units': 600,
                         'source': 'fixture', 'receipt': AT_MS - 500}},
        'positions': {}, 'instruments': {KEY: {'symbol': 'FIXTURE', 'tick': '0.05'}},
        'cash': '100000', 'costs': COSTS,
    }


def run(script, payload):
    with tempfile.TemporaryDirectory() as tmp:
        path = pathlib.Path(tmp) / 'obs.json'
        path.write_text(payload if isinstance(payload, str) else json.dumps(payload))
        args = [sys.executable, '-I', '-S', '-B', str(script), str(path)]
        done = subprocess.run(args, capture_output=True, text=True, timeout=10, cwd=tmp)
    if done.returncode != 0:
        raise AssertionError(f'exit {done.returncode}: {done.stderr.strip()[-300:]}')
    return json.loads(done.stdout)['actions']


def check(name):
    script = HERE / f'{name}.py'
    params = json.loads((HERE / f'{name}.params.json').read_text())['params']
    garbage = {k: 'x' for k in BASE} | {'price': float('nan')}
    quiet = {
        'not json': '{"abi":',
        'top-level array': [],
        'empty object': {},
        'params only': {'params': params},
        'no prospects': observation(params, BASE) | {'prospects': []},
        'garbage features': observation(params, garbage | TRIGGERS.get(name, {}) | {'price': 'x'}),
        'crossed quote': observation(params, BASE | TRIGGERS.get(name, {}),
                                     bid='501.00', ask='500.00'),
        'after last entry': observation(params, BASE | TRIGGERS.get(name, {})) | {
            'at_ms': AT_MS + (int(params['last_entry_minute']) - 660) * 60000 + 60000},
        'no cash': observation(params, BASE | TRIGGERS.get(name, {})) | {'cash': '0'},
    }
    for label, payload in quiet.items():
        actions = run(script, payload)
        assert actions == [], f'{label}: expected no actions, got {actions}'
    if name not in TRIGGERS:
        return 'no trigger fixture'
    actions = run(script, observation(params, BASE | TRIGGERS[name]))
    assert len(actions) == 1, f'trigger: expected one buy, got {actions}'
    buy = actions[0]
    intent = buy['exit_intent']
    assert buy['side'] == 'buy' and buy['instrument_key'] == KEY and buy['rupees'] is None
    assert type(buy['units']) is int and buy['units'] >= 1
    max_price, target, stop = (Decimal(buy['max_price']), Decimal(intent['target']),
                               Decimal(intent['stop']))
    assert target > max_price >= Decimal('500.00') and Decimal(0) < stop < Decimal('499.95')
    assert buy['units'] * max_price <= Decimal(params['order_rupees'])
    assert AT_MS < intent['time'], 'time exit must be in the future'
    return f"buy {buy['units']} @ <= {max_price}, target {target}, stop {stop}"


def main():
    names = sorted(p.name[:-len('.params.json')] for p in HERE.glob('*.params.json')
                   if json.loads(p.read_text()).get('version') == 1)
    failed = 0
    for name in names:
        try:
            print(f'ok   {name}: {check(name)}')
        except AssertionError as error:
            failed += 1
            print(f'FAIL {name}: {error}')
    print(f'{len(names) - failed}/{len(names)} action strategies pass')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
