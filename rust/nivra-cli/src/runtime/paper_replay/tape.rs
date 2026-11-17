use super::*;

/// One frozen virtual-observation tape. It is built only from original
/// research-observation or protection receipts; screening receipts are never
/// accepted and no quote is fetched after the freeze.
pub(crate) struct VirtualTape {
    pub manifest: Vec<u8>,
    pub events: Vec<u8>,
    pub manifest_sha256: String,
    pub events_sha256: String,
    pub observations: usize,
    pub receipts: usize,
    pub purpose: QuotePurposeV1,
    /// SHA-256 of the exact original archived receipt body, never a digest of
    /// the re-encoded tape events.
    pub archive_sha256: String,
    pub universe_sha256: String,
    pub receipt_ms: i64,
    pub event_at_ms: i64,
    pub budget_truncated: bool,
    pub coverage_gap: Option<Value>,
}

/// Explicit tape selection mode. Entry freezes the latest eligible original
/// quote per key at or before T0; exit takes the FIRST eligible original
/// receipt in `[due, deadline]`. They are not interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TapeMode {
    EntryFreeze,
    ExitWindow,
}

/// A typed tape refusal that retains the original receipt provenance instead of
/// collapsing a compacted or budget-truncated source into a missing quote.
#[derive(Debug)]
pub(crate) struct TapeGap {
    pub reason: &'static str,
    pub gap: Value,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn virtual_observation_tape_bounded(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    capital: Amount,
    max_rows: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
    mode: TapeMode,
) -> std::result::Result<VirtualTape, TapeGap> {
    virtual_observation_tape_bounded_with(
        source,
        session,
        previous_session,
        keys,
        from_ms,
        to_ms,
        capital,
        max_rows,
        max_observations,
        max_tape_bytes,
        now,
        mode,
        false,
    )
}

/// Predecessor continuation read: the ORIGINAL closed book is opened with
/// `immutable=1`, so no predecessor `-wal`/`-shm` file is created or changed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn virtual_observation_tape_bounded_immutable(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    capital: Amount,
    max_rows: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
    mode: TapeMode,
) -> std::result::Result<VirtualTape, TapeGap> {
    virtual_observation_tape_bounded_with(
        source,
        session,
        previous_session,
        keys,
        from_ms,
        to_ms,
        capital,
        max_rows,
        max_observations,
        max_tape_bytes,
        now,
        mode,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn virtual_observation_tape_bounded_with(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    capital: Amount,
    max_rows: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
    mode: TapeMode,
    immutable: bool,
) -> std::result::Result<VirtualTape, TapeGap> {
    if capital <= Amount::ZERO
        || !(1..=10_000).contains(&max_rows)
        || !(1..=100_000).contains(&max_observations)
        || !(1..=16 * 1024 * 1024).contains(&max_tape_bytes)
    {
        return Err(TapeGap {
            reason: "observation_tape_invalid",
            gap: json!({"kind":"invalid_tape_request"}),
        });
    }
    let purposes: &[QuotePurposeV1] = match mode {
        TapeMode::EntryFreeze => &[QuotePurposeV1::ResearchObservation],
        TapeMode::ExitWindow => &[
            QuotePurposeV1::ResearchObservation,
            QuotePurposeV1::Protection,
        ],
    };
    let full = candidate_quotes_window_with(
        source,
        session,
        keys,
        from_ms,
        to_ms,
        max_rows,
        now,
        purposes,
        mode == TapeMode::EntryFreeze,
        immutable,
    )
    .map_err(|reason| TapeGap {
        reason: "observation_quote_unavailable",
        gap: json!({"kind":"quote_query_unavailable","detail":reason}),
    })?;
    let quotes = full["quotes"].as_array().cloned().unwrap_or_default();
    let compacted = full["compacted"].as_array().cloned().unwrap_or_default();
    let truncated = full["truncated"].as_bool().unwrap_or(false);
    let mut chosen = BTreeMap::<String, Value>::new();
    for quote in &quotes {
        let Some(key) = quote["key"].as_str() else {
            continue;
        };
        if !keys.contains(key) || quote["evidence_status"] != "ok" {
            continue;
        }
        if chosen.contains_key(key) {
            continue;
        }
        chosen.insert(key.to_owned(), quote.clone());
    }
    let missing = keys
        .iter()
        .filter(|key| !chosen.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if chosen.is_empty() {
        if let Some(first) = compacted.first() {
            return Err(TapeGap {
                reason: "observation_quote_compacted",
                gap: json!({
                    "kind":"compacted_receipt",
                    "session":session.to_string(),
                    "at_ms":first["event_at_ms"],
                    "receipt_ms":first["receipt_ms"],
                    "archive_sha256":first["archive_sha256"],
                    "key":null,
                    "purpose":null,
                    "first_receipt_unknown":true
                }),
            });
        }
        if truncated {
            return Err(TapeGap {
                reason: "observation_coverage_budget_truncated",
                gap: json!({
                    "kind":"coverage_budget_truncated",
                    "session":session.to_string(),
                    "missing_keys":missing,
                    "max_rows":max_rows
                }),
            });
        }
        return Err(TapeGap {
            reason: "observation_quote_unavailable",
            gap: json!({
                "kind":"missing_quote",
                "session":session.to_string(),
                "from_ms":from_ms,
                "to_ms":to_ms
            }),
        });
    }
    // The FIRST eligible exit is only known when no earlier compacted receipt
    // could have occupied the same window. The same rule protects the latest
    // entry freeze from a compacted newer receipt.
    let chosen_receipt = chosen
        .values()
        .filter_map(|quote| quote["receipt_ms"].as_i64())
        .min()
        .unwrap_or(0);
    let first_receipt_unknown = compacted.iter().any(|row| {
        row["receipt_ms"]
            .as_i64()
            .is_some_and(|receipt| match mode {
                // Entry freezes the latest quote; a newer compacted receipt hides it.
                TapeMode::EntryFreeze => receipt >= chosen_receipt,
                // Exit takes the first receipt; an older compacted receipt hides it.
                TapeMode::ExitWindow => receipt <= chosen_receipt,
            })
    });
    if first_receipt_unknown {
        return Err(TapeGap {
            reason: "observation_quote_first_receipt_unknown",
            gap: json!({
                "kind":"first_receipt_unknown",
                "session":session.to_string(),
                "chosen_receipt_ms":chosen_receipt,
                "compacted":compacted,
                "first_receipt_unknown":true
            }),
        });
    }
    let mut observations = Vec::new();
    let mut purpose = QuotePurposeV1::ResearchObservation;
    let mut archive_sha256 = String::new();
    let mut receipt_ms = 0i64;
    let mut event_at_ms = 0i64;
    for (key, quote) in &chosen {
        let quote_purpose = QuotePurposeV1::from_archive(quote["purpose"].as_str())
            .unwrap_or(QuotePurposeV1::ResearchObservation);
        purpose = quote_purpose;
        archive_sha256 = quote["archive_sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        receipt_ms = receipt_ms.max(quote["receipt_ms"].as_i64().unwrap_or(0));
        event_at_ms = event_at_ms.max(quote["event_at_ms"].as_i64().unwrap_or(0));
        observations.push(json!({
            "key":key,
            "quote":{
                "bid":quote["bid"],
                "ask":quote["ask"],
                "bid_units":quote["bid_units"],
                "ask_units":quote["ask_units"],
                "source":quote["source_ms"],
                "receipt":quote["receipt_ms"]
            },
            "lower_circuit_limit":quote["lower_circuit_limit"],
            "upper_circuit_limit":quote["upper_circuit_limit"],
            "volume":quote["volume"],
            // Per-quote ORIGINAL receipt provenance. A multi-key tape must not
            // share one global archive SHA.
            "archive_sha256":quote["archive_sha256"],
            "event_at_ms":quote["event_at_ms"],
            "available_at_ms":quote["available_at_ms"],
            "purpose":quote["purpose"]
        }));
    }
    if observations.is_empty() || observations.len() > max_observations {
        return Err(TapeGap {
            reason: "observation_tape_too_large",
            gap: json!({"kind":"observation_budget","observations":observations.len()}),
        });
    }
    let coverage_gap = if missing.is_empty() {
        None
    } else {
        Some(json!({
            "kind":"coverage_budget_truncated",
            "missing_keys":missing,
            "max_rows":max_rows,
            "truncated":truncated
        }))
    };
    let kind = match purpose {
        QuotePurposeV1::ResearchObservation => "virtual_observation",
        QuotePurposeV1::Protection => "protection",
        QuotePurposeV1::Screen => {
            return Err(TapeGap {
                reason: "observation_tape_invalid",
                gap: json!({"kind":"screen_purpose"}),
            });
        }
    };
    let events = format!(
        "{}\n",
        serde_json::to_string(&json!({
            "at":event_at_ms,
            "available_at":receipt_ms,
            "kind":kind,
            "observations":observations
        }))
        .map_err(|_| TapeGap {
            reason: "observation_tape_invalid",
            gap: json!({"kind":"event_encoding"}),
        })?
    )
    .into_bytes();
    if events.len() as u64 > max_tape_bytes {
        return Err(TapeGap {
            reason: "observation_tape_too_large",
            gap: json!({"kind":"tape_bytes","bytes":events.len()}),
        });
    }
    let universe: Value = serde_json::from_slice(
        &crate::read_bounded(&source.join("wide-universe.json"), 8_000_000).map_err(|_| {
            TapeGap {
                reason: "observation_tape_invalid",
                gap: json!({"kind":"universe_unavailable"}),
            }
        })?,
    )
    .map_err(|_| TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"universe_invalid"}),
    })?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())
        .map_err(|_| TapeGap {
            reason: "observation_tape_invalid",
            gap: json!({"kind":"universe_invalid"}),
        })?;
    let universe_sha256 = full["universe_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let events_sha256 = nivra_core::replay::digest(&events);
    let previous_session = previous_session.unwrap_or(session.pred_opt().ok_or(TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"previous_session"}),
    })?);
    let per_key = chosen
        .iter()
        .map(|(key, quote)| {
            (
                key.clone(),
                json!({
                    "archive_sha256":quote["archive_sha256"],
                    "event_at_ms":quote["event_at_ms"],
                    "available_at_ms":quote["available_at_ms"],
                    "source_ms":quote["source_ms"],
                    "receipt_ms":quote["receipt_ms"],
                    "purpose":quote["purpose"],
                    "key":key
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let manifest = serde_json::to_vec(&json!({
        "schema":1,
        "session":session,
        "previous_session":previous_session,
        "capital":capital,
        "universe":instruments,
        "events_sha256":events_sha256,
        "synthetic":false,
        "max_observation_gap_ms":1,
        "history_directory":null,
        "paper_costs":null,
        "provenance":{
            "source":"original_paper_quote_journal_read_only",
            "quote_authority":"research_observation_virtual_only",
            "not_protection":true,
            "research_capital_assumption":true,
            "account_funds_claim":false,
            "purpose":purpose.as_str(),
            "mode":match mode { TapeMode::EntryFreeze => "entry_freeze", TapeMode::ExitWindow => "exit_window" },
            // Whole-tape identity is separate from any one key's original
            // receipt SHA. `archive_sha256` is only set for a single-key tape.
            "archive_sha256":if chosen.len() == 1 { json!(archive_sha256) } else { Value::Null },
            "whole_tape_sha256":digest(&events),
            "per_key":per_key,
            "universe_sha256":universe_sha256,
            "receipt_ms":receipt_ms,
            "budget_truncated":truncated,
            "coverage_gap":coverage_gap
        }
    }))
    .map_err(|_| TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"manifest_encoding"}),
    })?;
    if manifest.len() > 512 * 1024 {
        return Err(TapeGap {
            reason: "observation_tape_too_large",
            gap: json!({"kind":"manifest_bytes","bytes":manifest.len()}),
        });
    }
    nivra_cli::strategy_archive::validate_archive(&manifest, &events).map_err(|_| TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"archive_invalid"}),
    })?;
    Ok(VirtualTape {
        manifest_sha256: nivra_core::replay::digest(&manifest),
        events_sha256,
        manifest,
        events,
        observations: observations.len(),
        receipts: 1,
        purpose,
        // A multi-key tape has no single original receipt SHA. Never expose
        // the last key's SHA as if it were the tape's receipt.
        archive_sha256: if chosen.len() == 1 {
            archive_sha256
        } else {
            String::new()
        },
        universe_sha256,
        receipt_ms,
        event_at_ms,
        budget_truncated: truncated || coverage_gap.is_some(),
        coverage_gap,
    })
}
