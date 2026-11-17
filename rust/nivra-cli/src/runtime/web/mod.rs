//! Same-origin authenticated projections and stateless offline research only.
//! Backtests never open host paths, journals, credentials or trading controls.
use super::*;
mod router;
mod series;
mod v1;
use nivra_core::feature_store::FeatureStore;
pub(crate) use series::EquitySeries;
use std::{
    borrow::Cow,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Instant,
};
pub(crate) use v1::update_prepared_summary;
const MAX_BODY: usize = 12 * 1024 * 1024;
struct Asset {
    bytes: &'static [u8],
    gz: Vec<u8>,
    etag: String,
}
fn gzip(bytes: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
    encoder.write_all(bytes).expect("in-memory compression");
    encoder.finish().expect("in-memory compression")
}
fn make_asset(bytes: &'static [u8]) -> Asset {
    Asset {
        bytes,
        gz: gzip(bytes, 9),
        etag: format!("\"{:.16}\"", &digest(bytes)[..16]),
    }
}
macro_rules! v2_file {
    ($name:literal, $kind:literal) => {
        ($name, $kind, include_bytes!(concat!("v2/bundle/", $name)))
    };
}
const V2: &[(&str, &str, &[u8])] = &[
    v2_file!("index.html", "text/html; charset=utf-8"),
    v2_file!("app.js", "text/javascript; charset=utf-8"),
    v2_file!("uPlot.esm.js", "text/javascript; charset=utf-8"),
    v2_file!("app.css", "text/css; charset=utf-8"),
    v2_file!("SOURCES.md", "text/markdown; charset=utf-8"),
    v2_file!("LICENSES.txt", "text/plain; charset=utf-8"),
];
fn v2_asset(path: &str) -> Option<(&'static str, &'static Asset)> {
    static ASSETS: OnceLock<Vec<Asset>> = OnceLock::new();
    let name = match path {
        "/" => "index.html",
        "/app.js" => "app.js",
        "/app.css" => "app.css",
        "/uPlot.esm.js" => "uPlot.esm.js",
        "/SOURCES.md" => "SOURCES.md",
        "/LICENSES.txt" => "LICENSES.txt",
        _ => return None,
    };
    let index = V2.iter().position(|(file, _, _)| *file == name)?;
    let assets = ASSETS.get_or_init(|| V2.iter().map(|(_, _, bytes)| make_asset(bytes)).collect());
    Some((V2[index].1, &assets[index]))
}
fn asset_reply(
    head: &str,
    kind: &'static str,
    asset: &Asset,
    cache: Cow<'static, str>,
) -> Response {
    let matched = values(head, "if-none-match") == [asset.etag.as_str()];
    let mut reply = response(
        if matched {
            "304 Not Modified"
        } else {
            "200 OK"
        },
        kind,
        b"",
    );
    reply.cache = cache;
    reply.etag = Some(asset.etag.clone());
    reply.vary = true;
    if !matched {
        if accepts_gzip(head) {
            reply.body = asset.gz.clone();
            reply.encoding = Some("gzip");
        } else {
            reply.body = asset.bytes.to_vec();
        }
    }
    reply
}
fn accepts_gzip(head: &str) -> bool {
    values(head, "accept-encoding")
        .first()
        .is_some_and(|value| {
            value.split(',').any(|part| {
                let mut params = part.trim().split(';');
                params
                    .next()
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case("gzip"))
                    && params.all(|p| {
                        p.trim()
                            .strip_prefix("q=")
                            .is_none_or(|q| q.parse::<f32>().is_ok_and(|q| q > 0.0 && q <= 1.0))
                    })
            })
        })
}
fn values<'a>(request: &'a str, name: &str) -> Vec<&'a str> {
    request
        .split("\r\n")
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .collect()
}
fn local_host(request: &str, public_host: Option<&str>) -> bool {
    let hosts = values(request, "host");
    hosts == ["127.0.0.1:8771"] || public_host.is_some_and(|host| hosts == [host])
}
fn authorized(request: &str, token: &str, public_host: Option<&str>) -> bool {
    let auth = values(request, "authorization");
    local_host(request, public_host)
        && auth.len() == 1
        && constant_eq(auth[0].as_bytes(), format!("Bearer {token}").as_bytes())
}
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |v, (x, y)| v | (x ^ y)) == 0
}
pub(super) struct Response {
    pub(super) code: &'static str,
    pub(super) kind: &'static str,
    pub(super) body: Vec<u8>,
    pub(super) cache: Cow<'static, str>,
    pub(super) etag: Option<String>,
    pub(super) encoding: Option<&'static str>,
    pub(super) vary: bool,
    pub(super) retry_after: Option<u64>,
    pub(super) blob_size: Option<usize>,
}

struct FailedAuthBucket {
    tokens: f64,
    last_update: Instant,
}

impl FailedAuthBucket {
    fn new(per_minute: usize) -> Self {
        Self {
            tokens: per_minute as f64,
            last_update: Instant::now(),
        }
    }

    fn check_and_record(&mut self, per_minute: usize) -> Option<u64> {
        self.refill(per_minute);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let tokens_per_second = per_minute as f64 / 60.0;
            Some(((1.0 - self.tokens) / tokens_per_second).ceil().max(1.0) as u64)
        }
    }

    fn refill(&mut self, per_minute: usize) {
        let capacity = per_minute as f64;
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;
        let fill = elapsed * (capacity / 60.0);
        self.tokens = (self.tokens + fill).min(capacity);
    }
}

static FAILED_AUTH_BUCKET: Mutex<Option<FailedAuthBucket>> = Mutex::new(None);

/// Returns `Some(seconds)` when the failed-auth bucket is exhausted, otherwise `None`.
pub(super) fn record_failed_auth(settings: &DashboardSettings) -> Option<u64> {
    let mut lock = match FAILED_AUTH_BUCKET.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let bucket = lock.get_or_insert_with(|| FailedAuthBucket::new(settings.failed_auth_per_minute));
    bucket.check_and_record(settings.failed_auth_per_minute)
}

fn response(code: &'static str, kind: &'static str, body: impl AsRef<[u8]>) -> Response {
    Response {
        code,
        kind,
        body: body.as_ref().to_vec(),
        cache: Cow::Borrowed("no-store"),
        etag: None,
        encoding: None,
        vary: false,
        retry_after: None,
        blob_size: None,
    }
}
fn error(code: &'static str, reason: &str) -> Response {
    response(
        code,
        "application/json",
        serde_json::json!({"error":reason}).to_string(),
    )
}
fn content_length(head: &str) -> std::result::Result<usize, &'static str> {
    if !head.ends_with("\r\n\r\n") || !values(head, "transfer-encoding").is_empty() {
        return Err("invalid_request");
    }
    let lengths = values(head, "content-length");
    if lengths.len() > 1 {
        return Err("invalid_request");
    }
    match lengths.first() {
        None => Ok(0),
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            n.parse().map_err(|_| "invalid_request")
        }
        _ => Err("invalid_request"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktestInput {
    manifest: String,
    events: String,
}
fn research_available(v: &serde_json::Value) -> bool {
    v["mode"] == "research"
}
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn dispatch_source(
    head: &str,
    body: &[u8],
    token: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    public_host: Option<&str>,
    source: Option<&(PathBuf, String)>,
    history: Option<&PathBuf>,
    settings: &DashboardSettings,
) -> Response {
    dispatch_source_with(
        head,
        body,
        token,
        view,
        public_host,
        source,
        history,
        settings,
        None,
        None,
        None,
    )
}
#[allow(clippy::too_many_arguments)]
fn dispatch_source_with(
    head: &str,
    body: &[u8],
    token: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    public_host: Option<&str>,
    source: Option<&(PathBuf, String)>,
    history: Option<&PathBuf>,
    settings: &DashboardSettings,
    features: Option<&Arc<Mutex<FeatureStore>>>,
    equity: Option<&Arc<Mutex<EquitySeries>>>,
    instrument_catalog: Option<&Arc<Mutex<Option<serde_json::Value>>>>,
) -> Response {
    if values(head, "if-none-match").len() > 1 || values(head, "accept-encoding").len() > 1 {
        return if router::is_v1(head.lines().next().unwrap_or_default()) {
            v1::error("400 Bad Request", "duplicate_cache_header")
        } else {
            error("400 Bad Request", "duplicate_cache_header")
        };
    }
    let reply = dispatch_inner(
        head,
        body,
        token,
        view,
        public_host,
        source,
        history,
        settings,
        features,
        equity,
        instrument_catalog,
    );
    negotiate(reply, head, settings)
}
fn negotiate(mut reply: Response, head: &str, settings: &DashboardSettings) -> Response {
    if reply.kind == "application/json" {
        reply.vary = true;
        if reply.encoding.is_none()
            && reply.body.len() > settings.gzip_min_bytes
            && accepts_gzip(head)
        {
            reply.body = gzip(&reply.body, 1);
            reply.encoding = Some("gzip");
        }
    }
    reply
}
/// The dashboard's session API reads the controller's own state directory. The
/// live-stream/history file always lives at `<root>/model/...`; anything else
/// disables the session routes instead of guessing a path.
fn runtime_root(history: Option<&PathBuf>, source: Option<&(PathBuf, String)>) -> Option<PathBuf> {
    let path = history.or_else(|| source.map(|(path, _)| path))?;
    let model = path.parent()?;
    if model.file_name()? != "model" {
        return None;
    }
    model.parent().map(Path::to_path_buf)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_inner(
    head: &str,
    body: &[u8],
    token: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    public_host: Option<&str>,
    source: Option<&(PathBuf, String)>,
    history: Option<&PathBuf>,
    settings: &DashboardSettings,
    features: Option<&Arc<Mutex<FeatureStore>>>,
    equity: Option<&Arc<Mutex<EquitySeries>>>,
    instrument_catalog: Option<&Arc<Mutex<Option<serde_json::Value>>>>,
) -> Response {
    let line = head.split("\r\n").next().unwrap_or("");
    if !local_host(head, public_host) {
        return if router::is_v1(line) {
            v1::error("403 Forbidden", "local_host_required")
        } else {
            error("403 Forbidden", "local_host_required")
        };
    }
    let route = router::parse(line);
    if router::is_v1(line) {
        if !authorized(head, token, public_host) {
            if let Some(retry_after) = record_failed_auth(settings) {
                let mut reply = v1::error("429 Too Many Requests", "failed_auth_rate_limited");
                reply.retry_after = Some(retry_after);
                return reply;
            }
            return v1::error("401 Unauthorized", "authentication_required");
        }
        if let Ok(route @ (router::Route::Backtest | router::Route::BacktestExample)) = route {
            return backtest_route(route, head, body, view, public_host, settings);
        }
        let session_root = runtime_root(history, source);
        return v1::dispatch(
            route,
            head,
            body,
            view,
            settings,
            session_root.as_deref(),
            history.or(source.map(|(path, _)| path)),
            source,
            features,
            equity,
            instrument_catalog,
        );
    }
    let request_path = line
        .strip_prefix("GET ")
        .and_then(|v| v.strip_suffix(" HTTP/1.1"));
    if let Some(path) = request_path
        && let Some((kind, asset)) = v2_asset(path)
    {
        // Shell assets use stable filenames, so a long max-age would keep
        // serving a previous UI after deployment. Always revalidate with
        // the content ETag instead; a matching browser gets the cheap 304.
        return asset_reply(head, kind, asset, Cow::Borrowed("no-cache"));
    }
    error("404 Not Found", "route_not_found")
}
fn backtest_route(
    route: router::Route,
    head: &str,
    body: &[u8],
    view: &Arc<Mutex<serde_json::Value>>,
    public_host: Option<&str>,
    settings: &DashboardSettings,
) -> Response {
    if matches!(route, router::Route::BacktestExample) {
        return response(
            "200 OK",
            "application/json",
            nivra_core::replay::example().to_string(),
        );
    }
    if !view.lock().map(|v| research_available(&v)).unwrap_or(false) {
        return v1::error("409 Conflict", "backtest_paused_for_trading");
    }
    if body.len() > MAX_BODY {
        return v1::error("413 Payload Too Large", "backtest_preview_limit");
    }
    if values(head, "content-type") != ["application/json"] {
        return v1::error("415 Unsupported Media Type", "json_required");
    }
    let origin = values(head, "origin");
    if !origin.is_empty()
        && origin != ["http://127.0.0.1:8771"]
        && public_host.is_none_or(|host| origin != [format!("https://{host}")])
    {
        return v1::error("403 Forbidden", "local_research_origin_required");
    }
    if content_length(head) != Ok(body.len()) {
        return v1::error("400 Bad Request", "invalid_request_length");
    }
    let input: BacktestInput = match serde_json::from_slice(body) {
        Ok(i) => i,
        Err(_) => return v1::error("400 Bad Request", "backtest_request_invalid"),
    };
    let started = Instant::now();
    match nivra_core::replay::evaluate(input.manifest.as_bytes(), input.events.as_bytes(), || {
        started.elapsed() < Duration::from_millis(settings.backtest_timeout_ms.min(3000))
    }) {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(json) => response("200 OK", "application/json", json),
            Err(_) => v1::error("422 Unprocessable Content", "backtest_report_unavailable"),
        },
        Err(e) => v1::error("422 Unprocessable Content", &e.to_string()),
    }
}
#[cfg(test)]
fn dispatch(
    head: &str,
    body: &[u8],
    token: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    public_host: Option<&str>,
) -> Response {
    dispatch_source(
        head,
        body,
        token,
        view,
        public_host,
        None,
        None,
        &DashboardSettings::default(),
    )
}
fn encode(r: Response) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\n{}X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; font-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\nX-Frame-Options: DENY\r\nConnection: close\r\n\r\n",
        r.code,
        r.kind,
        r.body.len(),
        r.cache,
        format_args!("{}{}{}{}{}",
            r.etag.as_ref().map(|v| format!("ETag: {v}\r\n")).unwrap_or_default(),
            r.encoding.map(|v| format!("Content-Encoding: {v}\r\n")).unwrap_or_default(),
            match (r.vary,r.cache.starts_with("private")) { (true,true)=>"Vary: Accept-Encoding, Authorization\r\n",(true,false)=>"Vary: Accept-Encoding\r\n",(false,true)=>"Vary: Authorization\r\n",_=>"" },
            r.retry_after.map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default(),
            r.blob_size.map(|v|format!("X-Blob-Size: {v}\r\n")).unwrap_or_default()),
    ).into_bytes();
    bytes.extend_from_slice(&r.body);
    bytes
}

#[allow(clippy::too_many_arguments)]
pub fn start(
    token: String,
    public_host: Option<DashboardPublicHost>,
    view: Arc<Mutex<serde_json::Value>>,
    history: PathBuf,
    settings: DashboardSettings,
    features: Option<Arc<Mutex<FeatureStore>>>,
    equity: Option<Arc<Mutex<EquitySeries>>>,
    instrument_catalog: Option<Arc<Mutex<Option<serde_json::Value>>>>,
) -> Result<()> {
    serve(
        token,
        public_host,
        view,
        None,
        settings,
        Some(history),
        features,
        equity,
        instrument_catalog,
    )
}
pub fn start_with_feed(
    token: String,
    public_host: Option<DashboardPublicHost>,
    view: Arc<Mutex<serde_json::Value>>,
    source: Option<(PathBuf, String)>,
    settings: DashboardSettings,
) -> Result<()> {
    serve(
        token,
        public_host,
        view,
        source,
        settings,
        None,
        None,
        None,
        None,
    )
}
#[allow(clippy::too_many_arguments)]
fn serve(
    token: String,
    public_host: Option<DashboardPublicHost>,
    view: Arc<Mutex<serde_json::Value>>,
    source: Option<(PathBuf, String)>,
    settings: DashboardSettings,
    history: Option<PathBuf>,
    features: Option<Arc<Mutex<FeatureStore>>>,
    equity: Option<Arc<Mutex<EquitySeries>>>,
    instrument_catalog: Option<Arc<Mutex<Option<serde_json::Value>>>>,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8771")?;
    let sender = worker_pool(Arc::new(Ctx {
        token,
        public_host,
        view,
        source,
        settings,
        history,
        features,
        equity,
        instrument_catalog,
        streams: Arc::new(AtomicUsize::new(0)),
        #[cfg(test)]
        event_feed: None,
    }))?;
    thread::Builder::new()
        .name("paper-dashboard".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                enqueue(&sender, stream);
            }
        })?;
    Ok(())
}
#[derive(Clone)]
struct Ctx {
    token: String,
    public_host: Option<DashboardPublicHost>,
    view: Arc<Mutex<serde_json::Value>>,
    source: Option<(PathBuf, String)>,
    settings: DashboardSettings,
    history: Option<PathBuf>,
    features: Option<Arc<Mutex<FeatureStore>>>,
    equity: Option<Arc<Mutex<EquitySeries>>>,
    instrument_catalog: Option<Arc<Mutex<Option<serde_json::Value>>>>,
    streams: Arc<AtomicUsize>,
    #[cfg(test)]
    event_feed: Option<Arc<v1::TestFeed>>,
}
fn worker_pool(ctx: Arc<Ctx>) -> Result<mpsc::SyncSender<TcpStream>> {
    let (sender, receiver) = mpsc::sync_channel::<TcpStream>(ctx.settings.queue);
    let receiver = Arc::new(Mutex::new(receiver));
    for i in 0..ctx.settings.workers {
        let receiver = receiver.clone();
        let ctx = ctx.clone();
        thread::Builder::new()
            .name(format!("paper-dashboard-{i}"))
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                loop {
                    let stream = {
                        let Ok(receiver) = receiver.lock() else {
                            return;
                        };
                        receiver.recv()
                    };
                    let Ok(stream) = stream else { return };
                    handle(stream, &ctx);
                }
            })?;
    }
    Ok(sender)
}
fn enqueue(sender: &mpsc::SyncSender<TcpStream>, stream: TcpStream) {
    if let Err(err) = sender.try_send(stream) {
        let (mpsc::TrySendError::Full(mut stream) | mpsc::TrySendError::Disconnected(mut stream)) =
            err;
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let mut reply = error("503 Service Unavailable", "busy");
        reply.retry_after = Some(1);
        let _ = stream.write_all(&encode(reply));
    }
}
fn handle(mut stream: TcpStream, ctx: &Ctx) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let started = Instant::now();
    let mut bytes = vec![];
    let mut streaming = false;
    let reply = (|| {
        let end = loop {
            let mut buf = [0; 1024];
            let n = stream.read(&mut buf).map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            bytes.extend_from_slice(&buf[..n]);
            if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                if pos + 4 > 8192 {
                    return Ok(error("431 Request Header Fields Too Large", "header_limit"));
                }
                break pos + 4;
            }
            if bytes.len() > 8192 || started.elapsed() > Duration::from_secs(5) {
                return Ok(error("431 Request Header Fields Too Large", "header_limit"));
            }
        };
        let head = std::str::from_utf8(&bytes[..end])
            .map_err(|_| ())?
            .to_owned();
        let length = match content_length(&head) {
            Ok(n) => n,
            Err(e) => return Ok(error("400 Bad Request", e)),
        };
        if length > MAX_BODY {
            return Ok(error("413 Payload Too Large", "backtest_preview_limit"));
        }
        // Authenticate before accepting any research payload or doing compute.
        if head.starts_with("POST ")
            && !authorized(
                &head,
                &ctx.token,
                ctx.public_host.as_ref().map(DashboardPublicHost::as_str),
            )
        {
            if let Some(retry_after) = record_failed_auth(&ctx.settings) {
                let mut reply = v1::error("429 Too Many Requests", "failed_auth_rate_limited");
                reply.retry_after = Some(retry_after);
                return Ok(reply);
            }
            return Ok(error("401 Unauthorized", "authentication_required"));
        }
        while bytes.len() < end + length {
            if started.elapsed() > Duration::from_secs(5) {
                return Ok(error("408 Request Timeout", "request_deadline"));
            }
            let mut buf = [0; 8192];
            let n = stream.read(&mut buf).map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        if bytes.len() != end + length {
            return Ok(error("400 Bad Request", "invalid_request_length"));
        }
        if v1::wants_sse(&head) {
            match stream.try_clone() {
                Ok(stream) => match v1::start_sse(stream, &head, ctx.clone()) {
                    Ok(()) => {
                        streaming = true;
                        return Ok(response("200 OK", "text/event-stream", b""));
                    }
                    Err((code, reason, retry_after)) => {
                        let mut reply = v1::error(code, reason);
                        reply.retry_after = retry_after;
                        return Ok(reply);
                    }
                },
                Err(_) => return Ok(v1::error("503 Service Unavailable", "stream_unavailable")),
            }
        }
        if matches!(
            router::parse(head.lines().next().unwrap_or_default()),
            Ok(router::Route::Events(_))
        ) {
            return Ok(v1::poll_events(&head, ctx));
        }
        Ok(dispatch_source_with(
            &head,
            &bytes[end..],
            &ctx.token,
            &ctx.view,
            ctx.public_host.as_ref().map(DashboardPublicHost::as_str),
            ctx.source.as_ref(),
            ctx.history.as_ref(),
            &ctx.settings,
            ctx.features.as_ref(),
            ctx.equity.as_ref(),
            ctx.instrument_catalog.as_ref(),
        ))
    })()
    .unwrap_or_else(|(): ()| error("400 Bad Request", "request_unavailable"));
    if !streaming {
        let _ = stream.write_all(&encode(reply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_workers_isolate_slow_clients_and_refuse_a_full_queue() {
        fn pool(workers: usize) -> mpsc::SyncSender<TcpStream> {
            worker_pool(Arc::new(Ctx {
                token: "fixture".into(),
                public_host: None,
                view: view(),
                source: None,
                history: None,
                features: None,
                equity: None,
                instrument_catalog: None,
                streams: Arc::new(AtomicUsize::new(0)),
                event_feed: None,
                settings: DashboardSettings {
                    workers,
                    queue: 1,
                    ..DashboardSettings::default()
                },
            }))
            .unwrap()
        }
        fn connect(listener: &TcpListener, sender: &mpsc::SyncSender<TcpStream>) -> TcpStream {
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            enqueue(sender, listener.accept().unwrap().0);
            client
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let sender = pool(2);
        let mut slow = connect(&listener, &sender);
        slow.write_all(b"GET /api/v1/health HTTP/1.1\r\n").unwrap();
        thread::sleep(Duration::from_millis(40));
        let started = Instant::now();
        let mut fast = connect(&listener, &sender);
        fast.write_all(head("GET /api/v1/health", "").as_bytes())
            .unwrap();
        let mut reply = String::new();
        fast.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK"));
        assert!(started.elapsed() < Duration::from_millis(200));
        // The incomplete request remains open for the specified 900 ms.
        thread::sleep(Duration::from_millis(900).saturating_sub(started.elapsed()));
        drop(slow);
        drop(sender);

        let sender = pool(1);
        let mut first = connect(&listener, &sender);
        first.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        thread::sleep(Duration::from_millis(40));
        let _queued = connect(&listener, &sender);
        let started = Instant::now();
        let mut refused = connect(&listener, &sender);
        let mut reply = String::new();
        refused.read_to_string(&mut reply).unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(reply.starts_with("HTTP/1.1 503 Service Unavailable"));
        assert!(reply.contains("Retry-After: 1\r\n"));
        assert!(reply.ends_with("{\"error\":\"busy\"}"));
    }
    #[test]
    fn assets_revalidate_and_gzip_without_caching_private_json() {
        fn decoded(reply: &Response) -> Vec<u8> {
            let mut body = Vec::new();
            flate2::read::GzDecoder::new(reply.body.as_slice())
                .read_to_end(&mut body)
                .unwrap();
            body
        }
        for route in [
            "GET /legacy/",
            "GET /legacy",
            "GET /assets/dashboard.css",
            "GET /assets/dashboard.js",
            "GET /assets/theme.js",
        ] {
            assert_eq!(
                dispatch(&head(route, ""), b"", "fixture", &view(), None).code,
                "404 Not Found",
                "{route}"
            );
        }
        let v = view();
        v.lock().unwrap()["status"] = serde_json::json!("x".repeat(2048));
        let plain = dispatch(&head("GET /api/v1/health", ""), b"", "fixture", &v, None);
        let compressed = dispatch(
            &head("GET /api/v1/health", "Accept-Encoding: gzip\r\n"),
            b"",
            "fixture",
            &v,
            None,
        );
        assert_eq!(compressed.cache, "private, no-cache");
        assert!(compressed.etag.is_some());
        assert_eq!(decoded(&compressed), plain.body);
        assert!(
            String::from_utf8_lossy(&encode(compressed))
                .contains("Cache-Control: private, no-cache")
        );
        for extra in [
            "If-None-Match: a\r\nIf-None-Match: b\r\n",
            "Accept-Encoding: gzip\r\nAccept-Encoding: gzip\r\n",
        ] {
            assert_eq!(
                dispatch(&head("GET /", extra), b"", "fixture", &v, None).code,
                "400 Bad Request"
            );
        }
        let settings = DashboardSettings {
            static_max_age_s: 42,
            gzip_min_bytes: 1_048_576,
            ..DashboardSettings::default()
        };
        let asset = dispatch_source(
            &head("GET /app.js", ""),
            b"",
            "fixture",
            &v,
            None,
            None,
            None,
            &settings,
        );
        assert_eq!(asset.cache, "no-cache");
        let api = dispatch_source(
            &head("GET /api/v1/health", "Accept-Encoding: gzip\r\n"),
            b"",
            "fixture",
            &v,
            None,
            None,
            None,
            &settings,
        );
        assert!(api.encoding.is_none());
    }
    #[test]
    fn v2_shell_assets_revalidate_and_vendor_hashes_match_sources() {
        for (name, kind, bytes) in V2 {
            let route = if *name == "index.html" {
                "GET /".to_owned()
            } else {
                format!("GET /{name}")
            };
            let plain = dispatch(&head(&route, ""), b"", "fixture", &view(), None);
            assert_eq!(plain.code, "200 OK", "{name}");
            assert_eq!(plain.kind, *kind, "{name}");
            assert_eq!(plain.body, *bytes, "{name}");
            assert_eq!(plain.cache, "no-cache", "{name}");
            let etag = plain.etag.unwrap();
            let cached = dispatch(
                &head(&route, &format!("If-None-Match: {etag}\r\n")),
                b"",
                "fixture",
                &view(),
                None,
            );
            assert_eq!(cached.code, "304 Not Modified", "{name}");
            assert!(cached.body.is_empty(), "{name}");
        }
        for route in [
            "GET /v2",
            "GET /v2/",
            "GET /v2/app.js",
            "GET /v2/SOURCES.md",
        ] {
            assert_eq!(
                dispatch(&head(route, ""), b"", "fixture", &view(), None).code,
                "404 Not Found",
                "{route}"
            );
        }
        let gzip = dispatch(
            &head("GET /app.js", "Accept-Encoding: gzip\r\n"),
            b"",
            "fixture",
            &view(),
            None,
        );
        assert_eq!(gzip.encoding, Some("gzip"));
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(gzip.body.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(
            decoded,
            V2.iter().find(|(name, _, _)| *name == "app.js").unwrap().2
        );
        assert_eq!(
            dispatch(
                &head("GET /v2/missing.js", ""),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "404 Not Found"
        );

        let sources = std::str::from_utf8(
            V2.iter()
                .find(|(name, _, _)| *name == "SOURCES.md")
                .unwrap()
                .2,
        )
        .unwrap();
        for line in sources.lines() {
            let columns: Vec<_> = line.split('|').map(str::trim).collect();
            if columns.len() < 7
                || columns[1].is_empty()
                || columns[1] == "package"
                || columns[1].chars().all(|c| c == '-' || c == ':')
            {
                continue;
            }
            let file = columns[3];
            let bytes = V2
                .iter()
                .find(|(name, _, _)| *name == file)
                .unwrap_or_else(|| panic!("missing {file}"))
                .2;
            assert_eq!(digest(bytes), columns[5], "{file}");
        }
        let page = std::str::from_utf8(
            V2.iter()
                .find(|(name, _, _)| *name == "index.html")
                .unwrap()
                .2,
        )
        .unwrap();
        assert!(page.contains("type=\"module\""));
        assert!(page.contains("src=\"/app.js\""));
        assert!(!page.contains("<script>"));
        for (name, _, bytes) in V2 {
            if name.starts_with("vendor/") || !name.ends_with(".js") {
                continue;
            }
            let source = std::str::from_utf8(bytes).unwrap();
            // React owns DOM internals; application sources separately forbid raw HTML.
            assert!(!source.contains("eval("), "{name}");
        }
    }
    fn head(route: &str, extra: &str) -> String {
        format!(
            "{route} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n{extra}\r\n"
        )
    }
    fn view() -> Arc<Mutex<serde_json::Value>> {
        Arc::new(Mutex::new(
            serde_json::json!({"mode":"research","private":"fixture-account"}),
        ))
    }
    #[test]
    fn updater_accepts_real_v1_health_and_rejects_wrong_release_or_live_mode() {
        let v = view();
        // The updater requires a current completion timestamp, not request time.
        v.lock().unwrap()["as_of_ms"] = serde_json::json!(crate::runtime::millis());
        // The updater checks service health, not trading readiness: stopped/halted
        // books must remain stopped/halted and must not trigger an automatic reset.
        for status in ["scanning", "stopped", "halted"] {
            v.lock().unwrap()["status"] = serde_json::json!(status);
            let reply = dispatch(&head("GET /api/v1/health", ""), b"", "fixture", &v, None);
            assert_eq!(reply.code, "200 OK");
            let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
            assert_eq!(body["v"], 1);
            assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
            assert!(crate::update::paper_health(&reply.body, env!("CARGO_PKG_VERSION")).is_ok());
            assert!(crate::update::paper_health(&reply.body, "v0.0.0").is_err());
            // #149: the bounded updater preflight record is exposed in v1 health.
            assert!(body.as_object().unwrap().contains_key("updater"));
        }
        v.lock().unwrap()["updater"] = serde_json::json!({
            "status":"preflight_failed",
            "reason":"research_runtime_unreadable",
            "version":"v0.8.20",
            "phase":"candidate",
            "since":"2026-10-31T09:00:00+05:30",
        });
        let reply = dispatch(&head("GET /api/v1/health", ""), b"", "fixture", &v, None);
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["updater"]["reason"], "research_runtime_unreadable");
        assert_eq!(body["updater"]["version"], "v0.8.20");
        // The updater contract is served by /api/v1/health; the old /api/status
        // alias is removed and every legacy route stays 404.
        assert!(crate::update::paper_health(&reply.body, env!("CARGO_PKG_VERSION")).is_ok());
        for legacy in [
            "GET /api/status",
            "GET /api/model-history?offset=0",
            "GET /api/backtest",
            "GET /legacy/",
            "GET /assets/theme.js",
        ] {
            assert_eq!(
                dispatch(&head(legacy, ""), b"", "fixture", &v, None).code,
                "404 Not Found",
                "{legacy}"
            );
        }
        v.lock().unwrap()["live_enabled"] = serde_json::json!(true);
        let reply = dispatch(&head("GET /api/v1/health", ""), b"", "fixture", &v, None);
        assert!(crate::update::paper_health(&reply.body, env!("CARGO_PKG_VERSION")).is_err());
        let unauth = dispatch(
            &head("GET /api/v1/health", "").replace("Bearer fixture", "Bearer wrong"),
            b"",
            "fixture",
            &v,
            None,
        );
        assert!(crate::update::paper_health(&unauth.body, env!("CARGO_PKG_VERSION")).is_err());
    }
    #[test]
    fn paper_controller_dashboard_serves_session_archive_without_external_feed() {
        // VM layout (#23): paper controller starts with no feed, archive lives beside live-stream.json.
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("sessions/2026-10-23/model");
        let run = model.join("harness-digest-1792747583200-b19ef1d99cc0");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("turn-00-response.json"),
            br#"{"output":{"summary":"archived","handoff":{"decision":"wait"}}}"#,
        )
        .unwrap();
        let history = model.join("live-stream.json");
        let page = super::super::model_history::list_with_settings(
            &history,
            0,
            20,
            &DashboardSettings::default(),
            false,
        );
        assert!(page.get("unavailable").is_none(), "{page}");
        assert_eq!(page["runs"].as_array().unwrap().len(), 1);
        assert_eq!(page["runs"][0]["role"], "digest");
        assert_eq!(
            dispatch(
                &head("GET /api/model-history?offset=0", ""),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "404 Not Found"
        );
        // #222: the authenticated tools projection reads the bounded session
        // record beside live-stream.json and never calls a provider.
        fs::write(
            model.join("web-tools.ndjson"),
            concat!(
                "{\"v\":1,\"at_ms\":1792747583200,\"session\":\"2026-10-23\",\"run\":\"harness-supervisor-fixture\",",
                "\"turn\":0,\"call\":\"call_one\",\"tool\":\"web_search\",\"provider\":\"searxng\",",
                "\"query\":\"nifty bank\",\"url\":null,\"status\":\"ok\",\"reason\":null,",
                "\"latency_ms\":42,\"bytes\":512,\"results\":3,\"execution_authorized\":false}\n",
                "{\"v\":1,\"at_ms\":1792747584200,\"session\":\"2026-10-23\",\"run\":\"harness-supervisor-fixture\",",
                "\"turn\":1,\"call\":\"call_two\",\"tool\":\"web_fetch\",\"provider\":\"none\",",
                "\"query\":null,\"url\":\"https://docs.example.test/notice\",\"status\":\"failed\",",
                "\"reason\":\"web_tool_http_503\",\"latency_ms\":7,\"bytes\":0,\"results\":0,",
                "\"execution_authorized\":false}\n"
            ),
        )
        .unwrap();
        let tools = dispatch_source(
            &head("GET /api/v1/tools?date=2026-10-23&limit=10", ""),
            b"",
            "fixture",
            &view(),
            None,
            None,
            Some(&history),
            &DashboardSettings::default(),
        );
        assert_eq!(tools.code, "200 OK");
        let body: serde_json::Value = serde_json::from_slice(&tools.body).unwrap();
        assert_eq!(body["v"], 1);
        assert_eq!(body["session"], "2026-10-23");
        assert_eq!(body["total"], 2);
        assert_eq!(body["calls"][0]["tool"], "web_fetch");
        assert_eq!(body["calls"][0]["status"], "failed");
        assert_eq!(body["calls"][1]["query"], "nifty bank");
        assert!(body["calls"][1].get("api_key").is_none());
        assert_eq!(body["execution_authorized"], false);
        assert_eq!(
            dispatch_source(
                &head("GET /api/v1/tools?date=2026-10-24", ""),
                b"",
                "fixture",
                &view(),
                None,
                None,
                Some(&history),
                &DashboardSettings::default(),
            )
            .code,
            "200 OK"
        );
        assert_eq!(
            dispatch_source(
                &head("GET /api/v1/tools?limit=0", ""),
                b"",
                "fixture",
                &view(),
                None,
                None,
                Some(&history),
                &DashboardSettings::default(),
            )
            .code,
            "400 Bad Request"
        );
    }
    #[test]
    fn model_run_returns_detail_with_redacted_audit_and_validates_ids() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("sessions/2026-10-23/model");
        let id = "harness-digest-1792747583200-b19ef1d99cc0";
        let run = model.join(id);
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("turn-00-response.json"),
            br#"{"output":{"summary":"archived","handoff":{"decision":"wait"}}}"#,
        )
        .unwrap();
        fs::write(
            run.join("run-audit.json"),
            serde_json::to_vec(&serde_json::json!({
                "trigger": "digest",
                "prompt_sha256": "prompt-hash",
                "model_id": "luna",
                "started_ms": 1000,
                "ended_ms": 2000,
                "tokens_in": 10,
                "tokens_out": 5,
                "cost_estimate": 0.0,
                "tool_calls": [{
                    "name": "web_search",
                    "args": {"query": "Bearer abc123"},
                    "result_summary": {"ok": true},
                    "duration_ms": 1,
                    "status": "ok"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let without_audit = model.join("harness-digest-3000-noaudit");
        fs::create_dir_all(&without_audit).unwrap();
        fs::write(
            without_audit.join("turn-00-response.json"),
            br#"{"output":{"stance":"abstain"}}"#,
        )
        .unwrap();
        let history = model.join("live-stream.json");
        let settings = DashboardSettings::default();
        let detail =
            super::super::model_history::run_detail_with_settings(&history, id, &settings).unwrap();
        let body = detail.to_string();
        assert!(!body.contains("Bearer abc123"), "{body}");
        assert_eq!(detail["id"], id);
        assert!(detail["run"].is_object());
        assert_eq!(detail["audit"]["trigger"], "digest");
        assert_eq!(detail["audit"]["tool_calls"].as_array().unwrap().len(), 1);
        let no_audit = super::super::model_history::run_detail_with_settings(
            &history,
            "harness-digest-3000-noaudit",
            &settings,
        )
        .unwrap();
        assert_eq!(no_audit["audit"], serde_json::Value::Null);
        assert!(
            super::super::model_history::run_detail_with_settings(
                &history,
                "harness-digest-4000-unknown",
                &settings,
            )
            .is_none()
        );
        let too_long = "a".repeat(129);
        for invalid in ["..", "../etc", "a/b", "%2e%2e", "", too_long.as_str()] {
            assert!(!super::super::model_history::valid_id(invalid), "{invalid}");
        }
        assert_eq!(
            dispatch(
                &head(&format!("GET /api/model-run?id={id}"), ""),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "404 Not Found"
        );
        assert_eq!(
            dispatch(
                &head(&format!("GET /api/v1/runs/{id}"), "")
                    .replace("Bearer fixture", "Bearer wrong"),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "401 Unauthorized"
        );
    }
    #[test]
    fn history_ids_resolve_to_the_same_archived_run() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("sessions/2026-10-23/model");
        fs::create_dir_all(&model).unwrap();
        let harness = model.join("harness-digest-1792747583200-b19ef1d99cc0");
        fs::create_dir(&harness).unwrap();
        fs::write(
            harness.join("turn-00-response.json"),
            br#"{"output":{"summary":"archived","handoff":{"decision":"wait"}}}"#,
        )
        .unwrap();
        let audit = model.join("2026-10-23-claim-auditstyle");
        fs::create_dir(&audit).unwrap();
        fs::write(
            audit.join("input.json"),
            serde_json::to_vec(&serde_json::json!({
                "decide":{"model_id":"grok"},
                "input":{"trigger":{"kind":"fill"}}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            audit.join("output.json"),
            serde_json::to_vec(&serde_json::json!({
                "raw_output":"{\"actions\":[],\"stance\":\"abstain\"}"
            }))
            .unwrap(),
        )
        .unwrap();
        let history = model.join("live-stream.json");
        let settings = DashboardSettings::default();
        let page =
            super::super::model_history::list_with_settings(&history, 0, 20, &settings, false);
        let runs = page["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2, "{page}");
        let summary =
            super::super::model_history::list_with_settings(&history, 0, 20, &settings, true);
        assert_eq!(summary["next"], page["next"]);
        let compact = summary["runs"].as_array().unwrap();
        assert_eq!(compact.len(), runs.len());
        for (small, full) in compact.iter().zip(runs) {
            for field in [
                "id",
                "role",
                "started_ms",
                "duration_ms",
                "outcome",
                "tokens",
            ] {
                assert_eq!(small[field], full[field]);
            }
            assert!(small.get("turns").is_none());
            assert!(small.get("output").is_none());
            assert!(small.get("failure").is_none());
            // Summary reads do not open legacy account inputs to reconstruct
            // model/trigger metadata; detail keeps that compatibility path.
            if !small["model"].is_null() {
                assert_eq!(small["model"], full["model"]);
            }
            if !small["trigger"]["kind"].is_null() {
                assert_eq!(small["trigger"]["kind"], full["trigger"]["kind"]);
            }
        }
        assert!(
            runs.iter()
                .any(|row| row["id"] == "harness-digest-1792747583200-b19ef1d99cc0")
        );
        assert!(
            runs.iter()
                .any(|row| row["id"] == "2026-10-23-claim-auditstyle")
        );
        for row in runs {
            let id = row["id"].as_str().unwrap();
            let detail =
                super::super::model_history::run_detail_with_settings(&history, id, &settings)
                    .unwrap();
            assert_eq!(detail["id"], row["id"], "{id}");
            assert_eq!(detail["run"]["id"], row["id"], "{id}");
            assert_eq!(detail["run"]["outcome"], row["outcome"], "{id}");
            assert_eq!(
                dispatch(
                    &head(&format!("GET /api/model-run?id={id}"), ""),
                    b"",
                    "fixture",
                    &view(),
                    None
                )
                .code,
                "404 Not Found",
                "{id}"
            );
        }
    }
    #[test]
    fn configured_dashboard_public_host_is_accepted_and_other_hosts_or_origins_refuse() {
        let public = "dashboard.example.test";
        let configured = Some(public);
        let page = dispatch(
            &head("GET /api/v1/health", "").replace("127.0.0.1:8771", public),
            b"",
            "fixture",
            &view(),
            configured,
        );
        assert_eq!(page.code, "200 OK");
        for host in [
            "other.example.test",
            "dashboard.example.test:443",
            "DASHBOARD.example.test",
            "dashboard.example.test.evil",
        ] {
            assert_eq!(
                dispatch(
                    &head("GET /api/v1/health", "").replace("127.0.0.1:8771", host),
                    b"",
                    "fixture",
                    &view(),
                    configured
                )
                .code,
                "403 Forbidden"
            );
        }
        assert_eq!(
            dispatch(
                &head("GET /api/v1/health", "").replace("127.0.0.1:8771", public),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "403 Forbidden"
        );
        let example = nivra_core::replay::example();
        let input = serde_json::json!({
            "manifest":example["manifest"].to_string(),
            "events":example["events"]
        })
        .to_string();
        let request = head(
            "POST /api/v1/backtest",
            &format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                input.len()
            ),
        )
        .replace("127.0.0.1:8771", public);
        for (origin, expected) in [
            ("https://dashboard.example.test", "200 OK"),
            ("https://other.example.test", "403 Forbidden"),
            ("http://dashboard.example.test", "403 Forbidden"),
        ] {
            let request = request.replace(
                "Content-Type:",
                &format!("Origin: {origin}\r\nContent-Type:"),
            );
            assert_eq!(
                dispatch(&request, input.as_bytes(), "fixture", &view(), configured).code,
                expected,
                "{origin}"
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn model_run_does_not_serve_symlinked_run_directories() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("sessions/2026-10-23/model");
        fs::create_dir_all(&model).unwrap();
        let external = dir.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(
            external.join("turn-00-response.json"),
            br#"{"output":{"stance":"abstain"}}"#,
        )
        .unwrap();
        let id = "harness-digest-5000-link";
        std::os::unix::fs::symlink(&external, model.join(id)).unwrap();
        let history = model.join("live-stream.json");
        let response = dispatch_source(
            &head(&format!("GET /api/v1/runs/{id}"), ""),
            b"",
            "fixture",
            &view(),
            None,
            None,
            Some(&history),
            &DashboardSettings::default(),
        );
        assert_eq!(response.code, "404 Not Found");
    }
    #[test]
    fn auth_and_exact_hosts_guard_every_dashboard_resource() {
        super::super::model_history::check_archive();
        for route in [
            "GET /api/model-history?offset=0",
            "GET /api/model-history?offset=../",
            "GET /api/model-stream?after=0",
            "GET /api/model-stream?after=-1",
            "GET /legacy/",
        ] {
            assert_eq!(
                dispatch(&head(route, ""), b"", "fixture", &view(), None).code,
                "404 Not Found",
                "{route}"
            );
        }
        for route in ["GET /api/v1/health", "GET /api/v1/runs"] {
            assert_eq!(
                dispatch(&head(route, ""), b"", "fixture", &view(), None).code,
                "200 OK",
                "{route}"
            );
            assert_eq!(
                dispatch(
                    &head(route, "").replace("Bearer fixture", "Bearer wrong"),
                    b"",
                    "fixture",
                    &view(),
                    None
                )
                .code,
                "401 Unauthorized",
                "{route}"
            );
            assert_eq!(
                dispatch(
                    &head(route, "").replace("127.0.0.1:8771", "evil.invalid"),
                    b"",
                    "fixture",
                    &view(),
                    None
                )
                .code,
                "403 Forbidden",
                "{route}"
            );
        }
        let h = head("GET /api/v1/health", "");
        assert!(authorized(&h, "fixture", None));
        for raw in [
            h.replace("8771", "8772"),
            h.replace("fixture", "wrong"),
            h.replace(
                "Authorization:",
                "Authorization: Bearer fixture\r\nAuthorization:",
            ),
        ] {
            assert!(!authorized(&raw, "fixture", None));
        }
        let raw = "GET / HTTP/1.1\r\nHost: 127.0.0.1:8771\r\n\r\n";
        let page = dispatch(&head("GET /", ""), b"", "fixture", &view(), None);
        assert_eq!(page.code, "200 OK");
        assert!(!String::from_utf8_lossy(&page.body).contains("fixture-account"));
        assert_eq!(
            dispatch(
                &raw.replace("GET / ", "GET /api/v1/health "),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "401 Unauthorized"
        );
        assert_eq!(
            dispatch(
                &h.replace("127.0.0.1:8771", "evil"),
                b"",
                "fixture",
                &view(),
                None
            )
            .code,
            "403 Forbidden"
        );
        let public = "dashboard.example.test";
        let configured = Some(public);
        // Deserialize the complete deployed schema, preserving financial binding bytes.
        let c = super::super::tests::config(Path::new("fixture"));
        let binding = serde_json::to_vec(&c).unwrap();
        let mut json = serde_json::to_value(&c).unwrap();
        json["dashboard_public_host"] = serde_json::json!(public);
        let parsed: Config = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            parsed.dashboard_public_host.as_ref().unwrap().as_str(),
            public
        );
        assert_eq!(serde_json::to_vec(&parsed).unwrap(), binding);
        for invalid in [
            "",
            "EXAMPLE.test",
            "https://example.test",
            "example.test:443",
            "example.test/path",
            "*.example.test",
            "example.test.",
            "-bad.test",
            "bad-.test",
            "a..test",
            "127.0.0.1",
            "localhost",
            "a_b.test",
            "é.test",
        ] {
            json["dashboard_public_host"] = serde_json::json!(invalid);
            assert!(serde_json::from_value::<Config>(json.clone()).is_err());
        }
        for invalid in [
            format!("{}.test", "a".repeat(64)),
            format!("{}.test", "abcd.".repeat(50)),
        ] {
            assert!(DashboardPublicHost::try_from(invalid).is_err());
        }
        let public_request = h.replace("127.0.0.1:8771", public);
        assert_eq!(
            dispatch(&public_request, b"", "fixture", &view(), None).code,
            "403 Forbidden"
        );
        assert_eq!(
            dispatch(&public_request, b"", "fixture", &view(), configured).code,
            "200 OK"
        );
        assert_eq!(
            dispatch(&h, b"", "fixture", &view(), configured).code,
            "200 OK"
        );
        for host in [
            "dashboard.example.test:443",
            "DASHBOARD.example.test",
            "other.example.test",
            "dashboard.example.test.evil",
            "dashboard.example.test\r\nHost: 127.0.0.1:8771",
            "evil\r\nX-Forwarded-Host: dashboard.example.test",
        ] {
            assert_eq!(
                dispatch(
                    &h.replace("127.0.0.1:8771", host),
                    b"",
                    "fixture",
                    &view(),
                    configured
                )
                .code,
                "403 Forbidden"
            );
        }
        for host in ["127.0.0.1:8771", public] {
            for route in [
                "GET /",
                "GET /app.js",
                "GET /legacy/",
                "GET /assets/dashboard.css",
                "GET /assets/dashboard.js",
                "GET /assets/theme.js",
                "GET /api/v1/health",
                "GET /api/v1/backtest/example",
                "POST /api/v1/backtest",
            ] {
                let request = head(route, "").replace("127.0.0.1:8771", host);
                for bad in [
                    request.replace("Authorization: Bearer fixture\r\n", ""),
                    request.replace("Bearer fixture", "Bearer wrong"),
                    request.replace(
                        "Authorization:",
                        "Authorization: Bearer fixture\r\nAuthorization:",
                    ),
                ] {
                    assert_eq!(
                        dispatch(&bad, b"", "fixture", &view(), configured).code,
                        if route == "GET /" || route == "GET /app.js" {
                            "200 OK"
                        } else if route.starts_with("GET /legacy")
                            || route.starts_with("GET /assets/")
                        {
                            "404 Not Found"
                        } else {
                            "401 Unauthorized"
                        }
                    );
                }
            }
        }
        for route in [
            "GET /assets/fonts/InterVariable.woff2",
            "GET /assets/fonts/OFL.txt",
        ] {
            assert!(matches!(
                dispatch(&head(route, ""), b"", "fixture", &view(), None).code,
                "404 Not Found" | "405 Method Not Allowed"
            ));
        }
        let response = String::from_utf8(encode(page)).unwrap();
        assert!(response.contains("frame-ancestors 'none'"));
        assert!(response.contains("Cache-Control: no-cache"));
    }
    #[test]
    fn pure_backtest_route_runs_engine_and_refuses_invalid_or_cross_origin_inputs() {
        let example = nivra_core::replay::example();
        let input=serde_json::json!({"manifest":example["manifest"].to_string(),"events":example["events"]}).to_string();
        let h = head(
            "POST /api/v1/backtest",
            &format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                input.len()
            ),
        );
        let v = view();
        let before = v.lock().unwrap().clone();
        let sample = dispatch(
            &head("GET /api/v1/backtest/example", ""),
            b"",
            "fixture",
            &v,
            None,
        );
        assert_eq!(sample.code, "200 OK");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sample.body).unwrap(),
            example
        );
        for (route, expected) in [
            ("GET /api/v1/backtest", "405 Method Not Allowed"),
            ("POST /api/v1/backtest/example", "405 Method Not Allowed"),
            ("POST /api/v1/backtest?unexpected=1", "400 Bad Request"),
            ("POST /api/backtest", "404 Not Found"),
            ("GET /api/backtest/example", "404 Not Found"),
        ] {
            assert_eq!(
                dispatch(&head(route, ""), b"", "fixture", &v, None).code,
                expected,
                "{route}"
            );
        }
        assert_eq!(
            dispatch(&h, input.as_bytes(), "fixture", &v, None).code,
            "200 OK"
        );
        let public = "dashboard.example.test";
        for host in ["127.0.0.1:8771", public] {
            for (origin, expected) in [
                ("", "200 OK"),
                ("http://127.0.0.1:8771", "200 OK"),
                ("https://dashboard.example.test", "200 OK"),
                ("http://dashboard.example.test", "403 Forbidden"),
                ("https://dashboard.example.test:443", "403 Forbidden"),
                ("https://other.example.test", "403 Forbidden"),
                ("https://dashboard.example.test/", "403 Forbidden"),
                (
                    "https://dashboard.example.test\r\nOrigin: https://dashboard.example.test",
                    "403 Forbidden",
                ),
            ] {
                let mut request = h.replace("127.0.0.1:8771", host);
                if !origin.is_empty() {
                    request = request.replace(
                        "Content-Type:",
                        &format!("Origin: {origin}\r\nContent-Type:"),
                    );
                }
                assert_eq!(
                    dispatch(&request, input.as_bytes(), "fixture", &v, Some(public)).code,
                    expected
                );
            }
        }
        let public_origin = h.replace(
            "Content-Type:",
            "Origin: https://dashboard.example.test\r\nContent-Type:",
        );
        assert_eq!(
            dispatch(&public_origin, input.as_bytes(), "fixture", &v, None).code,
            "403 Forbidden"
        );
        assert_eq!(*v.lock().unwrap(), before);
        assert_eq!(
            dispatch(
                &h.replace("fixture", "wrong"),
                input.as_bytes(),
                "fixture",
                &v,
                None
            )
            .code,
            "401 Unauthorized"
        );
        assert_eq!(
            dispatch(
                &h.replace(
                    "Content-Type:",
                    "Origin: https://evil.test\r\nContent-Type:"
                ),
                input.as_bytes(),
                "fixture",
                &v,
                None
            )
            .code,
            "403 Forbidden"
        );
        assert_eq!(
            dispatch(&h, b"{}", "fixture", &v, None).code,
            "400 Bad Request"
        );
        assert_eq!(
            dispatch(&head("POST /api/stop", ""), b"", "fixture", &v, None).code,
            "404 Not Found"
        );
    }
    #[test]
    fn only_the_research_dashboard_admits_research_compute() {
        assert!(!research_available(
            &serde_json::json!({"status":"scanning"})
        ));
        assert!(research_available(
            &serde_json::json!({"mode":"research","status":"research_only"})
        ));
        let v = view();
        v.lock().unwrap()["mode"] = serde_json::json!("paper");
        assert_eq!(
            dispatch(&head("POST /api/v1/backtest", ""), b"", "fixture", &v, None).code,
            "409 Conflict"
        );
    }
    #[test]
    fn ambiguous_lengths_and_transfer_encoding_refuse() {
        assert!(
            content_length(&head(
                "POST /api/v1/backtest",
                "Content-Length: 2\r\nContent-Length: 2\r\n"
            ))
            .is_err()
        );
        assert!(
            content_length(&head(
                "POST /api/v1/backtest",
                "Transfer-Encoding: chunked\r\n"
            ))
            .is_err()
        );
        assert!(content_length(&head("POST /api/v1/backtest", "Content-Length: -1\r\n")).is_err());

        // Token bucket wait is deterministic with a local bucket; avoid mutating
        // the process-global server bucket from a parallel test.
        let mut bucket = FailedAuthBucket::new(2);
        assert_eq!(bucket.check_and_record(2), None);
        assert_eq!(bucket.check_and_record(2), None);
        let retry_after = bucket.check_and_record(2).unwrap();
        assert_eq!(retry_after, 30);
        let mut reply = error("429 Too Many Requests", "failed_auth_rate_limited");
        reply.retry_after = Some(retry_after);
        assert!(
            String::from_utf8(encode(reply))
                .unwrap()
                .contains("Retry-After: 30\r\n")
        );
    }
}
