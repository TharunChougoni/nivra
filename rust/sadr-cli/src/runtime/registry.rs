//! One loopback dashboard, optionally reading an explicitly selected registry.
use super::*;
fn configured_public_host(config: Option<&Config>) -> Option<DashboardPublicHost> {
    config.and_then(|c| c.dashboard_public_host.clone())
}
pub fn entry(
    registry: Option<&Path>,
    token_file: &Path,
    history: Option<&Path>,
    model_config: Option<&Path>,
) -> ExitCode {
    let result = (|| -> Result<()> {
        let token = private(token_file)?;
        if token.trim().len() < 32 {
            return Err("dashboard_token_too_short".into());
        }
        let config: Option<Config> = model_config
            .map(|path| -> Result<Config> {
                let config: Config = serde_json::from_slice(
                    &crate::read_bounded(path, 16384).map_err(|_| "model_feed_config_invalid")?,
                )?;
                config.validate()?;
                Ok(config)
            })
            .transpose()?;
        let storage = config
            .as_ref()
            .map(|c| c.storage.clone())
            .unwrap_or_default();
        let dashboard = config
            .as_ref()
            .map(|c| c.dashboard.clone())
            .unwrap_or_default();
        let mut db = registry
            .map(|p| {
                Connection::open_with_flags(
                    p,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
            })
            .transpose()?;
        if let Some(db) = db.as_mut() {
            db.busy_timeout(Duration::from_millis(storage.registry_busy_timeout_ms))?;
            db.execute_batch("PRAGMA query_only=ON;")?;
        }
        fn projection(db: &mut Option<Connection>, history: Option<&Path>) -> serde_json::Value {
            let mut value = match db {
                Some(db) => match sadr_store::controller::sessions(db, 100) {
                    Ok(page) => {
                        serde_json::json!({"mode":"registry","paper_only":true,"live_enabled":false,"sessions":page.sessions,"truncated":page.truncated,"observed_at_ms":millis(),"stale":true})
                    }
                    Err(_) => {
                        serde_json::json!({"mode":"registry","unavailable":true,"stale":true})
                    }
                },
                None => {
                    serde_json::json!({"mode":"research","paper_only":true,"live_enabled":false,"status":"research_only","observed_at_ms":millis()})
                }
            };
            if let Some(path) = history {
                value["history"] = super::history::project(path)
                    .unwrap_or_else(|_| serde_json::json!({"unavailable":true}));
            }
            value
        }
        let view = Arc::new(Mutex::new(projection(&mut db, history)));
        let settings = config
            .as_ref()
            .map(|c| c.dashboard.clone())
            .unwrap_or_default();
        let public_host = configured_public_host(config.as_ref());

        let feed = if let Some(config) = config {
            let config = rollover::current(&config)?;
            rollover::bound_state(&config)?;
            Some((
                config.directory.join("model/live-stream.json"),
                book_binding(&config)?,
            ))
        } else {
            None
        };
        web::start_with_feed(
            token.trim().to_owned(),
            public_host,
            view.clone(),
            feed,
            settings,
        )?;
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?
            .block_on(async {
                loop {
                    tokio::time::sleep(Duration::from_millis(dashboard.refresh_interval_ms)).await;
                    let next = projection(&mut db, history);
                    if let Ok(mut value) = view.lock() {
                        *value = next.clone();
                    }
                    web::update_prepared_summary(&next);
                }
            })
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("registry_dashboard_unavailable");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loaded_config_supplies_only_its_exact_public_host() {
        let mut c = super::super::tests::config(Path::new("fixture"));
        assert!(configured_public_host(None).is_none());
        assert!(configured_public_host(Some(&c)).is_none());
        c.dashboard_public_host = Some("dashboard.example.test".to_owned().try_into().unwrap());
        assert_eq!(
            configured_public_host(Some(&c)).unwrap().as_str(),
            "dashboard.example.test"
        );
    }
}
