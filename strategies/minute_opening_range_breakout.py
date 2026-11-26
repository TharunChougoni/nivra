"""Opening-range breakout on minute bars (Crabel, "Day Trading with Short Term
Price Patterns and Opening Range Breakout", 1990; Zarattini, Barbon and
Aziz, "A Profitable Day Trading Strategy For The U.S. Equity Market", SSRN
4729284, 2024, which trades only "stocks in play").

The opening range is the first `range_minutes` bars. Until
`last_entry_minute`, buy a name the first time a bar closes above the
range high by `buffer_bps`, if its opening-range volume is at least
`min_rel_volume` times its average over the last `volume_days` sessions.
Sell on a close below the range low, or from `flat_minute` (15:14 IST, so
the exit fills at 15:15). One entry per name per day, at most `slots`.
"""

# ---- shared minute-bar helpers (identical in every minute bar strategy) ----
# Bars are [key, open, high, low, close, volume] in paise, one per minute.
# A signal on the bar starting at minute m fills at the next observed open,
# so rules read only bars that have closed. Minutes count from IST midnight:
# 555 is 09:15, the first NSE cash bar, and 929 is 15:29, the last.
IST_MS = 19800000
DAY_MS = 86400000


def clock(at_ms):
    ist = at_ms + IST_MS
    return ist // DAY_MS, ist % DAY_MS // 60000


def sessions(day, bars, state):
    """Per-key session state: today's open, high, low, volume and minute
    count, plus the previous session's last close."""
    table = state.setdefault('sessions', {})
    for key, o, h, l, c, v in bars:
        s = table.get(key)
        if s is None or s['day'] != day:
            prev = s['close'] if s else None
            s = table[key] = {'day': day, 'prev_close': prev, 'open': o, 'high': h,
                              'low': l, 'volume': 0, 'bars': 0, 'done': False}
        s['high'] = max(s['high'], h)
        s['low'] = min(s['low'], l)
        s['close'] = c
        s['volume'] += v
        s['bars'] += 1
    return table


class Book:
    """The strategy's estimate of the Rust paper book for this split. Rust
    resets the book per split and never reports fills, so this tracks
    intended holdings and estimated cash. Sale proceeds count as cash from
    the next bar, because same-timestamp fills compete for cash in key
    order."""

    def __init__(self, split, bars, params, state):
        book = state.get('book')
        if book is None or book['split'] != split:
            book = state['book'] = {'split': split, 'cash': float(params['capital_rupees']) * 100.0,
                                    'incoming': 0.0, 'held': {}, 'entry': {}, 'exits': set()}
        else:
            book['cash'] += book['incoming']
            book['incoming'] = 0.0
        self.b = book
        self.cost = params['cost_bps'] / 10000.0
        self.rows = {row[0]: (i, row) for i, row in enumerate(bars)}
        self.used = set()
        self.out = []

    def held(self):
        return self.b['held']

    def sell(self, key):
        if key not in self.b['held']:
            return
        found = self.rows.get(key)
        if found is None or found[0] in self.used:
            self.b['exits'].add(key)
            return
        units = self.b['held'].pop(key)
        self.b['entry'].pop(key, None)
        self.b['exits'].discard(key)
        self.b['incoming'] += units * found[1][4] * (1 - self.cost)
        self.used.add(found[0])
        self.out.append([found[0], 0])

    def buy(self, key, rupees):
        found = self.rows.get(key)
        if found is None or found[0] in self.used or key in self.b['held'] or key in self.b['exits']:
            return False
        price = found[1][4] * (1 + self.cost)
        units = int(min(rupees * 100.0, self.b['cash']) // price)
        if units < 1:
            return False
        self.b['cash'] -= units * price
        self.b['held'][key] = units
        self.b['entry'][key] = found[1][4]
        self.used.add(found[0])
        self.out.append([found[0], units])
        return True

    def per_slot(self, slots):
        return (self.b['cash'] + self.b['incoming']) / 100.0 / max(slots - len(self.b['held']), 1)

    def housekeeping(self, minute, params):
        """Retry deferred exits; flatten everything from `flat_minute`."""
        for key in list(self.b['exits']):
            self.b['exits'].discard(key)
            self.sell(key)
        if minute >= params['flat_minute']:
            for key in list(self.b['held']):
                self.sell(key)
            return True
        return False
# ---- end shared helpers ----


def on_bars(at_ms, split, bars, params, state):
    day, minute = clock(at_ms)
    table = sessions(day, bars, state)
    book = Book(split, bars, params, state)
    if book.housekeeping(minute, params):
        return book.out
    history = state.setdefault('range_volume', {})
    range_end = 555 + params['range_minutes']
    for key, o, h, l, c, v in bars:
        s = table[key]
        if minute < range_end:
            continue
        if 'range_high' not in s:
            s['range_high'], s['range_low'], s['range_volume'] = s['high'], s['low'], s['volume']
            past = history.setdefault(key, [])
            s['rel_volume'] = s['range_volume'] / (sum(past) / len(past)) if past else None
            past.append(s['range_volume'])
            del past[:-params['volume_days']]
        if key in book.held():
            if c < s['range_low']:
                book.sell(key)
            continue
        if (s['done'] or minute > params['last_entry_minute'] or s['rel_volume'] is None
                or s['rel_volume'] < params['min_rel_volume']):
            continue
        if c > s['range_high'] * (1 + params['buffer_bps'] / 10000.0):
            s['done'] = True
            if len(book.held()) < params['slots']:
                book.buy(key, book.per_slot(params['slots']))
    return book.out
