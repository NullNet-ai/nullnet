use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sdk_route53::{
    Client,
    config::Region,
    types::{Change, ChangeAction, ChangeBatch, ResourceRecord, ResourceRecordSet, RrType},
};

mod config;
pub use config::Route53Config;

pub struct Route53Dns {
    client: Client,
}

impl Route53Dns {
    pub fn new(config: Route53Config) -> Self {
        let credentials = Credentials::new(
            config.access_key_id,
            config.secret_access_key,
            None,
            None,
            "routix",
        );

        let sdk_config = aws_sdk_route53::Config::builder()
            .credentials_provider(credentials)
            .region(Region::new(config.region))
            .build();

        Self {
            client: Client::from_conf(sdk_config),
        }
    }

    async fn resolve_zone_id(&self, domain: &str) -> Result<String> {
        let response = self
            .client
            .list_hosted_zones()
            .send()
            .await
            .context("Failed to list Route 53 hosted zones")?;

        let labels: Vec<&str> = domain.split('.').collect();

        for i in 0..labels.len() - 1 {
            let candidate = format!("{}.", labels[i..].join("."));

            for zone in response.hosted_zones() {
                if zone.name() == candidate {
                    let id = zone.id().trim_start_matches("/hostedzone/").to_string();
                    return Ok(id);
                }
            }
        }

        bail!("No Route 53 hosted zone found for domain: {domain}")
    }
}

#[async_trait]
impl DnsProvider for Route53Dns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let domain = name.trim_start_matches("_acme-challenge.");
        let zone_id = self.resolve_zone_id(domain).await?;

        let change = build_change(ChangeAction::Create, name, value)?;
        let batch = ChangeBatch::builder().changes(change).build()?;

        self.client
            .change_resource_record_sets()
            .hosted_zone_id(&zone_id)
            .change_batch(batch)
            .send()
            .await
            .context("Failed to create TXT record in Route 53")?;

        Ok(format!("{zone_id}|{name}|{value}"))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let parts: Vec<&str> = record_id.splitn(3, '|').collect();
        if parts.len() != 3 {
            bail!("Invalid Route 53 record_id format, expected '<zone_id>|<name>|<value>'");
        }
        let (zone_id, name, value) = (parts[0], parts[1], parts[2]);

        let change = build_change(ChangeAction::Delete, name, value)?;
        let batch = ChangeBatch::builder().changes(change).build()?;

        self.client
            .change_resource_record_sets()
            .hosted_zone_id(zone_id)
            .change_batch(batch)
            .send()
            .await
            .context("Failed to delete TXT record in Route 53")?;

        Ok(())
    }
}

fn build_change(action: ChangeAction, name: &str, value: &str) -> Result<Change> {
    let record = ResourceRecord::builder()
        .value(format!("\"{value}\""))
        .build()?;

    let rrs = ResourceRecordSet::builder()
        .name(name)
        .r#type(RrType::Txt)
        .ttl(60)
        .resource_records(record)
        .build()?;

    let change = Change::builder()
        .action(action)
        .resource_record_set(rrs)
        .build()?;

    Ok(change)
}
