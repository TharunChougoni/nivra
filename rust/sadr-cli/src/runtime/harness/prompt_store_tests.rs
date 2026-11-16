use super::*;
#[test]
fn prompt_files_are_private_deduplicated_hashed_and_preserve_failed_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let mut frame = Frame {
        role: "digest".into(),
        input: vec![],
        tools: vec![],
        citation_ids: vec![],
        final_answer_only: false,
        review_only: false,
        owner_tools: Default::default(),
        decision_evidence: false,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let text = frame_instructions(&frame).unwrap();
    let path = store_prompt(dir.path(), &text).unwrap();
    assert_eq!(store_prompt(dir.path(), &text).unwrap(), path);
    assert_eq!(fs::read_dir(dir.path().join("prompts")).unwrap().count(), 1);
    let bytes = fs::read(dir.path().join(&path)).unwrap();
    assert_eq!(path, format!("prompts/{}.md", digest(&bytes)));
    assert_eq!(bytes, text.as_bytes());
    assert_eq!(
        fs::metadata(dir.path().join(&path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    frame.final_answer_only = true;
    let final_text = frame_instructions(&frame).unwrap();
    let final_path = store_prompt(dir.path(), &final_text).unwrap();
    assert_ne!(path, final_path);
    assert_eq!(fs::read_dir(dir.path().join("prompts")).unwrap().count(), 2);
    fs::write(dir.path().join(&path), "corrupted fixture").unwrap();
    assert!(store_prompt(dir.path(), &text).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join(&path)).unwrap(),
        "corrupted fixture"
    );
    assert!(store_prompt(dir.path(), "Bearer secret-fixture").is_err());
}
