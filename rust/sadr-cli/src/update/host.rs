use super::*;

pub(crate) fn command(program: &str, args: &[&str]) -> Result<(i32, Vec<u8>)> {
    let (code, out, _) = command_in(program, args, None)?;
    Ok((code, out))
}
pub(crate) fn git_command(args: &[&str], directory: &Path) -> Command {
    let mut command = Command::new("git");
    command.args(args).current_dir(directory);
    // A shell's checkout, worktree, object/config overrides must not redirect the
    // updater's private bare repository. SSH still uses its own deploy-key alias.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes -oConnectTimeout=15")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}
pub(crate) fn command_in(
    program: &str,
    args: &[&str],
    directory: Option<&Path>,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let mut builder = if program == "git" {
        git_command(args, directory.ok_or("git_stable_directory_required")?)
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    let mut child = builder
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "update_command_unavailable")?;
    let stderr = child.stderr.take().ok_or("update_command_failed")?;
    let errors = thread::spawn(move || {
        let mut data = Vec::new();
        stderr.take(16385).read_to_end(&mut data).map(|_| data)
    });
    let stdout = child.stdout.take().ok_or("update_command_failed")?;
    let reader = thread::spawn(move || {
        let mut data = Vec::new();
        stdout
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map(|_| data)
    });
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|_| "update_command_failed")? {
            let data = reader
                .join()
                .map_err(|_| "update_command_failed")?
                .map_err(|_| "update_command_failed")?;
            if data.len() > 64 * 1024 * 1024 {
                return Err("update_command_output_limit");
            }
            let errors = errors
                .join()
                .map_err(|_| "update_command_failed")?
                .map_err(|_| "update_command_failed")?;
            return Ok((status.code().unwrap_or(-1), data, errors));
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("update_command_timeout");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
pub(crate) fn safe_git_stderr(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let mut hide = false;
    raw.split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            let sensitive = [
                "token",
                "bearer",
                "authorization",
                "password",
                "secret",
                "credential",
                "private-key",
                "hostname",
                "host",
                "identity",
            ]
            .iter()
            .any(|s| lower.contains(s));
            let redact = hide
                || sensitive
                || word.len() >= 24
                || word.contains("://")
                || word.contains('@')
                || word.contains('=')
                || word.contains('/')
                || word.contains('.')
                || word.contains("eyJ");
            hide = sensitive;
            if redact {
                "[redacted]".to_owned()
            } else {
                word.chars().filter(|c| !c.is_control()).collect()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(4096)
        .collect()
}
pub(crate) fn git_run(cache: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let directory = cache.parent().ok_or("invalid_cache_path")?;
    let step = if args.first() == Some(&"--git-dir") {
        args.get(2)
    } else {
        args.first()
    }
    .copied()
    .unwrap_or("unknown");
    for attempt in 1..=3 {
        let result = command_in("git", args, Some(directory));
        match result {
            Ok((0, data, _)) => return Ok(data),
            other => {
                let (code, detail, locked) = match other {
                    Ok((code, _, errors)) => {
                        let text = String::from_utf8_lossy(&errors);
                        let locked = text.contains("cannot lock ref")
                            || text.contains(".lock") && text.contains("File exists");
                        (code, safe_git_stderr(&errors), locked)
                    }
                    Err(code) => (-1, code.to_owned(), false),
                };
                let record = serde_json::json!({"step":step,"exit_code":code,"attempt":attempt,"stderr":detail,"at":now_utc().to_rfc3339()});
                eprintln!("release_git_diagnostic: {record}");
                let path = directory.join("git-diagnostic.json");
                let next = directory.join(format!("git-diagnostic.{}.next", std::process::id()));
                write_new(
                    &next,
                    &serde_json::to_vec(&record).map_err(|_| "update_record_failed")?,
                    false,
                )?;
                fs::rename(next, path).map_err(|_| "update_record_failed")?;
                if locked {
                    return Err("update_git_cache_locked_requires_review");
                }
                // Only idempotent remote reads/fetches are retried, never effects.
                if attempt == 3 || !matches!(step, "ls-remote" | "fetch") {
                    return Err("release_git_failed");
                }
                thread::sleep(Duration::from_secs(attempt));
            }
        }
    }
    Err("release_git_failed")
}
pub(crate) fn git(cache: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut all = vec!["--git-dir", cache.to_str().ok_or("invalid_cache_path")?];
    all.extend(args);
    git_run(cache, &all)
}
pub(crate) fn service_active() -> Result<bool> {
    match command(
        "systemctl",
        &[
            "--user",
            "is-active",
            "--quiet",
            "ind-sadr-rust-paper.service",
        ],
    )?
    .0
    {
        0 => Ok(true),
        3 | 4 => Ok(false),
        _ => Err("update_service_unknown"),
    }
}
pub(crate) fn service_restarts() -> Result<u64> {
    let (code, body) = command(
        "systemctl",
        &[
            "--user",
            "show",
            "--property=NRestarts",
            "--value",
            "ind-sadr-rust-paper.service",
        ],
    )?;
    if code != 0 {
        return Err("update_service_unknown");
    }
    String::from_utf8_lossy(&body)
        .trim()
        .parse()
        .map_err(|_| "update_service_unknown")
}
pub(crate) fn paper_health(body: &[u8], tag: &str) -> Result<()> {
    paper_health_at(body, tag, now_utc().timestamp_millis())
}

pub(crate) fn paper_health_at(body: &[u8], tag: &str, now_ms: i64) -> Result<()> {
    if body.len() > 65536 {
        return Err("update_dashboard_health_failed");
    }
    let view: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "update_dashboard_health_failed")?;
    // Genuine older v1 health snapshots have as_of_ms but no heartbeat.
    // Timestamp-less fixtures cannot establish controller liveness.
    if crate::runtime::operational_health::controller_liveness(&view, now_ms).status
        != crate::runtime::operational_health::LivenessStatus::Healthy
        || view["paper_only"] != true
        || view["live_enabled"] != false
        || !view["status"].is_string()
        || view.get("version").is_some_and(|v| {
            v.as_str()
                .is_none_or(|v| v.trim_start_matches('v') != tag.trim_start_matches('v'))
        })
    {
        return Err("update_dashboard_health_failed");
    }
    Ok(())
}
pub(crate) fn poll_health(
    mut service: impl FnMut() -> Result<u64>,
    mut probe: impl FnMut() -> Result<()>,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<()> {
    let restarts = service()?;
    loop {
        if service()? != restarts {
            return Err("update_service_unhealthy");
        }
        let result = probe();
        if service()? != restarts {
            return Err("update_service_unhealthy");
        }
        if result.is_ok() {
            return Ok(());
        }
        let remaining = Duration::from_secs(60).saturating_sub(elapsed());
        if remaining.is_zero() {
            return Err("update_dashboard_health_failed");
        }
        sleep(remaining.min(Duration::from_secs(2)));
    }
}
pub(crate) struct RealHost {
    pub(crate) agent: ureq::Agent,
    pub(crate) current: PathBuf,
    pub(crate) token_file: PathBuf,
    pub(crate) remote: String,
    pub(crate) cache: PathBuf,
    pub(crate) paper: PathBuf,
    pub(crate) now: Option<chrono::DateTime<chrono::FixedOffset>>,
    pub(crate) operator_stopped: bool,
}
impl Host for RealHost {
    fn fetch_optional(&mut self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let (_, file) = name.split_once('/').ok_or("invalid_asset_request")?;
        let tag = name.split_once('/').ok_or("invalid_asset_request")?.0;
        let entries = git(
            &self.cache,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                &format!("refs/assets/{tag}"),
            ],
        )?;
        if !String::from_utf8_lossy(&entries)
            .lines()
            .any(|entry| entry == file)
        {
            return Ok(None);
        }
        self.fetch(name, limit).map(Some)
    }
    fn keep_stopped(&self) -> bool {
        self.operator_stopped
    }
    fn remote(&self) -> Option<String> {
        Some(self.remote.clone())
    }
    fn fetch(&mut self, name: &str, limit: usize) -> Result<Vec<u8>> {
        let data = if name == "tags" {
            git_run(&self.cache, &["ls-remote", "--tags", &self.remote])?
        } else {
            let (tag, file) = name.split_once('/').ok_or("invalid_asset_request")?;
            // git init --bare is idempotent, under the same owner lock as fetch.
            if !exists(&self.cache) {
                fs::create_dir(&self.cache).map_err(|_| "update_directory_unavailable")?;
                fs::set_permissions(&self.cache, fs::Permissions::from_mode(0o700))
                    .map_err(|_| "update_permissions_failed")?;
            }
            secure_directory(&self.cache)?;
            if exists(&self.cache.join("HEAD"))
                && git(&self.cache, &["rev-parse", "--is-bare-repository"])? != b"true\n"
            {
                return Err("update_cache_not_bare");
            }
            git_run(
                &self.cache,
                &[
                    "init",
                    "--bare",
                    self.cache.to_str().ok_or("invalid_cache_path")?,
                ],
            )?;
            secure_directory(&self.cache)?;
            if file == "release.json" {
                // CI publishes the asset ref minutes after the tag; absent means pending.
                let listed = git_run(
                    &self.cache,
                    &[
                        "ls-remote",
                        &self.remote,
                        &format!("refs/release-assets/{tag}"),
                    ],
                )?;
                if listed.iter().all(u8::is_ascii_whitespace) {
                    return Err("release_assets_pending");
                }
                git(
                    &self.cache,
                    &[
                        "fetch",
                        "--atomic",
                        "--no-tags",
                        "--no-auto-maintenance",
                        &self.remote,
                        &format!("refs/release-assets/{tag}:refs/assets/{tag}"),
                        &format!("refs/tags/{tag}:refs/tags/{tag}"),
                    ],
                )?;
                if git(
                    &self.cache,
                    &["cat-file", "-t", &format!("refs/tags/{tag}")],
                )? != b"tag\n"
                {
                    return Err("annotated_tag_required");
                }
                let m = git(
                    &self.cache,
                    &["show", &format!("refs/assets/{tag}:release.json")],
                )?;
                let manifest: Manifest =
                    serde_json::from_slice(&m).map_err(|_| "invalid_release_manifest")?;
                let commit = git(
                    &self.cache,
                    &["rev-parse", &format!("refs/tags/{tag}^{{commit}}")],
                )?;
                if String::from_utf8_lossy(&commit).trim() != manifest.source_commit {
                    return Err("release_identity_mismatch");
                }
                m
            } else {
                git(&self.cache, &["show", &format!("refs/assets/{tag}:{file}")])?
            }
        };
        if data.len() > limit {
            return Err("release_download_limit");
        }
        Ok(data)
    }
    fn version(&mut self, binary: &Path) -> Result<String> {
        let (code, bytes) = command(
            binary.to_str().ok_or("invalid_binary_path")?,
            &["--version"],
        )?;
        if code != 0 {
            return Err("installed_version_unavailable");
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "installed_version_unavailable")?
            .trim();
        let version = text
            .strip_prefix("sadr-cli ")
            .ok_or("installed_version_unavailable")?;
        Version::parse(version)?;
        Ok(format!("v{version}"))
    }
    fn stop(&mut self) -> Result<()> {
        if command(
            "systemctl",
            &["--user", "stop", "ind-sadr-rust-paper.service"],
        )?
        .0 != 0
        {
            return Err("update_stop_failed");
        }
        Ok(())
    }
    fn guard(&mut self, force: bool) -> Result<Admission> {
        if self.now.is_none()
            && command(
                "timedatectl",
                &["show", "--property=NTPSynchronized", "--value"],
            )? != (0, b"yes\n".to_vec())
        {
            return Err("update_clock_untrusted");
        }
        if self.operator_stopped {
            // The operator already stopped the controller; the book is preserved and
            // only an explicit resume restarts trading, so the blackout does not apply.
            let (code, _) = command(
                "systemctl",
                &[
                    "--user",
                    "is-active",
                    "--quiet",
                    "ind-sadr-rust-paper.service",
                ],
            )?;
            if code == 0 {
                return Ok(Admission::Deferred(
                    "deferred_operator_stopped_service_active",
                ));
            }
            return Ok(
                if crate::runtime::stopped_book_safe(&self.paper).unwrap_or(false) {
                    Admission::Safe
                } else {
                    Admission::Deferred("deferred_operator_stopped_book_not_safe")
                },
            );
        }
        let now = self
            .now
            .unwrap_or_else(|| now_utc().fixed_offset())
            .with_timezone(&chrono::FixedOffset::east_opt(19800).expect("IST"));
        let Ok(book) = crate::runtime::update_book(&self.paper) else {
            // Unknown book state never authorises a restart with open exposure.
            return Ok(Admission::Deferred("deferred_book_unreadable"));
        };
        let open = book.open_positions > 0;
        let pending = book.pending_orders > 0;
        // The NSE calendar decides whether the exchange is closed; an unreadable
        // calendar leaves the clock window in force (deferring).
        let calendar_closed =
            crate::runtime::market_closed(&self.paper, now.timestamp_millis()).unwrap_or(false);
        let market = market_open(now) && !calendar_closed;
        if (open || pending) && book.halted {
            // A risk halt requires operator review; force never bypasses it.
            return Ok(Admission::Deferred("deferred_halted_book"));
        }
        if open || pending {
            let exposure = if open {
                "open_positions"
            } else {
                "pending_orders"
            };
            if force {
                return Ok(Admission::Forced(if market {
                    if open {
                        "market_open_open_positions"
                    } else {
                        "market_open_pending_orders"
                    }
                } else {
                    exposure
                }));
            }
            // A closed, settled model-led book carrying positions can be adopted
            // read-only outside market hours; it never mutates or clears the book
            // and the successor still starts through the reviewed carry path (#153).
            if open && !pending && !market && book.carried_safe {
                return Ok(Admission::Safe);
            }
            return Ok(Admission::Deferred(match (market, exposure) {
                (true, "open_positions") => "deferred_market_open_open_positions",
                (true, _) => "deferred_market_open_pending_orders",
                (false, "open_positions") => "deferred_open_positions",
                (false, _) => "deferred_pending_orders",
            }));
        }
        if market {
            // A reviewed forced activation may take the bounded restart window even
            // for a flat in-session book: no exposure is open, the original book is
            // preserved, and the same session resumes after the health-checked
            // restart. A halted or operator-stopped book still defers.
            if force && !book.halted && !book.stopped_safe {
                return Ok(Admission::Forced("market_open_flat"));
            }
            return Ok(Admission::Deferred(if book.flat_safe {
                "deferred_market_open"
            } else {
                "deferred_market_open_book_not_flat"
            }));
        }
        Ok(if book.flat_safe {
            Admission::Safe
        } else {
            Admission::Deferred("deferred_book_not_flat")
        })
    }
    fn smoke(&mut self, binary: &Path, tag: &str) -> Result<()> {
        let (code, body) = command(
            binary.to_str().ok_or("invalid_binary_path")?,
            &["--version"],
        )?;
        if code != 0
            || String::from_utf8_lossy(&body).trim()
                != format!("sadr-cli {}", tag.trim_start_matches('v'))
        {
            return Err("update_smoke_failed");
        }
        if command(
            binary.to_str().ok_or("invalid_binary_path")?,
            &["self-check", "--expected-version", tag],
        )?
        .0 != 0
        {
            return Err("update_self_check_failed");
        }
        let (code, _, errors) = command_in(
            binary.to_str().ok_or("invalid_binary_path")?,
            &[
                "preflight",
                "--config",
                self.paper.to_str().ok_or("invalid_config_path")?,
            ],
            None,
        )?;
        let state = self.cache.parent().ok_or("update_directory_unavailable")?;
        if code != 0 {
            // The bounded reason code, never raw stderr, is recorded for status.
            record_preflight_failure(state, "candidate", Some(tag), &errors);
            return Err("update_preflight_failed");
        }
        clear_preflight_failure(state);
        Ok(())
    }
    fn rollback_preflight(&mut self, binary: &Path) -> Result<()> {
        // Run the old binary's own preflight, not the candidate's. Output is
        // captured by the bounded helper; only a reason code is retained.
        let (code, _, errors) = command_in(
            binary.to_str().ok_or("invalid_binary_path")?,
            &[
                "preflight",
                "--config",
                self.paper.to_str().ok_or("invalid_config_path")?,
            ],
            None,
        )?;
        let state = self.cache.parent().ok_or("update_directory_unavailable")?;
        if code != 0 {
            record_preflight_failure(state, "rollback", None, &errors);
            return Err("update_rollback_config_incompatible");
        }
        clear_preflight_failure(state);
        Ok(())
    }
    fn restart(&mut self) -> Result<()> {
        let state = self
            .cache
            .parent()
            .ok_or("update_directory_unavailable")?
            .to_path_buf();
        let version = self.version(&self.current.join("sadr-rs"))?;
        Version::parse(version.trim())?;
        crate::alert::plan_restart(&state, version.trim(), now_utc().timestamp())
            .map_err(|_| "update_restart_marker_failed")?;
        if command(
            "systemctl",
            &["--user", "start", "ind-sadr-rust-paper.service"],
        )?
        .0 != 0
        {
            return Err("update_restart_failed");
        }
        Ok(())
    }
    fn healthy(&mut self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let tag = self.version(&self.current.join("sadr-rs"))?;
        Version::parse(tag.trim())?;
        let (code, version) = command(
            self.current
                .join("sadr-rs")
                .to_str()
                .ok_or("invalid_binary_path")?,
            &["--version"],
        )?;
        if code != 0
            || String::from_utf8_lossy(&version).trim()
                != format!("sadr-cli {}", tag.trim().trim_start_matches('v'))
        {
            return Err("update_smoke_failed");
        }
        let meta = self
            .token_file
            .symlink_metadata()
            .map_err(|_| "update_health_credential_unavailable")?;
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "update_owner_unavailable")?
            .uid();
        if !meta.is_file()
            || meta.mode() & 0o777 != 0o600
            || meta.nlink() != 1
            || meta.uid() != owner
        {
            return Err("update_health_credential_unavailable");
        }
        let token_bytes = read(&self.token_file, 256)?;
        let token = std::str::from_utf8(&token_bytes)
            .map_err(|_| "update_health_credential_unavailable")?
            .trim();
        if token.len() < 32 {
            return Err("update_health_credential_unavailable");
        }
        let start = Instant::now();
        poll_health(
            || {
                if !service_active()? {
                    return Err("update_service_unhealthy");
                }
                service_restarts()
            },
            || {
                let mut response = self
                    .agent
                    .get("http://127.0.0.1:8771/api/v1/health")
                    .header("Authorization", &format!("Bearer {token}"))
                    .call()
                    .map_err(|_| "update_dashboard_health_failed")?;
                let mut body = vec![];
                response
                    .body_mut()
                    .as_reader()
                    .take(65537)
                    .read_to_end(&mut body)
                    .map_err(|_| "update_dashboard_health_failed")?;
                paper_health(&body, tag.trim())
            },
            || start.elapsed(),
            thread::sleep,
        )
    }
}
