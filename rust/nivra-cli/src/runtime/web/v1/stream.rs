use super::*;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(crate) enum EventSource {
    Global,
    #[cfg(test)]
    Test(Arc<TestFeed>),
}
impl EventSource {
    pub(crate) fn from_ctx(_ctx: &Ctx) -> Self {
        #[cfg(test)]
        if let Some(feed) = &_ctx.event_feed {
            return Self::Test(feed.clone());
        }
        Self::Global
    }
    pub(crate) fn snapshot(&self, after: u64) -> serde_json::Value {
        match self {
            Self::Global => nivra_cli::event_feed::snapshot(after),
            #[cfg(test)]
            Self::Test(feed) => feed.snapshot(after),
        }
    }
    pub(crate) fn wait(&self, after: u64, timeout: Duration) {
        match self {
            Self::Global => nivra_cli::event_feed::wait_after(after, timeout),
            #[cfg(test)]
            Self::Test(feed) => feed.wait(after, timeout),
        }
    }
}
pub(crate) fn event_cursor(
    head: &str,
    query: &router::Query,
) -> std::result::Result<u64, &'static str> {
    if [
        "last-event-id",
        "accept",
        "if-none-match",
        "accept-encoding",
    ]
    .iter()
    .any(|key| values(head, key).len() > 1)
    {
        return Err("duplicate_event_header");
    }
    if query.get("poll").is_some_and(|value| value != "1") {
        return Err("event_poll_invalid");
    }
    let parse = |value: &str| {
        value
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let headers = values(head, "last-event-id");
    let header = headers
        .first()
        .map(|v| parse(v).ok_or("event_cursor_invalid"))
        .transpose()?;
    let query = query
        .get("after")
        .map(|v| parse(v).ok_or("event_cursor_invalid"))
        .transpose()?;
    if header.zip(query).is_some_and(|(a, b)| a != b) {
        return Err("event_cursor_conflict");
    }
    Ok(query.or(header).unwrap_or(0))
}
pub(crate) fn held_events(
    source: &EventSource,
    after: u64,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let data = source.snapshot(after);
        if data["unavailable"] == true {
            // The in-process ring lock is briefly held by a publisher. Retry
            // within the original hold window instead of failing the stream.
            if Instant::now() >= deadline {
                return data;
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        if data["gap"] == true
            || data["events"].as_array().is_some_and(|v| !v.is_empty())
            || Instant::now() >= deadline
        {
            return data;
        }
        source.wait(after, deadline.saturating_duration_since(Instant::now()));
    }
}
pub(crate) fn poll_source(
    head: &str,
    query: router::Query,
    source: EventSource,
    settings: &DashboardSettings,
) -> Response {
    let after = match event_cursor(head, &query) {
        Ok(after) => after,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let data = held_events(
        &source,
        after,
        Duration::from_millis(settings.longpoll_hold_ms),
    );
    if data["unavailable"] == true {
        return error("503 Service Unavailable", "events_unavailable");
    }
    let body = super::super::super::transcript::redact(
        &serde_json::json!({"v":1,"events":data["events"],"next":data["next"],"gap":data["gap"],"epoch":data["epoch"]}),
    );
    response("200 OK", "application/json", body.to_string())
}
pub(crate) fn poll_events(head: &str, ctx: &Ctx) -> Response {
    let reply = (|| {
        if !local_host(
            head,
            ctx.public_host.as_ref().map(DashboardPublicHost::as_str),
        ) {
            return error("403 Forbidden", "local_host_required");
        }
        if !authorized(
            head,
            &ctx.token,
            ctx.public_host.as_ref().map(DashboardPublicHost::as_str),
        ) {
            if let Some(retry_after) = super::super::record_failed_auth(&ctx.settings) {
                let mut reply = error("429 Too Many Requests", "failed_auth_rate_limited");
                reply.retry_after = Some(retry_after);
                return reply;
            }
            return error("401 Unauthorized", "authentication_required");
        }
        let Ok(Route::Events(query)) = router::parse(head.lines().next().unwrap_or_default())
        else {
            return error("400 Bad Request", "event_query_invalid");
        };
        poll_source(head, query, EventSource::from_ctx(ctx), &ctx.settings)
    })();
    negotiate(reply, head, &ctx.settings)
}
pub(crate) fn wants_sse(head: &str) -> bool {
    let Ok(Route::Events(query)) = router::parse(head.lines().next().unwrap_or_default()) else {
        return false;
    };
    query.get("poll") != Some("1")
        && values(head, "accept").iter().any(|value| {
            value
                .split(',')
                .any(|value| value.trim() == "text/event-stream")
        })
}
pub(crate) struct StreamPermit(Arc<AtomicUsize>);
impl Drop for StreamPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(crate) fn sse_frame(event: &serde_json::Value, epoch: &serde_json::Value) -> Option<Vec<u8>> {
    let seq = event["seq"].as_u64()?;
    let kind = match event["kind"].as_str() {
        Some("run_started") => "run_started",
        Some("run_ended") => "run_ended",
        Some("summary_changed") => "summary_changed",
        _ => "run_event",
    };
    let body = if kind == "summary_changed" {
        serde_json::json!({"etag":event["etag"],"revisions":event["revisions"]})
    } else {
        let mut body = event.clone();
        body["epoch"] = epoch.clone();
        super::super::super::transcript::redact(&body)
    };
    Some(format!("id: {seq}\nevent: {kind}\ndata: {body}\n\n").into_bytes())
}
/// One SSE connection's byte sink: plain, or a single gzip stream with a sync
/// flush after every frame, so each frame reaches the client whole.
pub(crate) enum FrameWriter<W: Write> {
    Plain(W),
    Gzip(flate2::write::GzEncoder<W>),
}
impl<W: Write> FrameWriter<W> {
    pub(crate) fn new(sink: W, gzip: bool) -> Self {
        if gzip {
            Self::Gzip(flate2::write::GzEncoder::new(
                sink,
                flate2::Compression::default(),
            ))
        } else {
            Self::Plain(sink)
        }
    }
    pub(crate) fn frame(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Plain(sink) => sink.write_all(bytes),
            Self::Gzip(encoder) => {
                encoder.write_all(bytes)?;
                encoder.flush()
            }
        }
    }
}
/// The latest display price batch, built at most once per interval and
/// shared by every stream. A stream that sent the previous batch gets only
/// the instruments whose receipt time moved; any other stream gets the full
/// snapshot. `batch_seq` advances only when a batch carries a change, so a
/// skipped sequence still means the client missed prices.
#[derive(Default)]
pub(crate) struct PriceBatches(Mutex<BatchState>);
#[derive(Default)]
struct BatchState {
    /// Last issued sequence; it never restarts, even across sessions.
    seq: u64,
    batch: Option<PriceBatch>,
}
struct PriceBatch {
    seq: u64,
    built: Instant,
    session: Option<String>,
    receipts: BTreeMap<String, i64>,
    full: Arc<[u8]>,
    delta: Arc<[u8]>,
}
impl PriceBatches {
    /// The frame to send after the stream's last sent `batch_seq` (0 when
    /// none), with that frame's sequence.
    pub(crate) fn next_frame(
        &self,
        features: &Mutex<FeatureStore>,
        settings: &DashboardSettings,
        sent: u64,
        now: Instant,
        now_ms: i64,
    ) -> Option<(u64, Arc<[u8]>)> {
        let mut state = self.0.lock().ok()?;
        let BatchState { seq, batch } = &mut *state;
        let interval = Duration::from_millis(settings.price_batch_interval_ms);
        if batch.as_ref().is_none_or(|b| now >= b.built + interval) {
            let (mut rows, session) = {
                let store = features.lock().ok()?;
                let rows = store.latest_prices(settings.price_batch_instruments);
                (rows, store.session().map(|date| date.to_string()))
            };
            rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
            let previous = batch.take().filter(|b| b.session == session);
            let changed = rows
                .iter()
                .filter(|(key, _, receipt)| {
                    previous
                        .as_ref()
                        .is_none_or(|b| b.receipts.get(key) != Some(receipt))
                })
                .collect::<Vec<_>>();
            *batch = match previous {
                Some(mut previous) if changed.is_empty() => {
                    previous.built = now;
                    Some(previous)
                }
                _ if rows.is_empty() => None,
                _ => {
                    *seq += 1;
                    let frame = |full: bool, items: &[&(String, f64, i64)]| {
                        price_batch_frame(*seq, now_ms, &session, full, items)
                    };
                    Some(PriceBatch {
                        seq: *seq,
                        built: now,
                        full: frame(true, &rows.iter().collect::<Vec<_>>()),
                        delta: frame(false, &changed),
                        receipts: rows.iter().map(|(k, _, r)| (k.clone(), *r)).collect(),
                        session,
                    })
                }
            };
        }
        let batch = batch.as_ref()?;
        match batch.seq {
            seq if seq == sent => None,
            seq if sent > 0 && seq == sent + 1 => Some((seq, Arc::clone(&batch.delta))),
            seq => Some((seq, Arc::clone(&batch.full))),
        }
    }
}
/// Price batches carry their own sequence; omitting `id:` keeps the model
/// stream cursor independent from display-only prices.
fn price_batch_frame(
    seq: u64,
    now_ms: i64,
    session: &Option<String>,
    full: bool,
    rows: &[&(String, f64, i64)],
) -> Arc<[u8]> {
    let items = rows
        .iter()
        .map(|(instrument_key, last_price, receipt_ms)| {
            serde_json::json!({
                "instrument_key": instrument_key,
                "last_price": last_price,
                "receipt_ms": receipt_ms,
            })
        })
        .collect::<Vec<_>>();
    let body = serde_json::json!({
        "v": 1,
        "batch_seq": seq,
        "as_of_ms": now_ms,
        "session": session,
        "full": full,
        "items": items,
        "price_kind": "receipt_price",
        "execution_authority": false,
        "source": "receipt_derived_feature_store",
        "note": "Display evidence only; receipt prices are not executable quotes."
    });
    format!("event: price_batch\ndata: {body}\n\n")
        .into_bytes()
        .into()
}
pub(crate) fn start_sse(
    mut stream: TcpStream,
    head: &str,
    ctx: Ctx,
) -> std::result::Result<(), (&'static str, &'static str, Option<u64>)> {
    if !local_host(
        head,
        ctx.public_host.as_ref().map(DashboardPublicHost::as_str),
    ) {
        return Err(("403 Forbidden", "local_host_required", None));
    }
    if !authorized(
        head,
        &ctx.token,
        ctx.public_host.as_ref().map(DashboardPublicHost::as_str),
    ) {
        if let Some(retry_after) = super::super::record_failed_auth(&ctx.settings) {
            return Err((
                "429 Too Many Requests",
                "failed_auth_rate_limited",
                Some(retry_after),
            ));
        }
        return Err(("401 Unauthorized", "authentication_required", None));
    }
    let Ok(Route::Events(query)) = router::parse(head.lines().next().unwrap_or_default()) else {
        return Err(("400 Bad Request", "event_query_invalid", None));
    };
    let mut after =
        event_cursor(head, &query).map_err(|reason| ("400 Bad Request", reason, None))?;
    let source = EventSource::from_ctx(&ctx);
    if source.snapshot(after)["unavailable"] == true {
        return Err(("503 Service Unavailable", "events_unavailable", None));
    }
    if ctx
        .streams
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < ctx.settings.sse_max_streams).then_some(count + 1)
        })
        .is_err()
    {
        return Err(("503 Service Unavailable", "stream_busy", None));
    }
    let permit = StreamPermit(ctx.streams.clone());
    let gzip = super::super::accepts_gzip(head);
    thread::Builder::new()
        .name("paper-dashboard-sse".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let _permit = permit;
            let _ = stream.set_write_timeout(Some(Duration::from_millis(
                ctx.settings.sse_write_timeout_ms,
            )));
            let mut headers = response("200 OK", "text/event-stream", b"");
            headers.vary = true;
            headers.encoding = gzip.then_some("gzip");
            let headers = String::from_utf8(encode(headers))
                .expect("fixed UTF-8 headers")
                .replace("Content-Length: 0\r\n", "")
                .replace("\r\n\r\n", "\r\nX-Accel-Buffering: no\r\n\r\n");
            if stream.write_all(headers.as_bytes()).is_err() {
                return;
            }
            let mut stream = FrameWriter::new(stream, gzip);
            let heartbeat = Duration::from_millis(ctx.settings.sse_heartbeat_ms);
            let batch_interval = Duration::from_millis(ctx.settings.price_batch_interval_ms.max(1));
            let mut next_heartbeat = Instant::now() + heartbeat;
            let mut next_batch = Instant::now() + batch_interval;
            let mut batch_seq = 0u64;
            loop {
                let now = Instant::now();
                if now >= next_heartbeat {
                    if stream.frame(b": heartbeat\n\n").is_err() {
                        return;
                    }
                    next_heartbeat = now + heartbeat;
                }
                if now >= next_batch {
                    if let Some(features) = ctx.features.as_ref()
                        && let Some((seq, frame)) = ctx.price_batches.next_frame(
                            features,
                            &ctx.settings,
                            batch_seq,
                            now,
                            millis(),
                        )
                    {
                        batch_seq = seq;
                        if stream.frame(&frame).is_err() {
                            return;
                        }
                    }
                    next_batch = now + batch_interval;
                }
                let data = held_events(
                    &source,
                    after,
                    next_heartbeat
                        .min(next_batch)
                        .saturating_duration_since(Instant::now()),
                );
                if data["unavailable"] == true {
                    return;
                }
                if data["gap"] == true {
                    let seq = data["events"]
                        .as_array()
                        .and_then(|v| v.first())
                        .and_then(|v| v["seq"].as_u64())
                        .map(|v| v.saturating_sub(1))
                        .or(data["next"].as_u64())
                        .unwrap_or(after);
                    let body =
                        serde_json::json!({"gap":true,"next":data["next"],"epoch":data["epoch"]});
                    if stream
                        .frame(format!("id: {seq}\nevent: run_event\ndata: {body}\n\n").as_bytes())
                        .is_err()
                    {
                        return;
                    }
                }
                for event in data["events"].as_array().into_iter().flatten() {
                    let Some(frame) = sse_frame(event, &data["epoch"]) else {
                        continue;
                    };
                    if stream.frame(&frame).is_err() {
                        return;
                    }
                }
                after = data["next"].as_u64().unwrap_or(after);
            }
        })
        .map_err(|_| ("503 Service Unavailable", "stream_unavailable", None))?;
    Ok(())
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestFeed {
    state: Mutex<(u64, Vec<serde_json::Value>)>,
    changed: std::sync::Condvar,
}
#[cfg(test)]
impl TestFeed {
    pub(crate) fn publish(&self, mut event: serde_json::Value) {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        event["seq"] = serde_json::json!(state.0);
        state.1.push(event);
        drop(state);
        self.changed.notify_all();
    }
    pub(crate) fn snapshot(&self, after: u64) -> serde_json::Value {
        let state = self.state.lock().unwrap();
        serde_json::json!({"events":state.1.iter().filter(|v|v["seq"].as_u64().unwrap()>after).collect::<Vec<_>>(),"next":state.0,"gap":false,"epoch":"fixture"})
    }
    pub(crate) fn wait(&self, after: u64, timeout: Duration) {
        let state = self.state.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.0 == after);
    }
}
