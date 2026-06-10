use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::PorkbunConfig;

const BASE_URL: &str = "https://porkbun.com/api/json/v3";

pub struct PorkbunDns {
    api_key: String,
    secret_api_key: String,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct AuthBody<'a> {
    apikey: &'a str,
    secretapikey: &'a str,
}

#[derive(Serialize)]
struct CreateRecordRequest<'a> {
    apikey: &'a str,
    secretapikey: &'a str,
    name: &'a str,
    r#type: &'a str,
    content: &'a str,
    ttl: &'a str,
}

#[derive(Deserialize)]
struct CreateRecordResponse {
    id: u64,
}

#[derive(Deserialize)]
struct DomainsResponse {
    domains: Vec<PorkbunDomain>,
}

#[derive(Deserialize)]
struct PorkbunDomain {
    domain: String,
}

impl PorkbunDns {
    pub fn new(config: PorkbunConfig) -> Self {
        Self {
            api_key: config.api_key,
            secret_api_key: config.secret_api_key,
            client: reqwest::Client::new(),
        }
    }

    async fn find_domain(&self, name: &str) -> Result<(String, String)> {
        let response: DomainsResponse = self
            .client
            .post(format!("{BASE_URL}/domain/listAll"))
            .json(&AuthBody {
                apikey: &self.api_key,
                secretapikey: &self.secret_api_key,
            })
            .send()
            .await
            .context("Failed to list Porkbun domains")?
            .json()
            .await
            .context("Failed to parse Porkbun domains response")?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if response.domains.iter().any(|d| d.domain == candidate) {
                return Ok((candidate, relative));
            }
        }

        bail!("No Porkbun domain found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for PorkbunDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (domain, relative_name) = self.find_domain(name).await?;

        let response: CreateRecordResponse = self
            .client
            .post(format!("{BASE_URL}/dns/createRecord/{domain}"))
            .json(&CreateRecordRequest {
                apikey: &self.api_key,
                secretapikey: &self.secret_api_key,
                name: &relative_name,
                r#type: "TXT",
                content: value,
                ttl: "60",
            })
            .send()
            .await
            .context("Failed to create TXT record in Porkbun")?
            .json()
            .await
            .context("Failed to parse Porkbun create TXT record response")?;

        Ok(format!("{domain}|{}", response.id))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (domain, id) = record_id
            .split_once('|')
            .context("Invalid Porkbun record_id format, expected '<domain>|<id>'")?;

        self.client
            .post(format!("{BASE_URL}/dns/deleteRecords/{domain}/{id}"))
            .json(&AuthBody {
                apikey: &self.api_key,
                secretapikey: &self.secret_api_key,
            })
            .send()
            .await
            .context("Failed to delete TXT record in Porkbun")?
            .error_for_status()
            .context("Porkbun DNS delete request failed")?;

        Ok(())
    }
}
