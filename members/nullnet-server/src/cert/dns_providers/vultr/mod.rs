use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::VultrConfig;

const BASE_URL: &str = "https://api.vultr.com/v2";

pub struct VultrDns {
    api_key: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct DomainsResponse {
    domains: Vec<VultrDomain>,
}

#[derive(Deserialize)]
struct VultrDomain {
    domain: String,
}

#[derive(Serialize)]
struct CreateRecordRequest<'a> {
    r#type: &'a str,
    name: &'a str,
    data: &'a str,
    ttl: u32,
}

#[derive(Deserialize)]
struct CreateRecordResponse {
    record: VultrRecord,
}

#[derive(Deserialize)]
struct VultrRecord {
    id: String,
}

impl VultrDns {
    pub fn new(config: VultrConfig) -> Self {
        Self {
            api_key: config.api_key,
            client: reqwest::Client::new(),
        }
    }

    async fn find_domain(&self, name: &str) -> Result<(String, String)> {
        let response: DomainsResponse = self
            .client
            .get(format!("{BASE_URL}/domains"))
            .bearer_auth(&self.api_key)
            .query(&[("per_page", "100")])
            .send()
            .await
            .context("Failed to list Vultr domains")?
            .json()
            .await
            .context("Failed to parse Vultr domains response")?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if response.domains.iter().any(|d| d.domain == candidate) {
                return Ok((candidate, relative));
            }
        }

        bail!("No Vultr domain found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for VultrDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (domain, relative_name) = self.find_domain(name).await?;

        let response: CreateRecordResponse = self
            .client
            .post(format!("{BASE_URL}/domains/{domain}/records"))
            .bearer_auth(&self.api_key)
            .json(&CreateRecordRequest {
                r#type: "TXT",
                name: &relative_name,
                data: value,
                ttl: 60,
            })
            .send()
            .await
            .context("Failed to create TXT record in Vultr")?
            .json()
            .await
            .context("Failed to parse Vultr create TXT record response")?;

        Ok(format!("{domain}|{}", response.record.id))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (domain, id) = record_id
            .split_once('|')
            .context("Invalid Vultr record_id format, expected '<domain>|<id>'")?;

        self.client
            .delete(format!("{BASE_URL}/domains/{domain}/records/{id}"))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("Failed to delete TXT record in Vultr")?
            .error_for_status()
            .context("Vultr DNS delete request failed")?;

        Ok(())
    }
}
