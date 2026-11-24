use super::*;
use serde_json::{Value, json};
fn ctx(feed: Arc<TestFeed>, streams: usize) -> Ctx {
    Ctx {
        token: "fixture".into(),
        public_host: None,
        view: Arc::new(Mutex::new(json!({"paper_only":true}))),
        history: None,
        features: None,
        equity: None,
        instrument_catalog: None,
        streams: Arc::new(AtomicUsize::new(0)),
        event_feed: Some(feed),
        settings: DashboardSettings {
            workers: 1,
            queue: 16,
            sse_max_streams: streams,
            sse_heartbeat_ms: 30,
            longpoll_hold_ms: 40,
            ..DashboardSettings::default()
        },
    }
}
fn request(extra: &str) -> String {
    format!(
        "GET /api/v1/events HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n{extra}\r\n"
    )
}
fn connect(
    listener: &TcpListener,
    sender: &mpsc::SyncSender<TcpStream>,
    request: &str,
) -> TcpStream {
    let mut stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    enqueue(sender, listener.accept().unwrap().0);
    stream.write_all(request.as_bytes()).unwrap();
    stream
}
fn until(stream: &mut TcpStream, needle: &str) -> String {
    let mut text = String::new();
    let start = Instant::now();
    while !text.contains(needle) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "missing fixture marker {needle}"
        );
        let mut bytes = [0u8; 4096];
        let count = stream.read(&mut bytes).unwrap();
        assert!(count > 0);
        text.push_str(std::str::from_utf8(&bytes[..count]).unwrap());
    }
    text
}
#[test]
fn longpoll_holds_without_events_returns_immediately_with_events_and_honours_cursors() {
    let feed = Arc::new(TestFeed::default());
    let ctx = ctx(feed.clone(), 0);
    let started = Instant::now();
    let reply = poll_events(&request(""), &ctx);
    assert!(started.elapsed() >= Duration::from_millis(40));
    let body: Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["events"], json!([]));
    feed.publish(json!({"kind":"text_delta","text":"fixture","archive_run":"harness-digest-1-fixture","execution_authority":false}));
    let started = Instant::now();
    let reply = poll_events(&request(""), &ctx);
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(
        serde_json::from_slice::<Value>(&reply.body).unwrap()["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let reply = poll_events(&request("Last-Event-ID: 1\r\n"), &ctx);
    assert_eq!(
        serde_json::from_slice::<Value>(&reply.body).unwrap()["events"],
        json!([])
    );
    for head in [
        request("Last-Event-ID: invalid\r\n"),
        request("Last-Event-ID: 1\r\nLast-Event-ID: 1\r\n"),
        request("").replace("/events ", "/events?after=0&poll=2 "),
        request("Last-Event-ID: 1\r\n").replace("/events ", "/events?after=0 "),
    ] {
        assert_eq!(poll_events(&head, &ctx).code, "400 Bad Request");
    }
}
#[test]
fn streams_are_capped_send_increasing_frames_and_heartbeats_without_blocking_status() {
    let feed = Arc::new(TestFeed::default());
    let ctx = ctx(feed.clone(), 3);
    let active = ctx.streams.clone();
    let sender = worker_pool(Arc::new(ctx)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut clients = Vec::new();
    for _ in 0..3 {
        let mut client = connect(
            &listener,
            &sender,
            &request("Accept: text/event-stream\r\n"),
        );
        let headers = until(&mut client, ": heartbeat\n\n");
        assert!(headers.contains("Content-Type: text/event-stream"));
        assert!(headers.contains("X-Accel-Buffering: no"));
        clients.push(client);
    }
    assert_eq!(active.load(Ordering::Acquire), 3);
    let mut refused = connect(
        &listener,
        &sender,
        &request("Accept: text/event-stream\r\n"),
    );
    let refused = until(&mut refused, "stream_busy");
    assert!(refused.starts_with("HTTP/1.1 503"));
    // All three SSE workers are separate from the single HTTP pool worker.
    let started = Instant::now();
    let mut status = connect(
        &listener,
        &sender,
        &request("").replace("/api/v1/events", "/api/v1/health"),
    );
    let response = until(&mut status, "Connection: close\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(started.elapsed() < Duration::from_millis(200));
    for (kind, text) in [
        ("run_started", "started"),
        ("text_delta", "prose"),
        ("tool_completed", "read_evidence completed"),
        ("run_ended", "accepted"),
    ] {
        feed.publish(json!({"kind":kind,"text":text,"archive_run":"harness-digest-1-fixture","execution_authority":false}));
    }
    let revisions = json!({"fills":"0011223344556677"});
    feed.publish(
        json!({"kind":"summary_changed","etag":"\"0123456789abcdef\"","revisions":revisions}),
    );
    for client in &mut clients {
        let terminator = format!(
            "data: {}\n\n",
            json!({"etag":"\"0123456789abcdef\"","revisions":revisions})
        );
        let text = until(client, &terminator);
        let ids: Vec<u64> = text
            .lines()
            .filter_map(|line| line.strip_prefix("id: "))
            .map(|value| value.parse().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
        let payloads: Vec<Value> = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|value| serde_json::from_str(value).unwrap())
            .collect();
        assert_eq!(payloads.len(), 5);
        assert_eq!(payloads[0]["archive_run"], "harness-digest-1-fixture");
        assert_eq!(
            payloads[4],
            json!({"etag":"\"0123456789abcdef\"","revisions":revisions})
        );
        assert!(until(client, ": heartbeat\n\n").contains(": heartbeat"));
    }
    // No source event disappeared while all three subscribers were open.
    assert_eq!(feed.snapshot(0)["events"].as_array().unwrap().len(), 5);
    drop(clients.pop());
    feed.publish(json!({"kind":"text_delta","text":"after_drop","archive_run":"harness-digest-1-fixture","execution_authority":false}));
    let deadline = Instant::now() + Duration::from_millis(500);
    while active.load(Ordering::Acquire) > 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(active.load(Ordering::Acquire), 2);
    let mut resumed = connect(
        &listener,
        &sender,
        &request("Accept: text/event-stream\r\nLast-Event-ID: 3\r\n"),
    );
    let text = until(&mut resumed, "after_drop");
    let ids: Vec<u64> = text
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .map(|value| value.parse().unwrap())
        .collect();
    assert_eq!(ids, vec![4, 5, 6]);
}
