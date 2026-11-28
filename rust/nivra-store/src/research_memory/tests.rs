use super::*;
use chrono::NaiveDate;
use nivra_core::autoresearch::{Comparator, Verdict, keep, usd_micros};
use nivra_core::autoresearch::{Regime, Score, Stage};
use nivra_core::finance::Amount;
use std::os::unix::fs::PermissionsExt;

fn day() -> NaiveDate {
    "2026-11-27".parse().unwrap()
}

fn limits(variants: u32) -> DayLimits {
    DayLimits {
        variants,
        turns: 4,
        calls: 20,
        usd_micros: usd_micros(1.0),
    }
}

fn campaign(store: &mut Store, cap: u32) -> Campaign {
    store
        .ensure_campaign(&NewCampaign {
            name: "bars-v1",
            segment: "nse_eq",
            manifest: b"{\"window\":\"frozen\"}",
            development_from: "2025-11-01".parse().unwrap(),
            development_to: "2026-11-26".parse().unwrap(),
            selection_from: "2026-08-26".parse().unwrap(),
            trial_cap: cap,
            now: 1,
        })
        .unwrap()
}

fn idea(store: &mut Store, campaign: i64, mechanism: &str) -> Idea {
    store
        .record_idea(&NewIdea {
            campaign_id: campaign,
            origin_request_key: None,
            family: "breakout",
            regime: Regime::Trend,
            mechanism,
            hypothesis: "Breakouts on volume continue for a day",
            falsifier: "Net P&L after costs is not above cash",
            comparator: "cash",
            parent_experiment_id: None,
            now: 2,
        })
        .unwrap()
        .0
}

fn submit<'a>(idea: i64, key: &'a str, blob: &'a str, generation: i64) -> Submission<'a> {
    Submission {
        idea_id: idea,
        stage: Stage::Bars,
        quote_window: None,
        source_path: "scripts/breakout.py",
        source_blob: blob,
        params_blob: blob,
        manifest_blob: blob,
        evaluator_digest: nivra_core::autoresearch::EVALUATOR,
        experiment_key: key,
        lease_generation: generation,
        deadline_ms: 10_000,
        day: day(),
        limits: limits(4),
        now: 3,
    }
}

fn scored(net: i64, blob: &str) -> Outcome {
    let score = Score {
        net_pnl: Amount::from_integer(net),
        max_drawdown_bps: 50,
        closed_trades: 11,
        open_positions: 0,
    };
    Outcome::Scored {
        decision: keep(
            &score,
            &[Comparator::Cash],
            nivra_core::autoresearch::Rule {
                max_drawdown_bps: 300,
                require_flat: false,
            },
        ),
        score,
        report_blob: blob.to_owned(),
    }
}

#[test]
fn a_trial_is_reserved_once_per_key_and_counted_against_the_campaign() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let campaign = campaign(&mut store, 2);
    let blob = store.put_object(b"def on_bars(): pass").unwrap();
    let lease = store
        .acquire_lease("coordinator", "test", 1, 0, 1_000, &|_| true)
        .unwrap()
        .unwrap();
    let first = idea(&mut store, campaign.id, "volume breakout");
    let Reserved::New(experiment, attempt) = store
        .reserve(&submit(first.id, &"a".repeat(64), &blob, lease.generation))
        .unwrap()
    else {
        panic!("first submission reserves a trial");
    };
    assert_eq!(experiment.ordinal, 1);
    // A duplicate submission returns the same record and spends nothing.
    assert!(matches!(
        store.reserve(&submit(first.id, &"a".repeat(64), &blob, lease.generation)).unwrap(),
        Reserved::Existing(ref same) if same.id == experiment.id
    ));
    // A restart after enqueue binds the job once; a stale generation cannot.
    assert!(
        store
            .bind_job(attempt.id, "job-1", 1, lease.generation, 4)
            .unwrap()
    );
    assert!(
        !store
            .bind_job(attempt.id, "job-1", 1, lease.generation + 1, 4)
            .unwrap()
    );
    // A preemption keeps the trial and runs a second attempt of the same key.
    let preempted = Outcome::Preempted {
        reason: "decision_preempted".into(),
    };
    assert!(
        store
            .finish(attempt.id, lease.generation, &preempted, 5)
            .unwrap()
    );
    let again = store
        .next_attempt(experiment.id, lease.generation, 20_000, 6)
        .unwrap();
    assert_eq!(again.attempt_no, 2);
    assert!(
        store
            .finish(again.id, lease.generation, &scored(-10, &blob), 7)
            .unwrap()
    );
    assert!(
        !store
            .finish(again.id, lease.generation, &scored(99, &blob), 8)
            .unwrap()
    );
    assert_eq!(store.trials(campaign.id).unwrap(), 1);
    let done = store.experiment(experiment.id).unwrap().unwrap();
    assert_eq!(done.verdict.as_deref(), Some("reject"));
    assert_eq!(
        store.idea(first.id).unwrap().unwrap().status,
        IdeaStatus::Rejected
    );
    assert_eq!(store.day_spend(day()).unwrap().variants, 1);

    let second = idea(&mut store, campaign.id, "gap reversal");
    assert!(matches!(
        store
            .reserve(&submit(second.id, &"b".repeat(64), &blob, lease.generation))
            .unwrap(),
        Reserved::New(..)
    ));
    let third = idea(&mut store, campaign.id, "range fade");
    assert_eq!(
        store
            .reserve(&submit(third.id, &"c".repeat(64), &blob, lease.generation))
            .unwrap(),
        Reserved::Refused("trial_allowance_spent")
    );
}

#[test]
fn trial_sixty_one_is_refused_and_confirmation_opens_once_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let campaign = campaign(&mut store, 60);
    let blob = store.put_object(b"frozen evidence").unwrap();
    let lease = store
        .acquire_lease("coordinator", "test", 1, 0, 1_000, &|_| true)
        .unwrap()
        .unwrap();
    let mut finalist = None;
    for trial in 1..=61 {
        let proposal = idea(&mut store, campaign.id, &format!("hypothesis {trial}"));
        let key = format!("{trial:064x}");
        let mut submission = submit(proposal.id, &key, &blob, lease.generation);
        submission.day = day()
            .checked_add_days(chrono::Days::new(u64::from((trial - 1) / 30)))
            .unwrap();
        submission.limits = limits(30);
        let reserved = store.reserve(&submission).unwrap();
        if trial == 61 {
            assert_eq!(reserved, Reserved::Refused("trial_allowance_spent"));
            break;
        }
        let Reserved::New(experiment, attempt) = reserved else {
            panic!("trial {trial}: {reserved:?}")
        };
        assert_eq!(experiment.ordinal, trial);
        let outcome = scored(if trial == 1 { 10 } else { -10 }, &blob);
        store
            .finish(attempt.id, lease.generation, &outcome, 4)
            .unwrap();
        if trial == 1 {
            finalist = Some(experiment.id);
        }
    }
    assert_eq!(store.trials(campaign.id).unwrap(), 60);
    let from = "2026-12-01".parse().unwrap();
    let to = "2027-01-31".parse().unwrap();
    let finalist = finalist.unwrap();
    assert!(
        store
            .freeze_finalist(campaign.id, finalist, from, to)
            .unwrap()
    );
    assert!(store.holdout_overlaps(from, to).unwrap());
    assert!(store.open_holdout_once(campaign.id).unwrap());
    assert!(
        store
            .finish_holdout(campaign.id, "reject", Some(&blob))
            .unwrap()
    );
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    assert!(!store.open_holdout_once(campaign.id).unwrap());
    assert!(
        !store
            .freeze_finalist(campaign.id, finalist, from, to)
            .unwrap()
    );
    let record = store.holdout(campaign.id).unwrap().unwrap();
    assert_eq!(record.verdict.as_deref(), Some("reject"));
    assert_eq!(record.evidence.as_deref(), Some(blob.as_str()));
    let history = store
        .history(&HistoryQuery {
            campaign_id: Some(campaign.id),
            family: None,
            mechanism: Some("hypothesis 2"),
            parent_experiment_id: None,
            before_id: None,
            limit: 20,
        })
        .unwrap();
    assert_eq!(history.experiments[0].verdict.as_deref(), Some("reject"));
}

#[test]
fn leases_survive_expiry_while_their_holder_lives_and_budgets_settle_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let held = store
        .acquire_lease("coordinator", "controller", 10, 0, 100, &|_| true)
        .unwrap()
        .unwrap();
    assert!(
        store
            .acquire_lease("coordinator", "timer", 11, 500, 100, &|_| true)
            .unwrap()
            .is_none(),
        "an expired lease of a live holder authorizes no second worker"
    );
    let taken = store
        .acquire_lease("coordinator", "timer", 11, 500, 100, &|_| false)
        .unwrap()
        .unwrap();
    assert_eq!(taken.generation, held.generation + 1);

    let limits = limits(1);
    assert_eq!(
        store
            .reserve_turn("turn-1", day(), 8, 400_000, &limits, 1)
            .unwrap(),
        Ok(())
    );
    assert_eq!(
        store
            .reserve_turn("turn-2", day(), 8, 700_000, &limits, 2)
            .unwrap(),
        Err("day_usd_exhausted")
    );
    assert!(store.settle_turn("turn-1", 3, 120_000).unwrap());
    assert!(!store.settle_turn("turn-1", 3, 120_000).unwrap());
    let spent = store.day_spend(day()).unwrap();
    assert_eq!(
        (spent.turns, spent.calls, spent.usd_micros),
        (1, 3, 120_000)
    );
    let tomorrow = day().succ_opt().unwrap();
    assert_eq!(store.day_spend(tomorrow).unwrap(), DaySpend::default());
}

#[test]
fn one_skill_revision_stays_active_and_a_backup_restores_every_referenced_object() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let campaign = campaign(&mut store, 10);
    let blob = store.put_object(b"report bytes").unwrap();
    let idea = idea(&mut store, campaign.id, "volume breakout");
    let Reserved::New(experiment, attempt) = store
        .reserve(&submit(idea.id, &"d".repeat(64), &blob, 1))
        .unwrap()
    else {
        panic!("reserved");
    };
    store.finish(attempt.id, 1, &scored(80, &blob), 4).unwrap();
    assert_eq!(store.cells(campaign.id).unwrap().len(), 1);
    let finding = store
        .record_finding(&NewFinding {
            experiment_id: experiment.id,
            kind: "positive",
            claim: "Volume breakouts beat cash after costs",
            scope: "NSE cash, daily bars",
            counterevidence: "Two losing months",
            next_test: "Tighten the exit",
            now: 5,
        })
        .unwrap();
    assert_eq!(finding.evidence_digest.as_deref(), Some(blob.as_str()));

    let (draft, new) = store
        .propose_skill(
            "fold-check",
            b"---\nname: fold-check\n---\n",
            &[finding.id],
            6,
        )
        .unwrap();
    assert!(new);
    store
        .review_skill(
            "fold-check",
            &draft.digest,
            true,
            "works twice",
            "review",
            7,
        )
        .unwrap();
    let (next, _) = store
        .propose_skill("fold-check", b"---\nname: fold-check\n---\nv2\n", &[], 8)
        .unwrap();
    assert_eq!(next.parent_digest.as_deref(), Some(draft.digest.as_str()));
    store
        .review_skill("fold-check", &next.digest, true, "better", "review", 9)
        .unwrap();
    let approved = store.skills(Some("approved")).unwrap();
    assert_eq!(approved.len(), 1);
    assert_eq!(approved[0].digest, next.digest);

    let backup = dir.path().join("backup");
    let report = store.backup(&backup).unwrap();
    assert!(report.objects >= 4);
    // Restore into a fresh root and resolve the original evidence there.
    let restored_root = dir.path().join("restored");
    std::fs::create_dir(&restored_root).unwrap();
    for entry in ["research.sqlite", "objects"] {
        std::process::Command::new("cp")
            .arg("-a")
            .arg(backup.join(entry))
            .arg(&restored_root)
            .status()
            .unwrap();
    }
    assert_eq!(verify(&restored_root).unwrap(), report);
    let restored = Store::open(&restored_root).unwrap();
    let evidence = restored.finding(finding.id).unwrap().unwrap();
    assert_eq!(
        restored
            .object(evidence.evidence_digest.as_deref().unwrap())
            .unwrap(),
        b"report bytes"
    );
    // A damaged object fails verification.
    let damaged = objects::object_path(&restored_root, &blob).unwrap();
    std::fs::set_permissions(&damaged, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&damaged, b"tampered").unwrap();
    assert_eq!(
        verify(&restored_root).unwrap_err(),
        Error("research_object_digest_mismatch")
    );
    assert_eq!(Verdict::Keep.as_str(), "keep");
}
