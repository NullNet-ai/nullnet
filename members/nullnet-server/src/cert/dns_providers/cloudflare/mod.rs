use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::CloudflareDnsConfig;

pub struct CloudflareDns {
    api_token: String,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct CreateRecordRequest<'a> {
    r#type: &'a str,
    name: &'a str,
    content: &'a str,
    ttl: u32,
}

#[derive(Deserialize)]
struct CloudflareResponse<T> {
    success: bool,
    result: Option<T>,
    errors: Vec<CloudflareApiError>,
}

#[derive(Deserialize)]
struct CloudflareApiError {
    message: String,
}

impl CloudflareDns {
    pub fn new(config: CloudflareDnsConfig) -> Self {
        Self {
            api_token: config.api_token,
            client: reqwest::Client::new(),
        }
    }

    async fn resolve_zone_id(&self, domain: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct ZoneResult {
            id: String,
        }

        let labels: Vec<&str> = domain.split('.').collect();

        for i in 0..labels.len() - 1 {
            let candidate = labels[i..].join(".");

            let response: CloudflareResponse<Vec<ZoneResult>> = self
                .client
                .get("https://api.cloudflare.com/client/v4/zones")
                .bearer_auth(&self.api_token)
                .query(&[("name", candidate.as_str()), ("status", "active")])
                .send()
                .await
                .with_context(|| format!("Failed to query zones for {candidate}"))?
                .json()
                .await
                .context("Failed to parse zone list response")?;

            if !response.success {
                bail!("Cloudflare API error: {}", response.error_messages());
            }

            if let Some(zone) = response.result.and_then(|z| z.into_iter().next()) {
                return Ok(zone.id);
            }
        }

        bail!("No active Cloudflare zone found for domain: {domain}")
    }

    fn records_url(zone_id: &str) -> String {
        format!("https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records")
    }
}

#[async_trait]
impl DnsProvider for CloudflareDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct DnsRecord {
            id: String,
        }

        let domain = name.trim_start_matches("_acme-challenge.");
        let zone_id = self.resolve_zone_id(domain).await?;

        let response: CloudflareResponse<DnsRecord> = self
            .client
            .post(Self::records_url(&zone_id))
            .bearer_auth(&self.api_token)
            .json(&CreateRecordRequest {
                r#type: "TXT",
                name,
                content: value,
                ttl: 60,
            })
            .send()
            .await
            .context("Failed to send create TXT record request")?
            .json()
            .await
            .context("Failed to parse create TXT record response")?;

        if !response.success {
            bail!("Cloudflare API error: {}", response.error_messages());
        }

        let record_id = response
            .result
            .ok_or_else(|| anyhow::anyhow!("Empty result in create TXT record response"))?
            .id;

        Ok(format!("{zone_id}/{record_id}"))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (zone_id, id) = record_id
            .split_once('/')
            .context("Invalid record_id format, expected '<zone_id>/<record_id>'")?;

        let response: CloudflareResponse<serde_json::Value> = self
            .client
            .delete(format!("{}/{}", Self::records_url(zone_id), id))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .context("Failed to send delete TXT record request")?
            .json()
            .await
            .context("Failed to parse delete TXT record response")?;

        if !response.success {
            bail!("Cloudflare API error: {}", response.error_messages());
        }

        Ok(())
    }
}

impl<T> CloudflareResponse<T> {
    fn error_messages(&self) -> String {
        self.errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}
