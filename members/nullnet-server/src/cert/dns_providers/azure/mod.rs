use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

mod config;
pub use config::AzureConfig;

const API_VERSION: &str = "2018-05-01";
const MANAGEMENT_URL: &str = "https://management.azure.com";

pub struct AzureDns {
    tenant_id: String,
    client_id: String,
    client_secret: String,
    subscription_id: String,
    resource_group: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct ZoneListResponse {
    value: Vec<AzureZone>,
}

#[derive(Deserialize)]
struct AzureZone {
    name: String,
}

#[derive(Serialize)]
struct RecordSetBody {
    properties: RecordSetProperties,
}

#[derive(Serialize)]
struct RecordSetProperties {
    #[serde(rename = "TTL")]
    ttl: u32,
    #[serde(rename = "TXTRecords")]
    txt_records: Vec<TxtRecordValue>,
}

#[derive(Serialize)]
struct TxtRecordValue {
    value: Vec<String>,
}

impl AzureDns {
    pub fn new(config: AzureConfig) -> Self {
        Self {
            tenant_id: config.tenant_id,
            client_id: config.client_id,
            client_secret: config.client_secret,
            subscription_id: config.subscription_id,
            resource_group: config.resource_group,
            client: reqwest::Client::new(),
        }
    }

    async fn get_token(&self) -> Result<String> {
        let token_url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant_id
        );

        let response: TokenResponse = self
            .client
            .post(&token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("grant_type", "client_credentials"),
                ("scope", "https://management.azure.com/.default"),
            ])
            .send()
            .await
            .context("Failed to request Azure OAuth2 token")?
            .json()
            .await
            .context("Failed to parse Azure OAuth2 token response")?;

        Ok(response.access_token)
    }

    fn zone_base_url(&self) -> String {
        format!(
            "{MANAGEMENT_URL}/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/dnsZones",
            self.subscription_id, self.resource_group
        )
    }

    async fn find_zone(&self, name: &str, token: &str) -> Result<(String, String)> {
        let response: ZoneListResponse = self
            .client
            .get(self.zone_base_url())
            .bearer_auth(token)
            .query(&[("api-version", API_VERSION)])
            .send()
            .await
            .context("Failed to list Azure DNS zones")?
            .json()
            .await
            .context("Failed to parse Azure DNS zones response")?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if response.value.iter().any(|z| z.name == candidate) {
                return Ok((candidate, relative));
            }
        }

        bail!("No Azure DNS zone found for: {name}")
    }

    fn record_url(&self, zone: &str, relative_name: &str) -> String {
        format!("{}/{zone}/TXT/{relative_name}", self.zone_base_url())
    }
}

#[async_trait]
impl DnsProvider for AzureDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let token = self.get_token().await?;
        let (zone, relative_name) = self.find_zone(name, &token).await?;

        let body = RecordSetBody {
            properties: RecordSetProperties {
                ttl: 60,
                txt_records: vec![TxtRecordValue {
                    value: vec![value.to_string()],
                }],
            },
        };

        self.client
            .put(self.record_url(&zone, &relative_name))
            .bearer_auth(&token)
            .query(&[("api-version", API_VERSION)])
            .json(&body)
            .send()
            .await
            .context("Failed to create TXT record in Azure DNS")?
            .error_for_status()
            .context("Azure DNS create TXT record request failed")?;

        Ok(format!("{zone}|{relative_name}"))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (zone, relative_name) = record_id
            .split_once('|')
            .context("Invalid Azure record_id format, expected '<zone>|<relative_name>'")?;

        let token = self.get_token().await?;

        self.client
            .delete(self.record_url(zone, relative_name))
            .bearer_auth(&token)
            .query(&[("api-version", API_VERSION)])
            .send()
            .await
            .context("Failed to delete TXT record in Azure DNS")?
            .error_for_status()
            .context("Azure DNS delete request failed")?;

        Ok(())
    }
}
