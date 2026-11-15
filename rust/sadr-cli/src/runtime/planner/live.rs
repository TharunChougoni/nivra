//! Bounded current-market observation. No financial writer or execution authority.
use super::*;
use chrono::Datelike;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;

fn eligible(
    i: &Instrument,
    q: &Quote,
    now: i64,
    cash: Amount,
    costs: &sadr_core::finance::FeeArithmetic,
) -> Result<()> {
    if !q.fresh(now, sadr_core::market::MAX_FINANCIAL_AGE_MS) {
        return Err("quote_stale_no_model".into());
    }
    let price = Portfolio::execution_with_costs(q.ask, i.tick, true, costs)?;
    if costs.affordable_cash_lots(price, cash, i64::MAX, false, 0)? == 0 {
        return Err("quote_unaffordable_no_model".into());
    }
    Ok(())
}

fn run(config: &Path, planner: &Path, output: &Path, symbols: &[String]) -> Result<Value> {
    let original: Config = serde_json::from_slice(
        &crate::read_bounded(config, 16384).map_err(|_| "preview_config_unavailable")?,
    )?;
    original.validate()?;
    data::verify_journal(&original)?;
    let book = rollover::current(&original)?;
    let (_, body) = rollover::bound_state(&book)?;
    let preserved: Portfolio = serde_json::from_str(&body)?;
    preserved.reconcile()?;
    let paper_costs = preserved.costs.clone().ok_or("paper_costs_required")?;
    // Diagnostics cannot replace a financial book or inspect an unresolved account.
    if preserved.halted || !preserved.closed || !preserved.positions.is_empty() {
        return Err("preview_requires_closed_flat_non_halted_book".into());
    }
    if !output.is_absolute()
        || output.exists()
        || output.starts_with(&book.directory)
        || symbols.is_empty()
        || symbols.len() > 40
        || symbols.iter().collect::<BTreeSet<_>>().len() != symbols.len()
    {
        return Err("live_preview_arguments_invalid".into());
    }
    sadr_store::native_grant::file_path_ok(output).map_err(|_| "preview_path_invalid")?;
    let settings = PlannerConfig::load_for_session(planner, book.session)?;
    settings.runtime_decision_owner_gate()?;
    let digest_role = settings.role(Trigger::Digest);
    if !settings.enabled || !digest_role.enabled {
        return Err("model_plan_unavailable_no_trades".into());
    }
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let now = millis();
    let today = local(now).date_naive();
    let mut observation = original.clone();
    (observation.session, observation.previous_session) = original.next_sessions(today)?;
    observation.directory = output.to_path_buf();
    observation.prior_config = None;
    observation.legacy_directory = None;
    let client = data::Client::new(&observation.market_data_transport)?;
    let universe = wide::preview_instruments(&client, &observation)?;
    let selected: Vec<_> = universe
        .into_iter()
        .filter(|i| symbols.contains(&i.symbol))
        .collect();
    if selected.len() != symbols.len() {
        return Err("preview_symbol_not_in_validated_universe".into());
    }
    let keys: Vec<_> = selected.iter().map(|i| i.key.as_str()).collect();
    let (requested, received, raw) = client.quotes(&original, &keys)?;
    rollover::atomic(
        &output.join("quote-receipt.json"),
        &json!({"requested_ms":requested,"received_ms":received,"body_sha256":digest(raw.as_bytes()),"body":raw}),
    )?;
    let response: Value = serde_json::from_str(&raw)?;
    if response["status"] != "success" {
        return Err("preview_quote_status".into());
    }
    let rows = response["data"].as_object().ok_or("preview_quote_rows")?;
    let cash = original.allocation()?;
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let checked = millis();
    let market_time = local(checked);
    let ordinary_window = market_time.weekday().number_from_monday() <= 5
        && (9 * 60 + 15..15 * 60 + 30).contains(&(market_time.hour() * 60 + market_time.minute()));
    for i in &selected {
        let matching: Vec<_> = rows
            .values()
            .filter(|r| r["instrument_token"] == i.key)
            .collect();
        let result = (|| -> Result<Quote> {
            if !ordinary_window {
                return Err("outside_market_window_no_model".into());
            }
            if matching.len() != 1 {
                return Err("quote_identity_missing_or_duplicate".into());
            }
            let q = data::parse_row(matching[0], received, &i.key, &i.symbol, i.tick)?;
            eligible(i, &q, checked, cash, &paper_costs)?;
            Ok(q)
        })();
        match result {
            Ok(q) => accepted.push(json!({"instrument":i,"quote":q,"volume":matching[0]["volume"],"ohlc":matching[0]["ohlc"]})),
            Err(_) => rejected.push(json!({"symbol":i.symbol,"reason":"quote_unavailable_stale_or_unaffordable"})),
        }
    }
    let input = json!({"session":today,"as_of_ms":checked,"receipt_ms":received,"paper_only":true,"diagnostic_only":true,"capital":cash,"cash":cash,"positions":{},"allowlist":selected,"shortlist":accepted,"exclusions":rejected,"fees":paper_costs.clone(),"limitations":["Snapshot ages while models reason; no execution or inferred fill","No historical features supplied; no claim of complete market coverage","No existing positions: sell-unheld and overspending must be rejected"]});
    rollover::atomic(&output.join("input.json"), &input)?;
    if accepted.is_empty() {
        let result = json!({"paper_only":true,"traded":false,"status":"no_eligible_live_quotes_no_model","model_calls":0,"observed":selected.len(),"eligible":0,"received_ms":received});
        rollover::atomic(&output.join("result.json"), &result)?;
        return Ok(result);
    }
    let base = settings
        .model_config
        .as_deref()
        .or(digest_role.model_config.as_deref())
        .ok_or("planner_model_config_required")?;
    let mut backend = Subscription::new(base);
    let guard = preview_guard();
    let before_usage = usage(&book)?;
    let digest_result = if digest_role.enabled {
        super::super::harness::run(
            &book,
            &settings,
            "digest",
            &digest_role,
            &format!(
                "Summarise untrusted dated market observations. Retain prices, identities, fees and uncertainty. No trade instructions. {input}"
            ),
            &input,
            &mut backend,
            &guard,
        )?
    } else {
        String::new()
    };
    rollover::atomic(
        &output.join("model-output.json"),
        &json!({"transport_evidence":backend.provenance(),"digest":digest_result}),
    )?;
    let after_usage = usage(&book)?;
    let calls = after_usage["digest"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(before_usage["digest"].as_u64().unwrap_or(0));
    let result = json!({"paper_only":true,"traded":false,"status":"validated_live_snapshot_digest",
        "model_calls":calls,"observed":symbols.len(),"eligible":accepted.len(),
        "received_ms":received,"completed_ms":millis(),"quote_age_at_completion_ms":millis()-received,
        "digest":serde_json::from_str::<Value>(&digest_result)?,"execution_authorized":false});
    rollover::atomic(&output.join("result.json"), &result)?;
    Ok(result)
}

pub(super) fn entry(config: &Path, planner: &Path, output: &Path, symbols: &[String]) -> ExitCode {
    match run(config, planner, output, symbols) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("live_preview_failed_preserve_artifacts_no_retry");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // Extended by the planner behavior test rather than adding a new suite case.
    pub(crate) fn freshness_and_cash() {
        let i = Instrument {
            key: "NSE_EQ|SYNTHETIC".into(),
            symbol: "SYNTHETIC".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        };
        let q = Quote {
            bid: Price::parse("99.95").unwrap(),
            ask: Price::parse("100").unwrap(),
            bid_units: 1,
            ask_units: 1,
            source: 10000,
            receipt: 10000,
        };
        assert!(
            eligible(
                &i,
                &q,
                12000,
                Amount::from_integer(5000),
                &crate::runtime::tests::test_costs()
            )
            .is_ok()
        );
        assert!(
            eligible(
                &i,
                &q,
                12001,
                Amount::from_integer(5000),
                &crate::runtime::tests::test_costs()
            )
            .is_err()
        );
        assert!(
            eligible(
                &i,
                &q,
                10000,
                Amount::from_integer(10),
                &crate::runtime::tests::test_costs()
            )
            .is_err()
        );
    }
}
