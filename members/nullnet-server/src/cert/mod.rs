//! ACME (Let's Encrypt) certificate issuance via DNS-01, ported from ../routix.
//! Credentials are supplied inline per request and never persisted.
mod dns_providers;
use async_trait::async_trait;
pub use dns_providers::*;
use serde::Deserialize;

mod authority;

pub use authority::*;

/// Credentials for a DNS provider, supplied directly in the certificate request payload.
/// The `"provider"` field acts as the discriminant.
///
/// Example JSON:
/// ```json
/// { "provider": "cloudflare", "api_token": "..." }
/// { "provider": "azure", "tenant_id": "...", "client_id": "...", "client_secret": "...", "subscription_id": "...", "resource_group": "..." }
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum DnsProviderCredentials {
    Azure {
        tenant_id: String,
        client_id: String,
        client_secret: String,
        subscription_id: String,
        resource_group: String,
    },
    Cloudflare {
        api_token: String,
    },
    #[serde(rename = "digitalocean")]
    DigitalOcean {
        api_token: String,
    },
    #[serde(rename = "godaddy")]
    GoDaddy {
        api_key: String,
        api_secret: String,
    },
    Google {
        project_id: String,
        client_email: String,
        private_key: String,
    },
    Hetzner {
        api_token: String,
    },
    Namecheap {
        api_user: String,
        api_key: String,
        client_ip: String,
    },
    Ovh {
        app_key: String,
        app_secret: String,
        consumer_key: String,
        /// Defaults to `https://eu.api.ovh.com/1.0` when omitted.
        endpoint: Option<String>,
    },
    Porkbun {
        api_key: String,
        secret_api_key: String,
    },
    #[serde(rename = "route53")]
    Route53 {
        access_key_id: String,
        secret_access_key: String,
        /// Defaults to `us-east-1` when omitted.
        region: Option<String>,
    },
    Vultr {
        api_key: String,
    },
}

impl DnsProviderCredentials {
    /// Returns the stable lowercase provider name used for display.
    pub fn provider_name(&self) -> &'static str {
        match self {
            DnsProviderCredentials::Azure { .. } => "azure",
            DnsProviderCredentials::Cloudflare { .. } => "cloudflare",
            DnsProviderCredentials::DigitalOcean { .. } => "digitalocean",
            DnsProviderCredentials::GoDaddy { .. } => "godaddy",
            DnsProviderCredentials::Google { .. } => "google",
            DnsProviderCredentials::Hetzner { .. } => "hetzner",
            DnsProviderCredentials::Namecheap { .. } => "namecheap",
            DnsProviderCredentials::Ovh { .. } => "ovh",
            DnsProviderCredentials::Porkbun { .. } => "porkbun",
            DnsProviderCredentials::Route53 { .. } => "route53",
            DnsProviderCredentials::Vultr { .. } => "vultr",
        }
    }
}

#[async_trait]
pub trait DnsProvider: Send + Sync {
    async fn create_txt_record(
        &self,
        name: &str,  // e.g. "_acme-challenge.example.com"
        value: &str, // the ACME key authorization digest
    ) -> anyhow::Result<String>;

    async fn delete_txt_record(&self, record_id: &str) -> anyhow::Result<()>;
}

pub fn create_dns_provider(
    credentials: DnsProviderCredentials,
) -> anyhow::Result<Box<dyn DnsProvider>> {
    let provider: Box<dyn DnsProvider> = match credentials {
        DnsProviderCredentials::Azure {
            tenant_id,
            client_id,
            client_secret,
            subscription_id,
            resource_group,
        } => Box::new(AzureDns::new(AzureConfig {
            tenant_id,
            client_id,
            client_secret,
            subscription_id,
            resource_group,
        })),
        DnsProviderCredentials::Cloudflare { api_token } => {
            Box::new(CloudflareDns::new(CloudflareDnsConfig { api_token }))
        }
        DnsProviderCredentials::DigitalOcean { api_token } => {
            Box::new(DigitalOceanDns::new(DigitalOceanConfig { api_token }))
        }
        DnsProviderCredentials::GoDaddy {
            api_key,
            api_secret,
        } => Box::new(GoDaddyDns::new(GoDaddyConfig {
            api_key,
            api_secret,
        })),
        DnsProviderCredentials::Google {
            project_id,
            client_email,
            private_key,
        } => Box::new(GoogleDns::new(GoogleDnsConfig {
            project_id,
            client_email,
            private_key,
        })),
        DnsProviderCredentials::Hetzner { api_token } => {
            Box::new(HetznerDns::new(HetznerDnsConfig { api_token }))
        }
        DnsProviderCredentials::Namecheap {
            api_user,
            api_key,
            client_ip,
        } => Box::new(NamecheapDns::new(NamecheapConfig {
            api_user,
            api_key,
            client_ip,
        })),
        DnsProviderCredentials::Ovh {
            app_key,
            app_secret,
            consumer_key,
            endpoint,
        } => Box::new(OvhDns::new(OvhConfig {
            app_key,
            app_secret,
            consumer_key,
            endpoint: endpoint.unwrap_or_else(|| "https://eu.api.ovh.com/1.0".to_string()),
        })),
        DnsProviderCredentials::Porkbun {
            api_key,
            secret_api_key,
        } => Box::new(PorkbunDns::new(PorkbunConfig {
            api_key,
            secret_api_key,
        })),
        DnsProviderCredentials::Route53 {
            access_key_id,
            secret_access_key,
            region,
        } => Box::new(Route53Dns::new(Route53Config {
            access_key_id,
            secret_access_key,
            region: region.unwrap_or_else(|| "us-east-1".to_string()),
        })),
        DnsProviderCredentials::Vultr { api_key } => {
            Box::new(VultrDns::new(VultrConfig { api_key }))
        }
    };

    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> DnsProviderCredentials {
        serde_json::from_str(json).expect("valid credentials")
    }

    #[test]
    fn cloudflare_round_trip() {
        let c = parse(r#"{"provider":"cloudflare","api_token":"tok"}"#);
        assert_eq!(c.provider_name(), "cloudflare");
        assert!(
            matches!(&c, DnsProviderCredentials::Cloudflare { api_token } if api_token == "tok")
        );
        assert!(create_dns_provider(c).is_ok());
    }

    #[test]
    fn renamed_provider_tags_match() {
        // variants with explicit #[serde(rename)] must use the lowercase id
        assert_eq!(
            parse(r#"{"provider":"route53","access_key_id":"a","secret_access_key":"s"}"#)
                .provider_name(),
            "route53"
        );
        assert_eq!(
            parse(r#"{"provider":"digitalocean","api_token":"t"}"#).provider_name(),
            "digitalocean"
        );
        assert_eq!(
            parse(r#"{"provider":"godaddy","api_key":"k","api_secret":"s"}"#).provider_name(),
            "godaddy"
        );
    }

    #[test]
    fn optional_fields_may_be_omitted() {
        // route53 region and ovh endpoint are optional
        assert!(matches!(
            parse(r#"{"provider":"route53","access_key_id":"a","secret_access_key":"s"}"#),
            DnsProviderCredentials::Route53 { region: None, .. }
        ));
        assert!(matches!(
            parse(r#"{"provider":"ovh","app_key":"a","app_secret":"s","consumer_key":"c"}"#),
            DnsProviderCredentials::Ovh { endpoint: None, .. }
        ));
    }

    #[test]
    fn unknown_provider_is_rejected() {
        assert!(serde_json::from_str::<DnsProviderCredentials>(r#"{"provider":"nope"}"#).is_err());
    }
}
