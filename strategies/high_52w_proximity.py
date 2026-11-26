"""52-week-high proximity (George and Hwang, "The 52-Week High and Momentum
Investing", JF 2004).

Each `rebalance_bars` bars, rank liquid names by close divided by the
highest high of the last `window_bars` bars (at least `min_bars`) and hold
the `slots` names nearest their high. The archive holds about 13 months, so
the window is the available history up to 250 bars rather than a full year.
"""

# ---- shared daily-bar helpers (identical in every daily bar strategy) ----
# Bars are [key, open, high, low, close, volume] in paise. A signal on bar t
# fills at the next observed open, so every rule below reads only bars <= t.
from collections import deque


def history(bars, state, params):
    """Append today's bars. Restart a key's history on a bad print or an
    overnight jump beyond `max_gap` (NSE price bands stop ordinary moves
    near 20%, so a bigger jump is usually an unadjusted corporate action)."""
    hist = state.setdefault('hist', {})
    jumped = []
    for key, o, h, l, c, v in bars:
        rows = hist.get(key)
        if rows and (o <= 0 or c <= 0 or abs(o / rows[-1][3] - 1) > params['max_gap']):
            rows = None
            jumped.append(key)
        if rows is None:
            rows = hist[key] = deque(maxlen=params['history_bars'])
        rows.append((o, h, l, c, v))
    return hist, jumped


class Book:
    """The strategy's estimate of the Rust paper book for this split. Rust
    resets the book per split and never reports fills, so this tracks
    intended holdings and estimated cash. Sale proceeds count as cash only
    from the next bar, because same-timestamp fills compete for cash in key
    order and a buy may settle before the sale that funds it."""

    def __init__(self, split, bars, hist, params, state):
        book = state.get('book')
        if book is None or book['split'] != split:
            book = state['book'] = {
                'split': split, 'cash': float(params['capital_rupees']) * 100.0,
                'incoming': 0.0, 'held': {}, 'entry': {}, 'age': {}, 'exits': set(),
                'wanted': [], 'bar': 0,
            }
        else:
            book['cash'] += book['incoming']
            book['incoming'] = 0.0
        book['bar'] += 1
        self.b = book
        self.hist = hist
        self.cost = params['cost_bps'] / 10000.0
        self.index = {row[0]: i for i, row in enumerate(bars)}
        self.used = set()
        self.out = []
        for key in book['held']:
            if key in self.index:
                book['age'][key] = book['age'].get(key, 0) + 1

    def held(self):
        return self.b['held']

    def close(self, key):
        return self.hist[key][-1][3]

    def equity(self):
        value = self.b['cash'] + self.b['incoming']
        for key, units in self.b['held'].items():
            rows = self.hist.get(key)
            if rows:
                value += units * rows[-1][3]
        return value

    def sell(self, key):
        if key not in self.b['held']:
            return
        row = self.index.get(key)
        if row is None or row in self.used:
            self.b['exits'].add(key)
            return
        units = self.b['held'].pop(key)
        self.b['entry'].pop(key, None)
        self.b['age'].pop(key, None)
        self.b['exits'].discard(key)
        self.b['incoming'] += units * self.close(key) * (1 - self.cost)
        self.used.add(row)
        self.out.append([row, 0])

    def buy(self, key, rupees):
        """Enter `rupees` of `key` at about today's close; False when it cannot."""
        row = self.index.get(key)
        if row is None or row in self.used or key in self.b['held'] or key in self.b['exits']:
            return False
        price = self.close(key) * (1 + self.cost)
        units = int(min(rupees * 100.0, self.b['cash']) // price)
        if units < 1:
            return False
        self.b['cash'] -= units * price
        self.b['held'][key] = units
        self.b['entry'][key] = self.close(key)
        self.b['age'][key] = 0
        self.used.add(row)
        self.out.append([row, units])
        return True

    def retry_exits(self):
        for key in list(self.b['exits']):
            self.b['exits'].discard(key)
            self.sell(key)

    def rebalance(self, desired, slots):
        """Hold exactly `desired` (best first), equal weight over `slots`.
        Buys that cash cannot cover yet are retried on later bars."""
        keep = set(desired)
        for key in list(self.b['held']):
            if key not in keep:
                self.sell(key)
        self.b['wanted'] = list(desired)
        self.top_up(slots)

    def top_up(self, slots):
        per = self.equity() / 100.0 / slots
        for key in self.b['wanted']:
            if len(self.b['held']) >= slots:
                break
            if key not in self.b['held']:
                self.buy(key, per)


def adv_rupees(rows, n):
    tail = list(rows)[-n:]
    return sum(r[3] * r[4] for r in tail) / len(tail) / 100.0


def liquid(rows, params, need):
    return (len(rows) >= need and rows[-1][3] >= params['min_price_rupees'] * 100
            and adv_rupees(rows, 20) >= params['min_adv_rupees'])


def sma(closes, n):
    return sum(closes[-n:]) / n


def stdev_returns(closes, n):
    tail = closes[-n - 1:]
    rets = [tail[i] / tail[i - 1] - 1 for i in range(1, len(tail))]
    mean = sum(rets) / len(rets)
    return (sum((r - mean) ** 2 for r in rets) / max(len(rets) - 1, 1)) ** 0.5
# ---- end shared helpers ----


def on_bars(at_ms, split, bars, params, state):
    hist, jumped = history(bars, state, params)
    book = Book(split, bars, hist, params, state)
    for key in jumped:
        book.sell(key)
    book.retry_exits()
    if book.b['bar'] % params['rebalance_bars'] != 1:
        book.top_up(params['slots'])
        return book.out
    scored = []
    for key, rows in hist.items():
        if key in book.index and liquid(rows, params, params['min_bars']):
            high = max(r[1] for r in rows)
            scored.append((rows[-1][3] / high, key))
    scored.sort(reverse=True)
    book.rebalance([key for _, key in scored[:params['slots']]], params['slots'])
    return book.out
