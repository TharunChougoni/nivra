"""Opening gap fade on minute bars (Berkman, Koch, Tuttle and Zhang, "Paying
Attention: Overnight Returns and the Hidden Cost of Buying at the Open",
JFQA 2012; Caporale and Plastun, "Price gaps: Another market anomaly?",
Investment Analysts Journal 2017).

Long only, so this fades gap downs. On the first bar of the session, if the
open is between `min_gap_bps` and `max_gap_bps` below the previous
session's last close, buy (filling at the second bar's open). Sell when a
bar closes at or above the previous close (the gap is filled), on a close
`stop_bps` below the first bar's close, or from `flat_minute`.
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
    picks = []
    for key, o, h, l, c, v in bars:
        s = table[key]
        if key in book.held():
            entry = book.b['entry'][key]
            if (s['prev_close'] and c >= s['prev_close']) or c < entry * (1 - params['stop_bps'] / 10000.0):
                book.sell(key)
            continue
        if s['bars'] != 1 or minute > params['last_entry_minute'] or not s['prev_close']:
            continue
        gap = (s['open'] / s['prev_close'] - 1) * 10000.0
        if -params['max_gap_bps'] <= gap <= -params['min_gap_bps']:
            picks.append((gap, key))
    for _, key in sorted(picks):
        if len(book.held()) >= params['slots']:
            break
        book.buy(key, book.per_slot(params['slots']))
    return book.out
