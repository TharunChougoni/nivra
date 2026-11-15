//! Standalone, read-only laboratory report. It contains no script or network assets.
use sadr_store::strategy_results::ResultsDb;
use serde_json::Value;
use std::{error::Error, fs::OpenOptions, io::Write, path::Path};

fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn cell(value: &Value) -> String {
    if value.is_null() {
        return "unknown".into();
    }
    escaped(&match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    })
}
fn render(summary: &Value) -> String {
    let mut html = String::from(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><title>Strategy laboratory report</title><style>body{font:16px system-ui;max-width:1400px;margin:2rem auto;padding:1rem;background:#f7f8fa;color:#182334}table{border-collapse:collapse;width:100%;background:white}th,td{padding:.6rem;text-align:left;border:1px solid #d8dfe7;overflow-wrap:anywhere}th{background:#e8edf4}code{font-size:12px}h1,h2{line-height:1.2}.scroll{overflow:auto}</style><h1>Strategy laboratory report</h1><p>Paper research evidence only. Approval history records owner decisions; it does not establish an active champion or activate a session. Rows are ordered by exact spec hash, not by performance.</p><p>Gross and net values are descriptive forward marks in basis points, not executable trading returns. Net estimates use each replay's configured hypothetical round-trip notional, spread and fee assumptions. Missing costs or evidence are shown as unknown. No portfolio sizing, liquidity or profitability claim is implied.</p><h2>Spec scorecards and latest scans</h2><div class=\"scroll\"><table><thead><tr><th>Spec / version / history</th><th>Exact hash</th><th>Samples</th><th>Gross bps</th><th>Net bps</th><th>Horizon and cost assumptions</th><th>Missing / failed</th><th>Missing-mark reasons</th><th>Selected / universe</th><th>Scan time ms</th></tr></thead><tbody>",
    );
    for spec in summary["specs"].as_array().into_iter().flatten() {
        let card = &spec["scorecard"];
        let scan = &spec["scan"];
        html.push_str(&format!("<tr><td>{} v{} / {} samples</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{} / {}</td><td>{}</td><td>{} / {}</td><td>{}</td></tr>",cell(&spec["id"]),cell(&spec["version"]),cell(&spec["lookback_samples"]),cell(&spec["hash"]),cell(&card["samples"]),cell(&card["gross_bps"]),cell(&card["net_bps"]),cell(&card["replay_config"]),cell(&card["missing_marks"]),cell(&card["failed_ticks"]),cell(&card["missing_mark_reasons"]),cell(&scan["selected"]),cell(&scan["universe"]),cell(&scan["at_ms"])));
    }
    html.push_str("</tbody></table></div><h2>Owner approval history</h2><p>Newest recorded approvals first. This is approval history, not a reconstructed deployment or champion activation history.</p><div class=\"scroll\"><table><thead><tr><th>Proposal</th><th>Exact hash</th><th>Approver</th><th>Evidence reference</th><th>Between sessions</th></tr></thead><tbody>");
    for approval in summary["approvals"].as_array().into_iter().flatten() {
        html.push_str(&format!(
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            cell(&approval["proposal"]),
            cell(&approval["hash"]),
            cell(&approval["approver"]),
            cell(&approval["reference"]),
            cell(&approval["between_sessions"])
        ));
    }
    html.push_str("</tbody></table></div>");
    if summary["truncated"].as_bool() == Some(true) {
        html.push_str("<p>Report truncated: at most 64 specs and 100 approvals. Use paginated laboratory queries for further evidence.</p>");
    }
    html.push_str("</html>");
    html
}
pub fn write(database: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let db = ResultsDb::open_read_only(database)?;
    let html = render(&db.report_summary()?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|_| "strategy_report_output_rejected")?;
    file.write_all(html.as_bytes())?;
    file.sync_all()?;
    println!("strategy_report_created_read_only_evidence");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_escapes_untrusted_text_and_preserves_database() {
        let html = render(
            &serde_json::json!({"specs":[{"id":"<script>alert('x')</script>"}],"approvals":[{"reference":"\"<img src=x onerror=alert(1)>"}]}),
        );
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;script&gt;"));
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("results.sqlite");
        drop(ResultsDb::open(&source).unwrap());
        let before = std::fs::read(&source).unwrap();
        let output = dir.path().join("report.html");
        write(&source, &output).unwrap();
        assert_eq!(before, std::fs::read(&source).unwrap());
        assert!(write(&source, &output).is_err());
        assert!(
            std::fs::read_to_string(output)
                .unwrap()
                .contains("Strategy laboratory report")
        );
    }
}
