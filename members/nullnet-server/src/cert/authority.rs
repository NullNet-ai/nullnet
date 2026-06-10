use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, Order, OrderStatus, RetryPolicy,
};

/// Issues certificates from an ACME CA (Let's Encrypt) using the DNS-01 challenge.
pub struct CertificateAuthority {
    acme_url: String,
}

impl CertificateAuthority {
    pub fn new(acme_url: impl Into<String>) -> Self {
        Self {
            acme_url: acme_url.into(),
        }
    }

    pub fn staging() -> Self {
        Self::new(instant_acme::LetsEncrypt::Staging.url())
    }

    pub fn production() -> Self {
        Self::new(instant_acme::LetsEncrypt::Production.url())
    }

    /// Run the full DNS-01 flow for `domain` and return the issued
    /// `(fullchain_pem, private_key_pem)`. The TXT challenge records are cleaned
    /// up before returning, success or failure.
    pub async fn request_certificate(
        &self,
        domain: &str,
        dns_provider: &dyn DnsProvider,
        dns_propagation_secs: u64,
    ) -> Result<(String, String)> {
        let (account, _) = self.acme_account().await?;
        let mut order = self.acme_order(domain, &account).await?;

        let records = self
            .dns_challenge(&mut order, dns_provider, dns_propagation_secs)
            .await?;

        if records.is_empty() {
            bail!("No DNS challenges were completed — no TXT records created");
        }

        let result = self.finalize_order(&mut order).await;

        self.cleanup_records(&records, dns_provider).await;

        let (private_key_pem, cert_chain_pem) = result?;
        Ok((cert_chain_pem, private_key_pem))
    }

    async fn finalize_order(&self, order: &mut Order) -> Result<(String, String)> {
        let status = order.poll_ready(&RetryPolicy::default()).await?;
        if status != OrderStatus::Ready {
            bail!("Unexpected order status after polling: {status:?}");
        }

        let private_key_pem = order.finalize().await?;
        let cert_chain_pem = order.poll_certificate(&RetryPolicy::default()).await?;

        Ok((private_key_pem, cert_chain_pem))
    }

    async fn cleanup_records(&self, records: &[String], dns_provider: &dyn DnsProvider) {
        for record_id in records {
            if let Err(e) = dns_provider.delete_txt_record(record_id).await {
                eprintln!("Failed to delete DNS TXT record {record_id}: {e}");
            }
        }
    }

    async fn acme_account(&self) -> Result<(Account, AccountCredentials)> {
        let new_account = NewAccount {
            contact: &[],
            terms_of_service_agreed: true,
            only_return_existing: false,
        };

        Account::builder()
            .context("Failed to create ACME account builder")?
            .create(&new_account, self.acme_url.clone(), None)
            .await
            .context("Failed to create ACME account")
    }

    async fn acme_order(&self, domain: &str, account: &Account) -> Result<Order> {
        let identifiers = [Identifier::Dns(domain.to_string())];
        let new_order: NewOrder<'_> = NewOrder::new(&identifiers);

        account
            .new_order(&new_order)
            .await
            .context("Failed to place ACME order")
    }

    async fn dns_challenge(
        &self,
        order: &mut Order,
        dns_provider: &dyn DnsProvider,
        dns_propagation_secs: u64,
    ) -> Result<Vec<String>> {
        use std::time::Duration;
        use tokio::time::sleep;

        let mut authorizations = order.authorizations();
        let mut record_ids: Vec<String> = Vec::new();

        while let Some(Ok(mut auth_handle)) = authorizations.next().await {
            match auth_handle.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                _ => bail!("Unexpected authorization status: {:?}", auth_handle.status),
            }

            let mut challenge = auth_handle
                .challenge(ChallengeType::Dns01)
                .context("No DNS-01 challenge found in authorization")?;

            let record_name = acme_record_name(&challenge.identifier().to_string());

            let record_id = dns_provider
                .create_txt_record(&record_name, &challenge.key_authorization().dns_value())
                .await
                .context("Failed to create DNS TXT record")?;

            record_ids.push(record_id);

            sleep(Duration::from_secs(dns_propagation_secs)).await;

            challenge
                .set_ready()
                .await
                .context("Failed to mark challenge as ready")?;
        }

        Ok(record_ids)
    }
}

/// The TXT record name proving control of `identifier`. A wildcard
/// (`*.example.com`) is validated against the bare domain, so the `*.` prefix is
/// stripped first: both `example.com` and `*.example.com` map to
/// `_acme-challenge.example.com`.
fn acme_record_name(identifier: &str) -> String {
    let bare = identifier.strip_prefix("*.").unwrap_or(identifier);
    format!("_acme-challenge.{bare}")
}

#[cfg(test)]
mod tests {
    use super::acme_record_name;

    #[test]
    fn record_name_exact_and_wildcard() {
        assert_eq!(
            acme_record_name("example.com"),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            acme_record_name("*.example.com"),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            acme_record_name("app.example.com"),
            "_acme-challenge.app.example.com"
        );
    }
}
