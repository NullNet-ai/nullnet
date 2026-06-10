use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::GoDaddyConfig;

const BASE_URL: &str = "https://api.godaddy.com/v1";

pub struct GoDaddyDns {
    api_key: String,
    api_secret: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct GoDaddyDomain {
    domain: String,
}

#[derive(Serialize)]
struct TxtRecord<'a> {
    data: &'a str,
    ttl: u32,
}

impl GoDaddyDns {
    pub fn new(config: GoDaddyConfig) -> Self {
        Self {
            api_key: config.api_key,
            api_secret: config.api_secret,
            client: reqwest::Client::new(),
        }
    }

    fn auth_header(&self) -> String {
        format!("sso-key {}:{}", self.api_key, self.api_secret)
    }

    async fn find_domain(&self, name: &str) -> Result<(String, String)> {
        let domains: Vec<GoDaddyDomain> = self
            .client
            .get(format!("{BASE_URL}/domains"))
            .header("Authorization", self.auth_header())
            .query(&[("limit", "500"), ("statuses", "ACTIVE")])
            .send()
            .await
            .context("Failed to list GoDaddy domains")?
            .json()
            .await
            .context("Failed to parse GoDaddy domains response")?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if domains.iter().any(|d| d.domain == candidate) {
                return Ok((candidate, relative));
            }
        }

        bail!("No GoDaddy domain found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for GoDaddyDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (domain, relative_name) = self.find_domain(name).await?;

        self.client
            .put(format!(
                "{BASE_URL}/domains/{domain}/records/TXT/{relative_name}"
            ))
            .header("Authorization", self.auth_header())
            .json(&[TxtRecord {
                data: value,
                ttl: 600,
            }])
            .send()
            .await
            .context("Failed to create TXT record in GoDaddy")?
            .error_for_status()
            .context("GoDaddy create TXT record request failed")?;

        Ok(format!("{domain}|{relative_name}"))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (domain, relative_name) = record_id
            .split_once('|')
            .context("Invalid GoDaddy record_id format, expected '<domain>|<relative_name>'")?;

        self.client
            .delete(format!(
                "{BASE_URL}/domains/{domain}/records/TXT/{relative_name}"
            ))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("Failed to delete TXT record in GoDaddy")?
            .error_for_status()
            .context("GoDaddy DNS delete request failed")?;

        Ok(())
    }
}
