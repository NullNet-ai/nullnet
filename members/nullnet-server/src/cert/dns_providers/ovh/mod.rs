use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Write;

mod config;
pub use config::OvhConfig;

pub struct OvhDns {
    endpoint: String,
    app_key: String,
    app_secret: String,
    consumer_key: String,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct CreateRecordRequest<'a> {
    #[serde(rename = "fieldType")]
    field_type: &'a str,
    #[serde(rename = "subDomain")]
    sub_domain: &'a str,
    target: &'a str,
    ttl: u32,
}

#[derive(Deserialize)]
struct OvhRecord {
    id: u64,
}

impl OvhDns {
    pub fn new(config: OvhConfig) -> Self {
        Self {
            endpoint: config.endpoint,
            app_key: config.app_key,
            app_secret: config.app_secret,
            consumer_key: config.consumer_key,
            client: reqwest::Client::new(),
        }
    }

    fn sign(&self, method: &str, url: &str, body: &str, timestamp: i64) -> Result<String> {
        use openssl::hash::MessageDigest;
        use openssl::pkey::PKey;
        use openssl::sign::Signer;

        let preimage = format!(
            "{}+{}+{}+{}+{}+{}",
            self.app_secret, self.consumer_key, method, url, body, timestamp
        );

        let key =
            PKey::hmac(self.app_secret.as_bytes()).context("Failed to create OVH HMAC key")?;
        let mut signer =
            Signer::new(MessageDigest::sha1(), &key).context("Failed to create OVH signer")?;
        signer
            .update(preimage.as_bytes())
            .context("Failed to update OVH signer")?;
        let sig = signer
            .sign_to_vec()
            .context("Failed to compute OVH HMAC-SHA1")?;

        let hex: String = sig.iter().try_fold::<String, _, anyhow::Result<String>>(
            String::new(),
            |mut acc, b| {
                write!(acc, "{b:02x}").map_err(|e| anyhow::anyhow!(e))?;
                Ok(acc)
            },
        )?;

        Ok(format!("$1${hex}"))
    }

    async fn get_timestamp(&self) -> Result<i64> {
        let ts: i64 = self
            .client
            .get(format!("{}/auth/time", self.endpoint))
            .send()
            .await
            .context("Failed to get OVH server time")?
            .json()
            .await
            .context("Failed to parse OVH server time")?;
        Ok(ts)
    }

    async fn signed_get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.endpoint, path);
        let ts = self.get_timestamp().await?;
        let sig = self.sign("GET", &url, "", ts)?;

        self.client
            .get(&url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", ts.to_string())
            .header("X-Ovh-Signature", sig)
            .send()
            .await
            .with_context(|| format!("OVH GET {path} failed"))?
            .json()
            .await
            .with_context(|| format!("Failed to parse OVH GET {path} response"))
    }

    async fn signed_post<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.endpoint, path);
        let body_str =
            serde_json::to_string(body).context("Failed to serialize OVH request body")?;
        let ts = self.get_timestamp().await?;
        let sig = self.sign("POST", &url, &body_str, ts)?;

        self.client
            .post(&url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", ts.to_string())
            .header("X-Ovh-Signature", sig)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .with_context(|| format!("OVH POST {path} failed"))?
            .json()
            .await
            .with_context(|| format!("Failed to parse OVH POST {path} response"))
    }

    async fn signed_delete(&self, path: &str) -> Result<()> {
        let url = format!("{}{}", self.endpoint, path);
        let ts = self.get_timestamp().await?;
        let sig = self.sign("DELETE", &url, "", ts)?;

        self.client
            .delete(&url)
            .header("X-Ovh-Application", &self.app_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", ts.to_string())
            .header("X-Ovh-Signature", sig)
            .send()
            .await
            .with_context(|| format!("OVH DELETE {path} failed"))?
            .error_for_status()
            .with_context(|| format!("OVH DELETE {path} returned error status"))?;

        Ok(())
    }

    async fn find_zone(&self, name: &str) -> Result<(String, String)> {
        let zones: Vec<String> = self.signed_get("/domain/zone").await?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if zones.contains(&candidate) {
                return Ok((candidate, relative));
            }
        }

        bail!("No OVH DNS zone found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for OvhDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (zone, sub_domain) = self.find_zone(name).await?;

        let record: OvhRecord = self
            .signed_post(
                &format!("/domain/zone/{zone}/record"),
                &CreateRecordRequest {
                    field_type: "TXT",
                    sub_domain: &sub_domain,
                    target: &format!("\"{value}\""),
                    ttl: 60,
                },
            )
            .await?;

        let _: serde_json::Value = self
            .signed_post(
                &format!("/domain/zone/{zone}/refresh"),
                &serde_json::json!({}),
            )
            .await
            .unwrap_or(serde_json::Value::Null);

        Ok(format!("{zone}|{}", record.id))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let (zone, id) = record_id
            .split_once('|')
            .context("Invalid OVH record_id format, expected '<zone>|<id>'")?;

        self.signed_delete(&format!("/domain/zone/{zone}/record/{id}"))
            .await?;

        let _: serde_json::Value = self
            .signed_post(
                &format!("/domain/zone/{zone}/refresh"),
                &serde_json::json!({}),
            )
            .await
            .unwrap_or(serde_json::Value::Null);

        Ok(())
    }
}
