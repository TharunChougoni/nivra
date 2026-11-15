//! Explicit immutable public report inputs; no journal or credential discovery.
use super::*;
use sadr_core::{controller::DayId, execution::Portfolio};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u8,
    sessions: Vec<Source>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: DayId,
    report: PathBuf,
    report_sha256: String,
    reviews: Option<PathBuf>,
    reviews_sha256: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Review {
    id: String,
    role: String,
    model: String,
    status: String,
    validated: bool,
    findings: String,
    evidence_ids: Vec<String>,
    at: sadr_core::time::Timestamp,
    outcome: Option<String>,
    candidate_id: Option<String>,
    latency_ms: Option<u32>,
    error_category: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Job {
    id: String,
    kind: String,
    status: String,
    result_hash: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reviews {
    schema: u8,
    id: DayId,
    reviews: Vec<Review>,
    jobs: Vec<Job>,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn text(s: &str, n: usize) -> bool {
    !s.is_empty() && s.chars().count() <= n && !s.chars().any(|c| c.is_control())
}
fn read(path: &Path, name: &str, budget: &mut usize) -> Result<Vec<u8>> {
    if !path.is_absolute() || path.file_name().is_none_or(|s| s != name) {
        return Err("history_explicit_path_required".into());
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err("history_explicit_path_required".into());
    }
    for p in path.ancestors() {
        if p.file_name().is_some_and(|n| {
            [".codex", ".hermes", ".grok", ".openclaw", ".aws"]
                .iter()
                .any(|blocked| n == *blocked)
        }) {
            return Err("history_credential_namespace_refused".into());
        }
        if fs::symlink_metadata(p)?.file_type().is_symlink() {
            return Err("history_symlink".into());
        }
    }
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = f.metadata()?;
    if !before.is_file() || before.len() > 4 * 1024 * 1024 || before.len() as usize > *budget {
        return Err("history_bound".into());
    }
    let mut raw = vec![];
    f.take(4 * 1024 * 1024 + 1).read_to_end(&mut raw)?;
    let after = fs::symlink_metadata(path)?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) || raw.len() > *budget
    {
        return Err("history_changed".into());
    }
    *budget -= raw.len();
    Ok(raw)
}
#[derive(Deserialize)]
struct Summary {
    paper_only: bool,
    live_enabled: bool,
    broker: String,
    account: String,
    session: sadr_core::time::ExchangeDate,
    as_of_ms: i64,
    portfolio: Portfolio,
    #[serde(default)]
    synthetic: bool,
    #[serde(default)]
    scan_count: Option<u64>,
}
fn session(s: &Source, budget: &mut usize) -> Result<serde_json::Value> {
    if !text(&s.id.account, 64) || !text(&s.id.broker, 32) || !hash(&s.report_sha256) {
        return Err("history_identity".into());
    }
    let raw = read(&s.report, "report.json", budget)?;
    if digest(&raw) != s.report_sha256 {
        return Err("history_digest".into());
    }
    // Only typed allowlisted fields are retained; large universe/raw fields are skipped.
    let v: Summary = serde_json::from_slice(&raw)?;
    if v.account != s.id.account
        || v.broker != s.id.broker
        || v.session != s.id.date
        || !v.paper_only
        || v.live_enabled
    {
        return Err("history_identity".into());
    }
    let as_of = v.as_of_ms;
    sadr_core::time::Timestamp::from_epoch_millis(as_of)?;
    let p = v.portfolio;
    p.reconcile()?;
    let mut reviews = vec![];
    let mut jobs = vec![];
    let mut review_status = "not_configured";
    let review_result = (|| -> Result<()> {
        match (&s.reviews, &s.reviews_sha256) {
            (None, None) => {}
            (Some(path), Some(h)) if hash(h) => {
                let raw = read(path, "reviews.json", budget)?;
                if digest(&raw) != *h {
                    return Err("history_review_digest".into());
                }
                let r: Reviews = serde_json::from_value(
                    sadr_core::native::decode(&raw).map_err(|_| "history_reviews")?,
                )?;
                if r.schema != 1 || r.id != s.id || r.reviews.len() > 100 || r.jobs.len() > 100 {
                    return Err("history_reviews".into());
                }
                let mut review_ids = std::collections::BTreeSet::new();
                for review in &r.reviews {
                    if !text(&review.id, 128)
                        || !review_ids.insert(&review.id)
                        || !text(&review.model, 120)
                        || !matches!(
                            review.role.as_str(),
                            "operations" | "strategy" | "decision" | "evidence"
                        )
                        || !matches!(
                            review.status.as_str(),
                            "pending"
                                | "running"
                                | "received"
                                | "completed"
                                | "failed"
                                | "cancelled"
                                | "abstained"
                        )
                        || review.findings.chars().count() > 2000
                        || review.evidence_ids.len() > 20
                        || review.evidence_ids.iter().any(|e| !text(e, 128))
                        || review.outcome.as_ref().is_some_and(|o| {
                            !matches!(
                                o.as_str(),
                                "select"
                                    | "veto"
                                    | "abstain"
                                    | "received_invalid"
                                    | "failed"
                                    | "timeout"
                            )
                        })
                        || (review.outcome.as_deref() == Some("select")
                            && review.candidate_id.is_none())
                        || review.candidate_id.as_ref().is_some_and(|c| !text(c, 128))
                        || review.latency_ms.is_some_and(|ms| ms > 3_600_000)
                        || review.error_category.as_ref().is_some_and(|c| {
                            !matches!(
                                c.as_str(),
                                "authentication"
                                    | "quota"
                                    | "timeout"
                                    | "schema"
                                    | "transport"
                                    | "provider_failed"
                                    | "cancelled"
                                    | "unknown"
                            )
                        })
                        || (!review.findings.is_empty()
                            && (!review.validated || review.status != "completed"))
                    {
                        return Err("history_reviews".into());
                    }
                }
                let mut job_ids = std::collections::BTreeSet::new();
                for job in &r.jobs {
                    if !text(&job.id, 128)
                        || !job_ids.insert(&job.id)
                        || !text(&job.kind, 80)
                        || !matches!(
                            job.status.as_str(),
                            "pending"
                                | "running"
                                | "succeeded"
                                | "failed"
                                | "unknown"
                                | "cancelled"
                        )
                        || job.result_hash.as_ref().is_some_and(|h| !hash(h))
                    {
                        return Err("history_jobs".into());
                    }
                }
                reviews = r.reviews;
                jobs = r.jobs;
                review_status = "supplied_artifact";
            }
            _ => return Err("history_reviews".into()),
        }
        Ok(())
    })();
    if review_result.is_err() {
        reviews.clear();
        jobs.clear();
        review_status = "unavailable";
    }
    Ok(
        serde_json::json!({"id":s.id,"paper_only":v.paper_only,"live_enabled":v.live_enabled,"status":if p.stopped {"stopped"} else if p.halted {"halted"} else if p.closed {"closed"} else {"archived_snapshot"},"capital":p.allocation()?,"synthetic":v.synthetic,"scanned_count":v.scan_count,"portfolio":p,"as_of_ms":as_of,"model_reviews":reviews,"research_jobs":jobs,"review_evidence":review_status,"stale":true,"provenance":"declared_report_hash_checked","trading_controls":false}),
    )
}
pub fn project(path: &Path) -> Result<serde_json::Value> {
    let mut budget = 8 * 1024 * 1024;
    let raw = read(path, "dashboard-history.json", &mut budget)?;
    let index: Index =
        serde_json::from_value(sadr_core::native::decode(&raw).map_err(|_| "history_index")?)?;
    if index.schema != 1 || index.sessions.len() > 100 {
        return Err("history_index".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut records = vec![];
    for s in &index.sessions {
        if !seen.insert((
            s.id.broker.clone(),
            s.id.account.clone(),
            s.id.date.to_string(),
        )) {
            return Err("history_duplicate".into());
        }
        records.push(session(s,&mut budget).unwrap_or_else(|_|serde_json::json!({"id":s.id,"status":"evidence_unavailable","unavailable":true,"stale":true,"trading_controls":false})));
    }
    Ok(
        serde_json::json!({"records":records,"financial_journals_modified":false,"source":"explicit_report_index","atomic_across_sessions":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = Portfolio::new();
        let report = dir.path().join("report.json");
        let body=serde_json::json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":"fixture","session":"2026-10-16","as_of_ms":1792100000000_i64,"portfolio":p,"private_unprojected":"MUST_NOT_ESCAPE"}).to_string();
        fs::write(&report, &body).unwrap();
        let reviews = dir.path().join("reviews.json");
        let reviewbody=serde_json::json!({"schema":1,"id":{"broker":"upstox","account":"fixture","date":"2026-10-16"},"reviews":[{"id":"review-1","role":"operations","model":"fixture-model","status":"completed","validated":true,"findings":"Fixture published observation","evidence_ids":["event-1"],"at":"2026-10-16T16:00:00+05:30"}],"jobs":[]}).to_string();
        fs::write(&reviews, &reviewbody).unwrap();
        let index = dir.path().join("dashboard-history.json");
        fs::write(&index,serde_json::json!({"schema":1,"sessions":[{"id":{"broker":"upstox","account":"fixture","date":"2026-10-16"},"report":report,"report_sha256":digest(body.as_bytes()),"reviews":reviews,"reviews_sha256":digest(reviewbody.as_bytes())}]}).to_string()).unwrap();
        (dir, index)
    }
    #[test]
    fn explicit_financial_and_review_history_is_bounded_read_only_and_redacted() {
        let (_dir, path) = fixture();
        let original = fs::read(&path).unwrap();
        let p = project(&path).unwrap();
        assert_eq!(
            p["records"][0]["model_reviews"][0]["findings"],
            "Fixture published observation"
        );
        assert_eq!(p["financial_journals_modified"], false);
        assert_eq!(p["records"][0]["stale"], true);
        assert_eq!(p["records"][0]["paper_only"], true);
        assert_eq!(p["records"][0]["live_enabled"], false);
        assert!(!p.to_string().contains("MUST_NOT_ESCAPE"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn duplicate_review_or_job_ids_hide_the_artifact_without_changing_finances() {
        for (field, conflict) in [
            ("reviews", false),
            ("reviews", true),
            ("jobs", false),
            ("jobs", true),
        ] {
            let (dir, path) = fixture();
            let baseline = project(&path).unwrap();
            let review_path = dir.path().join("reviews.json");
            let mut artifact: serde_json::Value =
                serde_json::from_slice(&fs::read(&review_path).unwrap()).unwrap();
            let entry = if field == "reviews" {
                artifact["reviews"][0].clone()
            } else {
                serde_json::json!({"id":"job-1","kind":"research","status":"pending","result_hash":null})
            };
            let mut conflicting = entry.clone();
            if conflict {
                conflicting["status"] = serde_json::json!("failed");
                if field == "reviews" {
                    conflicting["findings"] = serde_json::json!("");
                }
            }
            artifact[field] = serde_json::json!([entry, conflicting]);
            let raw = serde_json::to_vec(&artifact).unwrap();
            fs::write(&review_path, &raw).unwrap();
            let mut index: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            index["sessions"][0]["reviews_sha256"] = serde_json::json!(digest(&raw));
            fs::write(&path, index.to_string()).unwrap();

            let result = project(&path).unwrap();
            let record = &result["records"][0];
            assert_eq!(record["review_evidence"], "unavailable", "{field}");
            assert_eq!(record["model_reviews"], serde_json::json!([]));
            assert_eq!(record["research_jobs"], serde_json::json!([]));
            assert_eq!(record["portfolio"], baseline["records"][0]["portfolio"]);
            assert_eq!(record["status"], baseline["records"][0]["status"]);
            assert_eq!(fs::read(&review_path).unwrap(), raw);
        }
    }
    #[test]
    fn distinct_ids_and_separate_review_job_namespaces_remain_available() {
        let (dir, path) = fixture();
        let review_path = dir.path().join("reviews.json");
        let mut artifact: serde_json::Value =
            serde_json::from_slice(&fs::read(&review_path).unwrap()).unwrap();
        let mut second = artifact["reviews"][0].clone();
        second["id"] = serde_json::json!("review-2");
        artifact["reviews"].as_array_mut().unwrap().push(second);
        artifact["jobs"] = serde_json::json!([
            {"id":"review-1","kind":"research","status":"pending","result_hash":null},
            {"id":"job-2","kind":"research","status":"failed","result_hash":null}
        ]);
        let raw = serde_json::to_vec(&artifact).unwrap();
        fs::write(&review_path, &raw).unwrap();
        let mut index: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        index["sessions"][0]["reviews_sha256"] = serde_json::json!(digest(&raw));
        fs::write(&path, index.to_string()).unwrap();
        let result = project(&path).unwrap();
        assert_eq!(result["records"][0]["review_evidence"], "supplied_artifact");
        assert_eq!(
            result["records"][0]["model_reviews"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            result["records"][0]["research_jobs"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn changed_report_symlink_identity_and_raw_review_fields_fail_without_repair() {
        let (dir, path) = fixture();
        let report = dir.path().join("report.json");
        let original = fs::read(&report).unwrap();
        fs::write(&report, b"changed").unwrap();
        assert_eq!(project(&path).unwrap()["records"][0]["unavailable"], true);
        assert_eq!(fs::read(&report).unwrap(), b"changed");
        fs::remove_file(&report).unwrap();
        std::os::unix::fs::symlink(dir.path().join("other.json"), &report).unwrap();
        assert_eq!(project(&path).unwrap()["records"][0]["unavailable"], true);
        fs::remove_file(&report).unwrap();
        fs::write(&report, &original).unwrap();
        let review = dir.path().join("reviews.json");
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&review).unwrap()).unwrap();
        v["reviews"][0]["reasoning"] = serde_json::json!("private");
        let bytes = serde_json::to_vec(&v).unwrap();
        fs::write(&review, &bytes).unwrap();
        let mut i: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        i["sessions"][0]["reviews_sha256"] = serde_json::json!(digest(&bytes));
        fs::write(&path, i.to_string()).unwrap();
        let result = project(&path).unwrap();
        assert_eq!(result["records"][0]["review_evidence"], "unavailable");
        assert!(result["records"][0]["portfolio"].is_object());
    }
}
