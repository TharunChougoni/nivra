//! Research-only loopback dashboard: Backtest lab and verification, no controller state.
use super::*;
fn configured_public_host(config: Option<&Config>) -> Option<DashboardPublicHost> {
    config.and_then(|c| c.dashboard_public_host.clone())
}
pub fn entry(token_file: &Path, config: Option<&Path>) -> ExitCode {
    let result = (|| -> Result<()> {
        let token = private(token_file)?;
        if token.trim().len() < 32 {
            return Err("dashboard_token_too_short".into());
        }
        let config: Option<Config> = config
            .map(|path| -> Result<Config> {
                let config: Config = serde_json::from_slice(
                    &crate::read_bounded(path, 16384).map_err(|_| "dashboard_config_invalid")?,
                )?;
                config.validate()?;
                Ok(config)
            })
            .transpose()?;
        let dashboard = config
            .as_ref()
            .map(|c| c.dashboard.clone())
            .unwrap_or_default();
        fn projection() -> serde_json::Value {
            serde_json::json!({"mode":"research","paper_only":true,"live_enabled":false,"status":"research_only","observed_at_ms":millis()})
        }
        let view = Arc::new(Mutex::new(projection()));
        let settings = config
            .as_ref()
            .map(|c| c.dashboard.clone())
            .unwrap_or_default();
        let public_host = configured_public_host(config.as_ref());

        let archive = config.as_ref().and_then(|c| c.market_archive.clone());

        web::start(
            web::Ctx::new(token.trim().to_owned(), public_host, view.clone(), settings)
                .with_archive(archive),
        )?;
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?
            .block_on(async {
                loop {
                    tokio::time::sleep(Duration::from_millis(dashboard.refresh_interval_ms)).await;
                    let next = projection();
                    if let Ok(mut value) = view.lock() {
                        *value = next.clone();
                    }
                    web::update_prepared_summary(&next, None);
                }
            })
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("research_dashboard_unavailable");
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
