use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

mod config;
pub use config::GoogleDnsConfig;

const DNS_API: &str = "https://dns.googleapis.com/dns/v1";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub struct GoogleDns {
    project_id: String,
    client_email: String,
    private_key_pem: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct ManagedZoneListResponse {
    #[serde(rename = "managedZones")]
    managed_zones: Vec<ManagedZone>,
}

#[derive(Deserialize)]
struct ManagedZone {
    name: String, // e.g. "example-com"
    #[serde(rename = "dnsName")]
    dns_name: String, // e.g. "example.com." (trailing dot)
}

#[derive(Serialize)]
struct ChangeRequest<'a> {
    additions: Option<Vec<ResourceRecordSet<'a>>>,
    deletions: Option<Vec<ResourceRecordSet<'a>>>,
}

#[derive(Serialize)]
struct ResourceRecordSet<'a> {
    name: &'a str,
    r#type: &'a str,
    ttl: u32,
    rrdatas: Vec<String>,
}

impl GoogleDns {
    pub fn new(config: GoogleDnsConfig) -> Self {
        Self {
            project_id: config.project_id,
            client_email: config.client_email,
            private_key_pem: config.private_key,
            client: reqwest::Client::new(),
        }
    }

    async fn get_access_token(&self) -> Result<String> {
        let jwt = self.create_jwt()?;

        let response: TokenResponse = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", jwt.as_str()),
            ])
            .send()
            .await
            .context("Failed to exchange JWT for Google access token")?
            .json()
            .await
            .context("Failed to parse Google access token response")?;

        Ok(response.access_token)
    }

    fn create_jwt(&self) -> Result<String> {
        use openssl::hash::MessageDigest;
        use openssl::pkey::PKey;
        use openssl::rsa::Rsa;
        use openssl::sign::Signer;

        let now = chrono::Utc::now().timestamp();

        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = serde_json::json!({
            "iss": self.client_email,
            "scope": "https://www.googleapis.com/auth/cloud-platform",
            "aud": TOKEN_URL,
            "iat": now,
            "exp": now + 3600,
        });
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let to_sign = format!("{header}.{payload}");

        let rsa = Rsa::private_key_from_pem(self.private_key_pem.as_bytes())
            .context("Failed to parse GCP RSA private key")?;
        let pkey = PKey::from_rsa(rsa).context("Failed to create PKey from RSA")?;

        let mut signer =
            Signer::new(MessageDigest::sha256(), &pkey).context("Failed to create JWT signer")?;
        signer
            .update(to_sign.as_bytes())
            .context("Failed to update JWT signer")?;
        let sig = signer.sign_to_vec().context("Failed to sign JWT")?;

        Ok(format!("{to_sign}.{}", URL_SAFE_NO_PAD.encode(&sig)))
    }

    async fn find_zone(&self, name: &str, token: &str) -> Result<(String, String)> {
        let response: ManagedZoneListResponse = self
            .client
            .get(format!(
                "{DNS_API}/projects/{}/managedZones",
                self.project_id
            ))
            .bearer_auth(token)
            .send()
            .await
            .context("Failed to list Google Cloud DNS managed zones")?
            .json()
            .await
            .context("Failed to parse Google Cloud DNS managed zones response")?;

        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate_dns = format!("{}.", labels[i..].join("."));

            if let Some(zone) = response
                .managed_zones
                .iter()
                .find(|z| z.dns_name == candidate_dns)
            {
                let fqdn = format!("{name}.");
                return Ok((zone.name.clone(), fqdn));
            }
        }

        bail!("No Google Cloud DNS managed zone found for: {name}")
    }
}

#[async_trait]
impl DnsProvider for GoogleDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let token = self.get_access_token().await?;
        let (zone, fqdn) = self.find_zone(name, &token).await?;

        let change = ChangeRequest {
            additions: Some(vec![ResourceRecordSet {
                name: &fqdn,
                r#type: "TXT",
                ttl: 60,
                rrdatas: vec![format!("\"{value}\"")],
            }]),
            deletions: None,
        };

        self.client
            .post(format!(
                "{DNS_API}/projects/{}/managedZones/{zone}/changes",
                self.project_id
            ))
            .bearer_auth(&token)
            .json(&change)
            .send()
            .await
            .context("Failed to create TXT record in Google Cloud DNS")?
            .error_for_status()
            .context("Google Cloud DNS create TXT record request failed")?;

        Ok(format!("{zone}|{fqdn}|{value}"))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let parts: Vec<&str> = record_id.splitn(3, '|').collect();
        if parts.len() != 3 {
            bail!("Invalid Google Cloud DNS record_id format, expected '<zone>|<fqdn>|<value>'");
        }
        let (zone, fqdn, value) = (parts[0], parts[1], parts[2]);

        let token = self.get_access_token().await?;

        let change = ChangeRequest {
            additions: None,
            deletions: Some(vec![ResourceRecordSet {
                name: fqdn,
                r#type: "TXT",
                ttl: 60,
                rrdatas: vec![format!("\"{value}\"")],
            }]),
        };

        self.client
            .post(format!(
                "{DNS_API}/projects/{}/managedZones/{zone}/changes",
                self.project_id
            ))
            .bearer_auth(&token)
            .json(&change)
            .send()
            .await
            .context("Failed to delete TXT record in Google Cloud DNS")?
            .error_for_status()
            .context("Google Cloud DNS delete TXT record request failed")?;

        Ok(())
    }
}
