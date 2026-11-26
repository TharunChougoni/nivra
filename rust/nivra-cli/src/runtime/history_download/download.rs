use super::*;

pub(crate) fn download(
    transport: &mut impl Transport,
    clock: impl Fn() -> i64,
    instruments: &[Instrument],
    output: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    interval: Interval,
) -> Result<()> {
    download_controlled(
        transport,
        clock,
        instruments,
        output,
        start,
        end,
        interval,
        None,
        None,
    )
    .map(|_| ())
}
#[allow(clippy::too_many_arguments)] // Original download inputs plus an explicit cancellation/deadline guard.
pub(crate) fn download_controlled(
    transport: &mut impl Transport,
    clock: impl Fn() -> i64,
    instruments: &[Instrument],
    output: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    interval: Interval,
    control: Option<&PreparationControl>,
    calendar: Option<&Config>,
) -> Result<usize> {
    let mut failures = 0usize;
    let today = local(clock()).date_naive();
    let end = end.unwrap_or(today.checked_sub_days(Days::new(1)).ok_or("history_date")?);
    let start = start.unwrap_or(
        end.checked_add_days(Days::new(1))
            .and_then(|day| day.checked_sub_months(Months::new(12)))
            .ok_or("history_date")?,
    );
    // Upstox v3: minute history from January 2022, daily from January 2000;
    // 1-minute requests span one calendar month; daily requests span ten years.
    // https://upstox.com/developer/api-documentation/v3/get-historical-candle-data/
    if start > end
        || end >= today
        || start
            < NaiveDate::from_ymd_opt(
                if interval == Interval::Days {
                    2000
                } else {
                    2022
                },
                1,
                1,
            )
            .ok_or("history_date")?
        || instruments.is_empty()
    {
        return Err("history_range".into());
    }
    fs::create_dir_all(output)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(output.join("download.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| "history_download_locked")?;
    let _lock = AdvisoryLock::new(lock);
    let archive = market_data::open(&output.join("candles.sqlite"))
        .map_err(|_| "history_archive_unavailable")?;
    market_data::init_candles(&archive).map_err(|_| "history_archive_unavailable")?;
    // Discover old window layouts once. Coverage is the validated request range,
    // including empty responses; candle dates do not certify calendar completeness.
    let mut existing = BTreeMap::<(String, String), Vec<(NaiveDate, NaiveDate)>>::new();
    let selected_keys: BTreeSet<_> = instruments
        .iter()
        .map(|instrument| instrument.key.as_str())
        .collect();
    for entry in fs::read_dir(output)? {
        if let Some(control) = control {
            control.check()?;
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.ends_with(".receipt.json")) else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            return Err("history_receipt_not_regular".into());
        }
        let receipt = read_receipt(&entry.path()).map_err(Fail::from)?;
        if !interval.units().contains(&receipt.unit.as_str())
            || !selected_keys.contains(receipt.key.as_str())
            || receipt.end < start
            || receipt.start > end
        {
            continue;
        }
        if receipt.start > receipt.end
            || name
                != format!(
                    "{}.receipt.json",
                    range_id(&receipt.key, &receipt.unit, receipt.start, receipt.end)
                )
        {
            return Err("history_receipt_binding".into());
        }
        candles(&receipt)?;
        existing
            .entry((receipt.key, receipt.unit))
            .or_default()
            .push((receipt.start, receipt.end));
    }
    let mut inventory = Vec::new();
    'instrument: for instrument in instruments {
        for &unit in interval.units() {
            let mut ranges = existing
                .remove(&(instrument.key.clone(), unit.to_owned()))
                .unwrap_or_default();
            ranges.sort();
            let mut cursor = start;
            let mut gaps = Vec::new();
            for &(from, to) in &ranges {
                if cursor < from {
                    gaps.push((
                        cursor,
                        from.checked_sub_days(Days::new(1)).ok_or("history_date")?,
                    ));
                }
                cursor = cursor.max(to.checked_add_days(Days::new(1)).ok_or("history_date")?);
            }
            if cursor <= end {
                gaps.push((cursor, end));
            }
            // Preparation uses reviewed sessions, not calendar-day gaps. One
            // overlapping daily request catches up split gaps without changing
            // any original receipt or claiming that missing candles exist.
            if unit == "days"
                && let Some(calendar) = calendar
            {
                let mut missing = Vec::new();
                for (mut day, last) in gaps {
                    while day <= last {
                        if let Some(control) = control {
                            control.check()?;
                        }
                        if calendar.is_trading_day(day)? {
                            missing.push(day);
                        }
                        day = day.succ_opt().ok_or("history_date")?;
                    }
                }
                gaps = match (missing.first(), missing.last()) {
                    (Some(first), Some(last)) => vec![(*first, *last)],
                    _ => Vec::new(),
                };
            }
            for (mut from, gap_end) in gaps {
                while from <= gap_end {
                    if let Some(control) = control {
                        control.check()?;
                    }
                    let to = window_end(from, gap_end, unit)?;
                    ranges.push((from, to));
                    from = to.checked_add_days(Days::new(1)).ok_or("history_date")?;
                }
            }
            ranges.sort();
            for (from, to) in ranges {
                if let Some(control) = control {
                    control.check()?;
                }
                let id = range_id(&instrument.key, unit, from, to);
                let path = output.join(format!("{id}.receipt.json"));
                let excluded = output.join(format!("{id}.unavailable.json"));
                if excluded.exists() && transport.skip_unavailable() {
                    let prior: Value = serde_json::from_slice(
                        &crate::read_bounded(&excluded, 4096)
                            .map_err(|_| "history_exclusion_invalid")?,
                    )?;
                    if prior["key"] != instrument.key
                        || prior["unit"] != unit
                        || prior["start"] != from.to_string()
                        || prior["end"] != to.to_string()
                        || prior["status"] != "provider_http_400_unavailable"
                    {
                        return Err("history_exclusion_invalid".into());
                    }
                    inventory.push(prior);
                    continue;
                }
                let receipt: Receipt = if path.symlink_metadata().is_ok() {
                    if !path.symlink_metadata()?.is_file() {
                        return Err("history_receipt_not_regular".into());
                    }
                    read_receipt(&path).map_err(Fail::from)?
                } else {
                    let (requested_ms, received_ms, body) = match transport.fetch(
                        &instrument.key,
                        unit,
                        from,
                        to,
                    ) {
                        Ok(v) => v,
                        Err(error) => {
                            let code = failure_code(error.as_ref());
                            let retry_at = error
                                .downcast_ref::<data::RateLimited>()
                                .and_then(|limit| limit.retry_at_ms);
                            // Retain redacted per-range failures independently of the
                            // mutable pass projection, including aborted passes (#35).
                            let diagnostic = serde_json::to_vec(&json!({
                                "key":instrument.key,"unit":unit,"start":from,"end":to,
                                "recorded_ms":clock(),"failure":code,"retry_at_ms":retry_at
                            }))?;
                            let failure_path =
                                output.join(format!("{id}-{}.failure.json", digest(&diagnostic)));
                            immutable(&failure_path, &diagnostic)?;
                            if transport.skip_unavailable() && code == "http_400" {
                                let failure = json!({"status":"provider_http_400_unavailable","key":instrument.key,"unit":unit,"start":from,"end":to,"recorded_ms":clock(),"coverage":"missing","no_retry":true});
                                immutable(&excluded, &serde_json::to_vec(&failure)?)?;
                                inventory.push(failure);
                                continue;
                            }
                            if failures < transport.max_fetch_failures()
                                && transient_fetch_failure(&code)
                            {
                                failures += 1;
                                continue 'instrument;
                            }
                            return Err(error);
                        }
                    };
                    let receipt = Receipt {
                        key: instrument.key.clone(),
                        unit: unit.into(),
                        start: from,
                        end: to,
                        requested_ms,
                        received_ms,
                        body_sha256: digest(body.as_bytes()),
                        body,
                    };
                    // Archive before validation. A malformed successful response is retained,
                    // and reruns fail closed rather than replacing its receipt.
                    immutable(&path, &serde_json::to_vec(&receipt)?)?;
                    receipt
                };
                if receipt.key != instrument.key
                    || receipt.unit != unit
                    || receipt.start != from
                    || receipt.end != to
                {
                    return Err("history_receipt_binding".into());
                }
                let rows = candles(&receipt)?;
                if !market_data::record_candle_receipt(
                    &archive,
                    market_data::CandleReceipt {
                        range_id: &id,
                        instrument_key: &receipt.key,
                        unit: &receipt.unit,
                        start: &receipt.start.to_string(),
                        end: &receipt.end.to_string(),
                        requested_ms: receipt.requested_ms,
                        received_ms: receipt.received_ms,
                        body_sha256: &receipt.body_sha256,
                        body: &receipt.body,
                    },
                )
                .map_err(|_| "history_archive_unavailable")?
                {
                    return Err("history_archive_receipt_conflict".into());
                }
                // Operational coverage consumes validated original receipts,
                // not derived replay artifacts. Never rebuild an old shard as
                // a prerequisite for acquiring missing candles.
                let dates: BTreeSet<_> = if transport.receipts_only() {
                    BTreeSet::new()
                } else {
                    rows.iter().map(|(d, _)| *d).collect()
                };
                let mut shards = Vec::new();
                for date in dates {
                    let name = format!("{id}-{date}");
                    let at = date
                        .and_hms_opt(0, 0, 0)
                        .ok_or("history_date")?
                        .and_local_timezone(FixedOffset::east_opt(19800).ok_or("history_date")?)
                        .single()
                        .ok_or("history_date")?
                        .timestamp_millis();
                    let events = format!(
                        "{}\n",
                        json!({"at":at,"available_at":at,"kind":"screen","observations":[]})
                    );
                    let declaration = digest(LIMITATION.as_bytes());
                    let manifest = serde_json::to_vec(
                        &json!({"session":date,"warmup_start":date,"policy":nivra_core::execution::POLICY,"split":"development","receipt_clock":"virtual_historical","seed":0,"bindings":{"engine":nivra_core::replay::engine_fingerprint(),"strategy":declaration,"calendar":declaration,"fees":declaration,"slippage":declaration,"data":digest(events.as_bytes())},"universe":[instrument]}),
                    )?;
                    nivra_core::replay::evaluate(&manifest, events.as_bytes(), || true)?;
                    let manifest_path = output.join(format!("{name}.manifest.json"));
                    // Match a reviewed predecessor byte-for-byte; no arbitrary
                    // engine, rewritten bytes or relaxed financial/data bindings.
                    let prior = if manifest_path.symlink_metadata().is_ok() {
                        Some(fs::read(&manifest_path)?)
                    } else {
                        None
                    };
                    // Reuse a predecessor whose only differences are a well-formed
                    // engine fingerprint from an earlier release (#63) or the
                    // instrument's display symbol after an exchange rename (#63,
                    // ICICIPRULI -> ICICILIFE). Key, series, tick, policy and every
                    // data/financial binding must still match exactly.
                    let preserved = prior
                        .as_deref()
                        .filter(|bytes| reusable_manifest(bytes, &manifest))
                        .map(<[u8]>::to_vec);
                    immutable(&manifest_path, preserved.as_deref().unwrap_or(&manifest))?;
                    immutable(
                        &output.join(format!("{name}.events.jsonl")),
                        events.as_bytes(),
                    )?;
                    let evidence: Vec<_> = rows
                        .iter()
                        .filter(|(d, _)| *d == date)
                        .map(|(_, r)| r)
                        .collect();
                    immutable(
                        &output.join(format!("{name}.candles.json")),
                        &serde_json::to_vec(
                            &json!({"limitation":LIMITATION,"receipt":format!("{id}.receipt.json"),"receipt_sha256":digest(&fs::read(&path)?),"candles":evidence}),
                        )?,
                    )?;
                    shards.push(name);
                }
                inventory.push(json!({"receipt":format!("{id}.receipt.json"),"sha256":digest(&fs::read(&path)?),"key":instrument.key,"unit":unit,"start":from,"end":to,"candles":rows.len(),"shards":shards,"coverage":"provider response only; empty ranges and gaps are not certified trading holidays"}));
            }
        }
    }
    let mut index = json!({"schema":1,"start":start,"end":end,"limitation":LIMITATION,"binding_declarations":"strategy/calendar/fees/slippage hashes bind the limitation text, not authenticated execution inputs","universe":"current frozen paper universe; historical membership unverified","instruments":instruments,"ranges":inventory});
    // Bind the actual receipt layout: preserve legacy indexes when gap planning
    // or broader-range reuse produces a different inventory for the same dates.
    let mut identity = json!([start, end, instruments, interval.units(), index["ranges"]]);
    if transport.receipts_only() {
        index["artifact_mode"] = json!("receipts_only");
        identity
            .as_array_mut()
            .ok_or("history_index_invalid")?
            .push(json!("receipts_only"));
    }
    let plan = digest(&serde_json::to_vec(&identity)?);
    immutable(
        &output.join(format!("{plan}.index.json")),
        &serde_json::to_vec_pretty(&index)?,
    )?;
    Ok(failures)
}

pub(crate) fn prepare_daily(
    config: &Config,
    instruments: &[Instrument],
    output: &Path,
    start: NaiveDate,
    end: NaiveDate,
    control: PreparationControl,
) -> Result<usize> {
    let mut provider = Provider::new(config, Some(control.clone()), true)?;
    download_controlled(
        &mut provider,
        millis,
        instruments,
        output,
        Some(start),
        Some(end),
        Interval::Days,
        Some(&control),
        Some(config),
    )
}
