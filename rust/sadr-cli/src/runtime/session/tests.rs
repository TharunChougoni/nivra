use super::*;

pub(crate) fn exercise() {
    for text in [
        "api_key=private-reply-fixture",
        "X-API-Key: private-reply-fixture",
        "--api-key=private-reply-fixture",
    ] {
        assert!(!bound_reply(text).contains("private-reply-fixture"));
    }
    let redacted = bound_reply(r#"{"api_key":"private-reply-fixture","summary":"retained"}"#);
    assert_eq!(
        serde_json::from_str::<Value>(&redacted).unwrap(),
        json!({"summary":"retained"})
    );
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let session = "2026-10-31";
    let log = SessionLog::new(root, session);
    let turn = Turn {
        id: "turn-1".into(),
        session: session.into(),
        trigger: "session_open".into(),
        prompt_version: prompt_version("system", "open {{notes}}"),
        notes_version: 0,
        fills_at_start: 0,
        started_at_ms: 10,
        request: json!({
            "trigger":"session_open",
            "session_context":{"schema_version":1,"exchanges":["recursive"]},
            "recent_turns":["recursive"],
            "summary":"recursive",
            "seed":{"prior":"host envelope"},
        }),
    };
    log.append(10, Body::Turn { turn: turn.clone() }).unwrap();
    assert_eq!(log.pending().unwrap().id, "turn-1");
    assert!(log.result_for("turn-1").is_none());
    let result = TurnResult {
        turn_id: "turn-1".into(),
        finished_at_ms: 20,
        outcome: "ok".into(),
        reason: None,
        reply: "{}".into(),
        actions: Vec::new(),
    };
    log.append(
        20,
        Body::Result {
            result: result.clone(),
        },
    )
    .unwrap();
    assert!(log.pending().is_none());
    assert_eq!(log.result_for("turn-1").unwrap(), result);
    assert_eq!(log.turns().len(), 1);
    let conversation = log.conversation();
    assert_eq!(conversation["schema_version"], 1);
    assert_eq!(conversation["exchanges"].as_array().unwrap().len(), 1);
    assert_eq!(
        conversation["exchanges"][0]["turn"]["request"]["trigger"],
        "session_open"
    );
    for key in ["session_context", "recent_turns", "summary", "seed"] {
        assert!(
            conversation["exchanges"][0]["turn"]["request"]
                .get(key)
                .is_none()
        );
    }
    assert_eq!(conversation["exchanges"][0]["result"]["reply"], "{}");
    assert_eq!(conversation["exchanges"][0]["result"]["actions"], json!([]));
    assert_eq!(
        log.context_bytes(),
        serde_json::to_vec(&conversation).unwrap().len() as u64
    );
    // The framed reader never errors and never returns a partial last record,
    // including a cut at every byte offset.
    let path = root.join(ROOT).join(session).join("log.bin");
    let raw = fs::read(&path).unwrap();
    for cut in 0..raw.len() {
        let partial = root.join(format!("partial-{cut}.bin"));
        fs::write(&partial, &raw[..cut]).unwrap();
        let records = read_frames(&partial);
        assert!(
            records.len() <= 2,
            "cut={cut} yielded {} records",
            records.len()
        );
        assert!(records.iter().all(|record| record.seq > 0));
        fs::remove_file(partial).unwrap();
    }
    // Replay is bounded: a large committed request keeps continuity fields only,
    // the digest keeps only its brief, and the original log keeps every turn.
    let mut big = turn.clone();
    big.id = "turn-big".into();
    big.request = json!({
        "digest":{"brief":"focus","rows":vec!["x".repeat(64);200]},
        "evidence":{"rows":vec!["y".repeat(64);200]},
    });
    log.append(30, Body::Turn { turn: big }).unwrap();
    log.append(
        31,
        Body::Result {
            result: TurnResult {
                turn_id: "turn-big".into(),
                finished_at_ms: 31,
                outcome: "failed".into(),
                reason: None,
                reply: "{}".into(),
                actions: Vec::new(),
            },
        },
    )
    .unwrap();
    let bounded = log.conversation();
    let last = bounded["exchanges"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["turn"]["request"]["replay_bounded"], json!(true));
    assert!(last["turn"]["request"].get("evidence").is_none());
    assert_eq!(last["turn"]["request"]["digest"]["brief"], json!("focus"));
    assert!(last["turn"]["request"]["digest"].get("rows").is_none());
    assert_eq!(log.turns().len(), 2, "the original log keeps every turn");

    let first_end = frame_header() + u32::from_be_bytes(raw[..4].try_into().unwrap()) as usize;
    for offset in [0, first_end] {
        for damage in [
            "checksum",
            "json",
            "zero_length",
            "long_length",
            "increased_length",
        ] {
            let mut corrupt = raw.clone();
            match damage {
                "checksum" => corrupt[offset + 4] ^= 1,
                "json" => {
                    let start = offset + frame_header();
                    let len =
                        u32::from_be_bytes(raw[offset..offset + 4].try_into().unwrap()) as usize;
                    corrupt[start] = b'!';
                    let digest = sha256(&corrupt[start..start + len]);
                    corrupt[offset + 4..start].copy_from_slice(&digest);
                }
                "zero_length" => corrupt[offset..offset + 4].copy_from_slice(&0u32.to_be_bytes()),
                "long_length" => {
                    corrupt[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
                }
                "increased_length" => {
                    let len = u32::from_be_bytes(raw[offset..offset + 4].try_into().unwrap());
                    corrupt[offset..offset + 4].copy_from_slice(&(len + 1).to_be_bytes());
                }
                _ => unreachable!(),
            }
            fs::write(&path, &corrupt).unwrap();
            assert_eq!(log.records().len(), usize::from(offset != 0));
            assert_eq!(fs::read(&path).unwrap(), corrupt);
            let appended = log.append(30, Body::Turn { turn: turn.clone() });
            assert_eq!(
                fs::read(&path).unwrap(),
                corrupt,
                "damage={damage} offset={offset}"
            );
            assert!(appended.is_err(), "damage={damage} offset={offset}");
        }
    }
    for sequences in [[1, 1], [2, 1], [1, u64::MAX]] {
        fs::write(&path, &raw).unwrap();
        let mut records = read_frames(&path);
        let mut bytes = Vec::new();
        for (record, seq) in records.iter_mut().zip(sequences) {
            record.seq = seq;
            bytes.extend_from_slice(&encode_frame(record).unwrap());
        }
        bytes.extend_from_slice(&[0, 0]);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(log.records().len(), 2);
        let appended = log.append(30, Body::Turn { turn: turn.clone() });
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(appended.is_err());
    }
    for cut in [first_end + 2, first_end + frame_header() - 1, raw.len() - 3] {
        fs::write(&path, &raw[..cut]).unwrap();
        let appended = log.append(30, Body::Turn { turn: turn.clone() }).unwrap();
        assert_eq!(appended.seq, 2);
        let repaired = fs::read(&path).unwrap();
        assert_eq!(&repaired[..first_end], &raw[..first_end]);
        assert_eq!(log.records().len(), 2);
    }
    let linked_log = SessionLog::new(root, "linked-log");
    fs::create_dir_all(&linked_log.directory).unwrap();
    let target = root.join("link-target.bin");
    fs::write(&target, &raw[..raw.len() - 3]).unwrap();
    std::os::unix::fs::symlink(&target, linked_log.path()).unwrap();
    assert!(
        linked_log
            .append(30, Body::Turn { turn: turn.clone() })
            .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), &raw[..raw.len() - 3]);
    assert!(
        fs::symlink_metadata(linked_log.path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let blocked_log = SessionLog::new(root, "blocked-log");
    fs::create_dir_all(blocked_log.path()).unwrap();
    assert!(
        blocked_log
            .append(30, Body::Turn { turn: turn.clone() })
            .is_err()
    );
    assert!(blocked_log.path().is_dir());
    let oversized_log = SessionLog::new(root, "oversized-log");
    fs::create_dir_all(&oversized_log.directory).unwrap();
    let oversized = fs::File::create(oversized_log.path()).unwrap();
    oversized.set_len(MAX_LOG_BYTES as u64 + 1).unwrap();
    assert!(
        oversized_log
            .append(30, Body::Turn { turn: turn.clone() })
            .is_err()
    );
    assert_eq!(
        fs::metadata(oversized_log.path()).unwrap().len(),
        MAX_LOG_BYTES as u64 + 1
    );
    // A partial tail is repaired on the next append instead of poisoning it.
    fs::write(&path, &raw[..raw.len() - 3]).unwrap();
    log.append(
        30,
        Body::Summary {
            summary: Summary {
                up_to_seq: 1,
                text: "older turns".into(),
                created_at_ms: 30,
            },
        },
    )
    .unwrap();
    // The interrupted result frame was never durable: it is dropped, the turn
    // becomes pending again, and the summary keeps its sequence.
    assert_eq!(log.records().len(), 2);
    assert_eq!(log.pending().unwrap().id, "turn-1");
    assert_eq!(log.summary().unwrap().up_to_seq, 1);
    assert!(log.recent_turns(8).is_empty());

    // Conversation reconstruction reopens the append-only log, excludes the
    // summary-covered prefix, includes complete failed turns and action
    // receipt references, and leaves a pending turn out until its result lands.
    let log = SessionLog::new(root, session);
    let first_failure_turn = Turn {
        id: "turn-2".into(),
        session: session.into(),
        trigger: "scanner_update".into(),
        prompt_version: "prompt-v2".into(),
        notes_version: 3,
        fills_at_start: 0,
        started_at_ms: 50,
        request: json!({
            "evidence":{"source":"scanner"},
            "api_key":"synthetic-test-secret",
            "session_context":{"should":"be stripped"},
            "recent_turns":["should be stripped"],
            "summary":"should be stripped",
            "seed":{"should":"be stripped"},
        }),
    };
    log.append(
        50,
        Body::Turn {
            turn: first_failure_turn,
        },
    )
    .unwrap();
    let action_ref = action("directive", "dir-2", &json!({"text":"watch"}));
    let failure = TurnResult {
        turn_id: "turn-2".into(),
        finished_at_ms: 60,
        outcome: "failed".into(),
        reason: Some("supervisor_call_timeout".into()),
        reply: "provider timeout".into(),
        actions: vec![action_ref.clone()],
    };
    log.append(
        60,
        Body::Result {
            result: failure.clone(),
        },
    )
    .unwrap();
    let second_turn = Turn {
        id: "turn-3".into(),
        session: session.into(),
        trigger: "portfolio_change".into(),
        prompt_version: "prompt-v2".into(),
        notes_version: 3,
        fills_at_start: 0,
        started_at_ms: 70,
        request: json!({"evidence":{"source":"portfolio"}}),
    };
    log.append(70, Body::Turn { turn: second_turn }).unwrap();
    log.append(
        80,
        Body::Result {
            result: TurnResult {
                turn_id: "turn-3".into(),
                finished_at_ms: 80,
                outcome: "ok".into(),
                reason: None,
                reply: "abstain".into(),
                actions: Vec::new(),
            },
        },
    )
    .unwrap();
    let pending_turn = Turn {
        id: "turn-4".into(),
        session: session.into(),
        trigger: "idle".into(),
        prompt_version: "prompt-v2".into(),
        notes_version: 3,
        fills_at_start: 0,
        started_at_ms: 90,
        request: json!({"evidence":{"source":"idle"}}),
    };
    log.append(90, Body::Turn { turn: pending_turn }).unwrap();
    assert_eq!(log.pending().unwrap().id, "turn-4");
    let reopened = SessionLog::new(root, session);
    let conversation = reopened.conversation();
    assert_eq!(conversation["summary"]["text"], "older turns");
    let exchanges = conversation["exchanges"].as_array().unwrap();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0]["turn"]["id"], "turn-2");
    assert_eq!(exchanges[0]["turn"]["prompt_version"], "prompt-v2");
    assert_eq!(
        exchanges[0]["turn"]["request"]["evidence"]["source"],
        "scanner"
    );
    assert!(exchanges[0]["turn"]["request"].get("api_key").is_none());
    assert_eq!(
        reopened
            .turns()
            .iter()
            .find(|turn| turn.id == "turn-2")
            .unwrap()
            .request["api_key"],
        "synthetic-test-secret"
    );
    for key in ["session_context", "recent_turns", "summary", "seed"] {
        assert!(exchanges[0]["turn"]["request"].get(key).is_none());
    }
    assert_eq!(exchanges[0]["result"]["outcome"], "failed");
    assert_eq!(exchanges[0]["result"]["reason"], "supervisor_call_timeout");
    assert_eq!(exchanges[0]["result"]["reply"], failure.reply);
    assert_eq!(
        exchanges[0]["result"],
        serde_json::to_value(&failure).unwrap()
    );
    assert_eq!(exchanges[0]["result"]["actions"][0]["id"], action_ref.id);
    assert_eq!(
        exchanges[0]["result"]["actions"][0]["digest"],
        action_ref.digest
    );
    assert_eq!(exchanges[1]["turn"]["id"], "turn-3");
    assert_eq!(
        exchanges[1]["turn"]["request"]["evidence"]["source"],
        "portfolio"
    );
    assert_eq!(exchanges[1]["result"]["outcome"], "ok");
    assert_eq!(
        reopened.context_bytes(),
        serde_json::to_vec(&conversation).unwrap().len() as u64
    );
    let without_reply_bytes =
        serde_json::to_vec(&conversation).unwrap().len() as u64 - failure.reply.len() as u64;
    assert!(reopened.context_bytes() > without_reply_bytes);

    // Notes: add, pin, mid-turn version change, revision-guarded mark, keep and
    // close expiry. Keep notes survive for tomorrow even with no handoff.
    let notes = Notes::new(root, session);
    let note = notes.add("no new pharma", true, 40).unwrap();
    assert_eq!(notes.version(), 1);
    assert_eq!(notes.pin_waiting("turn-1", 41).unwrap(), 2);
    assert_eq!(notes.snapshot().notes[0].state, NoteState::Pinned);
    assert_eq!(notes.snapshot().notes[0].updated_by, "host");
    let built_on = notes.version();
    let avoid = notes.add("avoid that symbol", false, 42).unwrap();
    assert!(check_notes_version(root, session, built_on).is_err());
    assert!(check_notes_version(root, session, notes.version()).is_ok());
    assert_eq!(
        notes
            .mark(&note.id, 0, NoteState::Done, 43)
            .unwrap_err()
            .to_string(),
        "session_note_revision_changed"
    );
    notes.mark(&note.id, 1, NoteState::Done, 44).unwrap();
    let second = notes.add("keep this", true, 45).unwrap();
    let third = notes.add("temporary", false, 46).unwrap();
    assert_eq!(
        notes.expire_at_close(47).unwrap(),
        vec![avoid.id.clone(), third.id.clone()]
    );
    let keep = notes.keep_for_seed();
    assert_eq!(keep.len(), 2);
    assert!(keep.iter().any(|n| n.id == note.id));
    assert!(keep.iter().any(|n| n.id == second.id));
    assert_eq!(notes.snapshot().version, 7);

    // One action per turn id: an identical replayed action is refused, while a
    // distinct action in the same turn is allowed.
    let first_action = action("directive", "dir-1", &json!({"text":"watch"}));
    commit_action(root, session, "turn-1", &first_action).unwrap();
    assert!(commit_action(root, session, "turn-1", &first_action).is_err());
    let other = action("directive", "dir-2", &json!({"text":"wait"}));
    commit_action(root, session, "turn-1", &other).unwrap();
    assert_eq!(actions_for(root, session, "turn-1").len(), 2);

    // The host-built seed carries keep notes without any model reply.
    let target = "2026-11-01";
    let built = build_seed(
        root,
        session,
        target,
        &SeedContext {
            source_turn_id: "turn-handoff".into(),
            standing_instructions: "paper-only standing instructions".into(),
            positions: json!([{"instrument":{"key":"NSE_EQ|A","symbol":"A","series":"EQ","tick":"0.05"},"units":1,"entry":"100","mark":"101","marked_at":56_000}]),
            directives: json!([]),
            alerts: json!([]),
            research_state: json!({"available":false,"reason":"no_durable_research_state_source"}),
            unresolved_work_refs: json!({"available":false,"reason":"request_store_unavailable","refs":[]}),
            review_state: Value::Null,
            workspace_state: Value::Null,
        },
    );
    write_seed(root, target, &built).unwrap();
    assert_eq!(seed_for_request(root, target).unwrap(), built);
    // Owner-review refs are optional, bounded and path-free. Seeds without the
    // field stay valid for older binaries and existing installations.
    assert!(built.get("review_state").is_none());
    let mut reviewed = built.clone();
    reviewed["review_state"] = json!({"available":true,"refs":[{
        "review_id":"a".repeat(64),"sealed_sha256":"b".repeat(64),
        "artifact_sha256":"c".repeat(64),"status":"completed","completed_ms":56_000
    }]});
    validate_seed(&reviewed, target).unwrap();
    let mut unavailable = built.clone();
    unavailable["review_state"] = json!({"available":false,"reason":"owner_review_not_attempted"});
    validate_seed(&unavailable, target).unwrap();
    for mutate in [
        json!({"available":true,"refs":[]}),
        json!({"available":true,"refs":[{"review_id":"a".repeat(64),"sealed_sha256":"b".repeat(64),"artifact_sha256":"c".repeat(64),"status":"completed","completed_ms":0}]}),
        json!({"available":true,"refs":[{"review_id":"A".repeat(64),"sealed_sha256":"b".repeat(64),"artifact_sha256":"c".repeat(64),"status":"completed","completed_ms":1}]}),
        json!({"available":true,"refs":[{"review_id":"a".repeat(64),"sealed_sha256":"b".repeat(64),"artifact_sha256":"c".repeat(64),"status":"unknown","completed_ms":1}]}),
        json!({"available":true,"refs":[{"review_id":"a".repeat(64),"sealed_sha256":"b".repeat(64),"artifact_sha256":"c".repeat(64),"status":"completed","completed_ms":1,"path":"/tmp/review.json"}]}),
        json!({"available":false,"reason":"x","refs":[]}),
        json!({"available":false}),
        json!({"available":true,"refs":[{"review_id":"a".repeat(64),"sealed_sha256":"b".repeat(64),"artifact_sha256":"c".repeat(64),"status":"completed","completed_ms":1}],"extra":true}),
        json!({"available":true,"refs":(0..9).map(|_| json!({"review_id":"a".repeat(64),"sealed_sha256":"b".repeat(64),"artifact_sha256":"c".repeat(64),"status":"failed","completed_ms":1})).collect::<Vec<_>>()}),
    ] {
        let mut invalid = built.clone();
        invalid["review_state"] = mutate;
        assert_eq!(validate_seed(&invalid, target), Err("session_seed_invalid"));
    }
    let mut unknown = built.clone();
    unknown["schema_version"] = json!(3);
    assert_eq!(validate_seed(&unknown, target), Err("session_seed_invalid"));
    let mut executable = built.clone();
    executable["positions"] = json!([{"instrument":{"key":"NSE_EQ|A","symbol":"A","series":"EQ","tick":"0.05"},"units":1,"entry":"100","mark":"101","marked_at":1,"exit_intent":{"stop":"90"}}]);
    assert_eq!(
        validate_seed(&executable, target),
        Err("session_seed_invalid")
    );
    assert_eq!(seed_for_request(root, "2026-11-02").unwrap(), Value::Null);
    write_seed(root, target, &built).unwrap();
    let seed_path = root.join(ROOT).join(target).join("seed.json");
    let original_seed_bytes = fs::read(&seed_path).unwrap();
    let changed_seed = json!({"source_session":session,"target_session":target,"changed":true});
    assert_eq!(
        write_seed(root, target, &changed_seed)
            .unwrap_err()
            .to_string(),
        "session_seed_conflict"
    );
    assert_eq!(fs::read(&seed_path).unwrap(), original_seed_bytes);
    assert_eq!(built["keep_notes"][0]["id"], note.id);
    assert_eq!(built["positions"][0]["instrument"]["key"], "NSE_EQ|A");

    // Existing malformed and symlink seeds are explicit invalid artifacts and
    // are never replaced by a later handoff attempt.
    let invalid_session = "2026-11-02";
    let invalid_path = root.join(ROOT).join(invalid_session).join("seed.json");
    fs::create_dir_all(invalid_path.parent().unwrap()).unwrap();
    fs::write(&invalid_path, b"partial seed").unwrap();
    assert_eq!(
        write_seed(root, invalid_session, &built)
            .unwrap_err()
            .to_string(),
        "session_seed_invalid"
    );
    assert_eq!(fs::read(&invalid_path).unwrap(), b"partial seed");

    let symlink_session = "2026-11-03";
    let symlink_path = root.join(ROOT).join(symlink_session).join("seed.json");
    fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&seed_path, &symlink_path).unwrap();
    assert_eq!(
        write_seed(root, symlink_session, &built)
            .unwrap_err()
            .to_string(),
        "session_seed_invalid"
    );
    assert!(
        fs::symlink_metadata(&symlink_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // A chat/aside instance never carries a write tool; the host filters before
    // the provider request, not by prompt text (#145).
    let tools = vec![
        "get_portfolio".to_owned(),
        "sandbox_write".to_owned(),
        "replay".to_owned(),
        "run_registered_analysis".to_owned(),
        "web_search".to_owned(),
    ];
    assert_eq!(
        read_only_tools(&tools),
        vec!["get_portfolio".to_owned(), "web_search".to_owned()]
    );
    assert!(write_tool("sandbox_write") && write_tool("replay"));
    assert!(
        write_tool("workspace_write")
            && write_tool("workspace_run")
            && write_tool("workspace_install")
    );
    assert!(!write_tool("get_portfolio") && !write_tool("web_search"));
    assert!(!write_tool("workspace_list") && !write_tool("workspace_read"));
    assert_eq!(bound_reply("short"), "short");
    assert!(bound_reply(&"x".repeat(MAX_REPLY_BYTES + 10)).ends_with('…'));

    // Checked pending projection: a missing log, a complete log with no
    // pending turn, a real pending turn and every unavailable source are
    // distinct. The strict reader never repairs, truncates or follows a link.
    let checked_root = tempfile::tempdir().unwrap();
    let checked_session = "2026-11-04";
    let checked_log = SessionLog::new(checked_root.path(), checked_session);
    let checked_path = checked_root
        .path()
        .join(ROOT)
        .join(checked_session)
        .join("log.bin");
    assert_eq!(checked_log.pending_checked().source, PendingSource::Missing);
    assert!(checked_log.pending_checked().turn.is_none());
    checked_log
        .append(1, Body::Turn { turn: turn.clone() })
        .unwrap();
    let checked_pending = checked_log.pending_checked();
    assert_eq!(checked_pending.source, PendingSource::Pending);
    assert_eq!(checked_pending.turn.unwrap().id, "turn-1");
    checked_log
        .append(
            2,
            Body::Result {
                result: result.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        checked_log.pending_checked().source,
        PendingSource::NoPending
    );
    assert!(checked_log.pending_checked().turn.is_none());
    // A corrupt frame after a durable pending turn keeps the checked evidence
    // visible but marks the source unavailable.
    let mut second_turn = turn.clone();
    second_turn.id = "turn-2".into();
    checked_log
        .append(3, Body::Turn { turn: second_turn })
        .unwrap();
    let mut third_turn = turn.clone();
    third_turn.id = "turn-3".into();
    checked_log
        .append(4, Body::Turn { turn: third_turn })
        .unwrap();
    let mut checked_raw = fs::read(&checked_path).unwrap();
    let last = checked_raw.len() - 1;
    checked_raw[last] ^= 1;
    fs::write(&checked_path, &checked_raw).unwrap();
    let checked_corrupt = checked_log.pending_checked();
    assert_eq!(checked_corrupt.source, PendingSource::Corrupt);
    assert_eq!(
        checked_corrupt.turn.as_ref().map(|turn| turn.id.as_str()),
        Some("turn-2")
    );
    // An incomplete tail after a terminal result cannot prove that no future
    // pending turn exists, so it is unavailable rather than "no pending".
    let incomplete_root = tempfile::tempdir().unwrap();
    let incomplete_session = "2026-11-06";
    let incomplete_log = SessionLog::new(incomplete_root.path(), incomplete_session);
    incomplete_log
        .append(1, Body::Turn { turn: turn.clone() })
        .unwrap();
    incomplete_log
        .append(
            2,
            Body::Result {
                result: result.clone(),
            },
        )
        .unwrap();
    let incomplete_path = incomplete_root
        .path()
        .join(ROOT)
        .join(incomplete_session)
        .join("log.bin");
    let mut incomplete_raw = fs::read(&incomplete_path).unwrap();
    incomplete_raw.extend_from_slice(&[0, 0]);
    fs::write(&incomplete_path, &incomplete_raw).unwrap();
    let checked_incomplete = incomplete_log.pending_checked();
    assert_eq!(checked_incomplete.source, PendingSource::Incomplete);
    assert!(checked_incomplete.turn.is_none());
    // A symlink is not followed by the checked reader, even to a valid log.
    let checked_target = checked_root.path().join("checked-target.bin");
    fs::write(&checked_target, &checked_raw).unwrap();
    fs::remove_file(&checked_path).unwrap();
    std::os::unix::fs::symlink(&checked_target, &checked_path).unwrap();
    assert_eq!(checked_log.pending_checked().source, PendingSource::Symlink);
    // A directory at the log path is non-regular.
    fs::remove_file(&checked_path).unwrap();
    fs::create_dir_all(&checked_path).unwrap();
    assert_eq!(
        checked_log.pending_checked().source,
        PendingSource::NonRegular
    );
    // A file where the session directory should be is unreadable.
    let unreadable_root = tempfile::tempdir().unwrap();
    let unreadable_session = "2026-11-05";
    let unreadable_parent = unreadable_root.path().join(ROOT).join(unreadable_session);
    fs::create_dir_all(unreadable_parent.parent().unwrap()).unwrap();
    fs::write(&unreadable_parent, b"not-a-directory").unwrap();
    assert_eq!(
        SessionLog::new(unreadable_root.path(), unreadable_session)
            .pending_checked()
            .source,
        PendingSource::Unreadable
    );
    // An oversized sparse log is refused from metadata without a full read.
    let checked_oversized = checked_root.path().join("oversized-checked.bin");
    let sparse = fs::File::create(&checked_oversized).unwrap();
    sparse.set_len(MAX_LOG_BYTES as u64 + 1).unwrap();
    drop(sparse);
    assert_eq!(
        read_log_bounded(&checked_oversized, 1024, false),
        Err(LogReadError::Oversize)
    );
    assert!(read_frames(&checked_oversized).is_empty());
}
