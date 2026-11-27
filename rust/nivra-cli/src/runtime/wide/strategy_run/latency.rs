use super::*;

const CALL_WINDOW: i64 = 256;
const TABLE: &str =
    "CREATE TABLE IF NOT EXISTS strategy_call_timing(id INTEGER PRIMARY KEY, body TEXT NOT NULL);";

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) struct QuoteTime {
    source_at_ms: i64,
    received_at_ms: i64,
}

impl From<&Quote> for QuoteTime {
    fn from(quote: &Quote) -> Self {
        Self {
            source_at_ms: quote.source,
            received_at_ms: quote.receipt,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct GateTime {
    pub index: usize,
    pub quote: QuoteTime,
    pub decided_at_ms: i64,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Timing {
    strategy_id: String,
    observed_at_ms: i64,
    /// Newest executable receipt in the observation. Each gate sample below
    /// carries the exact quote for its action.
    quote: Option<QuoteTime>,
    pub submitted_at_ms: Option<i64>,
    pub reply_received_at_ms: Option<i64>,
    pub book_locked_at_ms: Option<i64>,
    pub gates: Vec<GateTime>,
    pub outcome: String,
}

impl Timing {
    pub(super) fn new(
        strategy_id: String,
        observed_at_ms: i64,
        quotes: &BTreeMap<String, Quote>,
    ) -> Self {
        Self {
            strategy_id,
            observed_at_ms,
            quote: quotes
                .values()
                .max_by_key(|q| (q.receipt, q.source))
                .map(QuoteTime::from),
            submitted_at_ms: None,
            reply_received_at_ms: None,
            book_locked_at_ms: None,
            gates: Vec::new(),
            outcome: String::new(),
        }
    }

    pub(super) fn record(&self, db: &Connection) -> Result<()> {
        db.execute_batch(TABLE)?;
        db.execute(
            "INSERT INTO strategy_call_timing(body) VALUES(?)",
            [serde_json::to_string(self)?],
        )?;
        db.execute(
            "DELETE FROM strategy_call_timing WHERE id <= last_insert_rowid() - ?",
            [CALL_WINDOW],
        )?;
        Ok(())
    }
}

pub(super) fn summary(db: &Connection) -> Result<Value> {
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='strategy_call_timing')",
        [],
        |row| row.get(0),
    )?;
    let mut hops: [(&str, Vec<i64>); 8] = [
        ("quote_source_to_receipt", Vec::new()),
        ("receipt_to_observation", Vec::new()),
        ("observation_to_submit", Vec::new()),
        ("submit_to_reply", Vec::new()),
        ("reply_to_book_lock", Vec::new()),
        ("book_lock_to_gate", Vec::new()),
        ("reply_to_gate", Vec::new()),
        ("quote_source_to_gate", Vec::new()),
    ];
    let mut calls = 0;
    if present {
        let mut query =
            db.prepare("SELECT body FROM strategy_call_timing ORDER BY id DESC LIMIT ?")?;
        let rows = query.query_map([CALL_WINDOW], |row| row.get::<_, String>(0))?;
        for row in rows {
            let call: Timing = serde_json::from_str(&row?)?;
            calls += 1;
            let mut add = |hop: usize, start: Option<i64>, end: Option<i64>| {
                if let Some(ms) = start
                    .zip(end)
                    .and_then(|(a, b)| b.checked_sub(a))
                    .filter(|&ms| ms >= 0)
                {
                    hops[hop].1.push(ms);
                }
            };
            if let Some(quote) = call.quote {
                add(0, Some(quote.source_at_ms), Some(quote.received_at_ms));
                add(1, Some(quote.received_at_ms), Some(call.observed_at_ms));
            }
            add(2, Some(call.observed_at_ms), call.submitted_at_ms);
            add(3, call.submitted_at_ms, call.reply_received_at_ms);
            add(4, call.reply_received_at_ms, call.book_locked_at_ms);
            for gate in call.gates {
                add(5, call.book_locked_at_ms, Some(gate.decided_at_ms));
                add(6, call.reply_received_at_ms, Some(gate.decided_at_ms));
                add(7, Some(gate.quote.source_at_ms), Some(gate.decided_at_ms));
            }
        }
    }
    let mut result = json!({"calls": calls, "window_calls": CALL_WINDOW});
    for (name, mut samples) in hops {
        samples.sort_unstable();
        let count = samples.len();
        let percentile = |n: usize| {
            count
                .checked_mul(n)
                .map(|rank| rank.div_ceil(100))
                .and_then(|rank| rank.checked_sub(1))
                .map(|index| samples[index])
        };
        result[name] = json!({
            "samples": count,
            "p50_ms": percentile(50),
            "p95_ms": percentile(95),
            "p99_ms": percentile(99),
        });
    }
    Ok(result)
}
