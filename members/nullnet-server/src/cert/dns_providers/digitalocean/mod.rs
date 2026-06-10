use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::DigitalOceanConfig;

const BASE_URL: &str = "https://api.digitalocean.com/v2";

pub struct DigitalOceanDns {
    api_token: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct DomainsResponse {
    domains: Vec<Domain>,
}

#[derive(Deserialize)]
struct Domain {
    name: String,
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
    domain_record: DomainRecord,
}

#[derive(Deserialize)]
struct DomainRecord {
    id: u64,
}

impl DigitalOceanDns {
    pub fn new(config: DigitalOceanConfig) -> Self {
        Self {
            api_token: config.api_token,
            client: reqwest::Client::new(),
        }
    }

    async fn find_domain(&self, name: &str) -> Result<(String, String)> {
        let response: DomainsResponse = self
            .client
            .get(format!("{BASE_URL}/domains"))
            .bearer_auth(&self.api_token)
            .query(&[("per_page", "200")])
            .send()
            .await
            .context("Failed to list DigitalOcean domains")?
            .json()
            .await
            .context("Failed to parse DigitalOcean domains response")?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if response.domains.iter().any(|d| d.name == candidate) {
                return Ok((candidate, relative));
            }
        }

        bail!("No DigitalOcean domain found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for DigitalOceanDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (domain, relative_name) = self.find_domain(name).await?;

        let response: CreateRecordResponse = self
            .client
            .post(format!("{BASE_URL}/domains/{domain}/records"))
            .bearer_auth(&self.api_token)
            .json(&CreateRecordRequest {
                r#type: "TXT",
                name: &relative_name,
                data: value,
                ttl: 60,
            })
            .send()
            .await
            .context("Failed to create TXT record in DigitalOcean")?
            .json()
            .await
            .context("Failed to parse DigitalOcean create TXT record response")?;

        Ok(format!("{domain}|{}", response.domain_record.id))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (domain, id) = record_id
            .split_once('|')
            .context("Invalid DigitalOcean record_id format, expected '<domain>|<id>'")?;

        self.client
            .delete(format!("{BASE_URL}/domains/{domain}/records/{id}"))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .context("Failed to delete TXT record in DigitalOcean")?
            .error_for_status()
            .context("DigitalOcean DNS delete request failed")?;

        Ok(())
    }
}
