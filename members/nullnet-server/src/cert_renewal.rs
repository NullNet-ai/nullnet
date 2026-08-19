//! Background ACME auto-renewal, ported from ../routix. Periodically scans
//! `./certs` for certs nearing expiry that have stored DNS-provider credentials
//! (ACME-issued ones), re-issues them, and writes them back — the certs watcher
//! then pushes the renewed cert to the proxies. Manual uploads have no stored
//! credentials and are skipped.
use crate::cert::{self, CertificateAuthority, DnsProviderCredentials};
use crate::events::{Event, EventStore};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::{self, MissedTickBehavior};

const SECS_PER_DAY: i64 = 86_400;

pub(crate) struct RenewalConfig {
    /// How often to scan for expiring certs.
    check_interval_secs: u64,
    /// Renew certs expiring within this many days.
    renew_before_days: i64,
    /// Seconds to wait after creating the DNS TXT record before notifying ACME.
    dns_propagation_secs: u64,
}

impl RenewalConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            check_interval_secs: env_parsed("CERT_RENEWAL_CHECK_INTERVAL_SECS", 43_200), // 12h
            renew_before_days: env_parsed("CERT_RENEWAL_DAYS_BEFORE", 30),
            dns_propagation_secs: env_parsed("CERT_RENEWAL_DNS_PROPAGATION_SECS", 30),
        }
    }
}

fn env_parsed<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Spawn the renewal loop. The first pass runs immediately (catches already-due
/// certs at startup), then every `check_interval_secs`.
pub(crate) fn start(events: EventStore, config: RenewalConfig) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(config.check_interval_secs));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            run_pass(&events, &config).await;
        }
    });
}

async fn run_pass(events: &EventStore, config: &RenewalConfig) {
    let now = now_secs();
    let threshold = now + config.renew_before_days * SECS_PER_DAY;

    for domain in crate::certs::cert_domains().await {
        // only ACME-issued certs carry stored credentials; skip manual uploads
        let Some(creds_json) = crate::certs::load_dns_credentials(&domain).await else {
            continue;
        };
        let Some(expiry) = crate::certs::read_expiry(&domain).await else {
            // An ACME cert whose expiry can't be read is never renewed, so this
            // silently opts it out of the guarantee the loop exists to provide.
            eprintln!("Cert renewal: cannot read expiry of '{domain}'; skipping");
            events
                .emit(Event::certificate_renewal_failed(
                    domain.clone(),
                    "certificate expiry could not be read".to_string(),
                ))
                .await;
            continue;
        };
        if expiry > threshold {
            continue;
        }

        let days_left = (expiry - now) / SECS_PER_DAY;
        println!("Cert renewal: '{domain}' expires in {days_left}d, renewing");

        match renew_one(&domain, &creds_json, config).await {
            Ok(()) => {
                events
                    .emit(Event::certificate_renewed(domain.clone()))
                    .await;
                println!("Cert renewal: '{domain}' renewed successfully");
            }
            Err(e) => {
                eprintln!("Cert renewal: failed to renew '{domain}': {e:#}");
                events
                    .emit(Event::certificate_renewal_failed(
                        domain.clone(),
                        format!("{e:#}"),
                    ))
                    .await;
            }
        }
    }
}

async fn renew_one(domain: &str, creds_json: &str, config: &RenewalConfig) -> anyhow::Result<()> {
    let credentials: DnsProviderCredentials = serde_json::from_str(creds_json)?;
    let provider = cert::create_dns_provider(credentials)?;

    // staging CA in debug builds, production in release (matches request_handler)
    let ca = if cfg!(debug_assertions) {
        CertificateAuthority::staging()
    } else {
        CertificateAuthority::production()
    };

    let (fullchain_pem, key_pem) = ca
        .request_certificate(domain, provider.as_ref(), config.dns_propagation_secs)
        .await?;

    crate::certs::write_cert(domain, &fullchain_pem, &key_pem)
        .await
        .map_err(|e| anyhow::anyhow!("failed to write renewed certificate: {e:?}"))?;
    Ok(())
}
