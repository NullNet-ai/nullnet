use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::HetznerDnsConfig;

const BASE_URL: &str = "https://dns.hetzner.com/api/v1";

pub struct HetznerDns {
    api_token: String,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct CreateRecordRequest<'a> {
    zone_id: &'a str,
    r#type: &'a str,
    name: &'a str,
    value: &'a str,
    ttl: u32,
}

#[derive(Deserialize)]
struct ZoneListResponse {
    zones: Vec<Zone>,
}

#[derive(Deserialize)]
struct Zone {
    id: String,
}

#[derive(Deserialize)]
struct RecordResponse {
    record: Record,
}

#[derive(Deserialize)]
struct Record {
    id: String,
}

impl HetznerDns {
    pub fn new(config: HetznerDnsConfig) -> Self {
        Self {
            api_token: config.api_token,
            client: reqwest::Client::new(),
        }
    }

    async fn find_zone(&self, name: &str) -> Result<(String, String)> {
        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let zone_name = labels[i..].join(".");
            let relative_name = labels[..i].join(".");

            let response: ZoneListResponse = self
                .client
                .get(format!("{BASE_URL}/zones"))
                .header("Auth-API-Token", &self.api_token)
                .query(&[("name", &zone_name)])
                .send()
                .await
                .with_context(|| format!("Failed to query Hetzner zones for {zone_name}"))?
                .json()
                .await
                .context("Failed to parse Hetzner zone list response")?;

            if let Some(zone) = response.zones.into_iter().next() {
                return Ok((zone.id, relative_name));
            }
        }

        bail!("No Hetzner DNS zone found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for HetznerDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (zone_id, relative_name) = self.find_zone(name).await?;

        let response: RecordResponse = self
            .client
            .post(format!("{BASE_URL}/records"))
            .header("Auth-API-Token", &self.api_token)
            .json(&CreateRecordRequest {
                zone_id: &zone_id,
                r#type: "TXT",
                name: &relative_name,
                value: &format!("\"{value}\""),
                ttl: 60,
            })
            .send()
            .await
            .context("Failed to create TXT record in Hetzner DNS")?
            .json()
            .await
            .context("Failed to parse Hetzner create TXT record response")?;

        Ok(response.record.id)
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        self.client
            .delete(format!("{BASE_URL}/records/{record_id}"))
            .header("Auth-API-Token", &self.api_token)
            .send()
            .await
            .context("Failed to delete TXT record in Hetzner DNS")?
            .error_for_status()
            .context("Hetzner DNS delete request failed")?;

        Ok(())
    }
}
