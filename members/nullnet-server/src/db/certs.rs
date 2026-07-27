use crate::crypto;
use crate::db::AsyncSqlite;
use crate::db::models::{Certificate, NewCertificate, NewDnsCredential};
use crate::db::schema::{certificates, dns_credentials};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A certificate as handed back to callers: PEM chain plus the *decrypted*
/// private key (the encrypted-at-rest representation never leaves this module).
pub(crate) struct CertRecord {
    pub(crate) domain: String,
    pub(crate) fullchain_pem: String,
    pub(crate) key_pem: String,
    pub(crate) not_after: i64,
}

impl CertRecord {
    fn from_row(row: Certificate) -> Result<Self, Error> {
        Ok(Self {
            key_pem: crypto::cipher().decrypt(&row.key_pem_enc)?,
            domain: row.domain,
            fullchain_pem: row.fullchain_pem,
            not_after: row.not_after,
        })
    }
}

/// Typed access to the `certificates`/`dns_credentials` tables. Handles
/// encryption at rest transparently, mirroring `certs.rs`'s current
/// file-based call surface so it can be swapped in as a drop-in replacement.
pub(crate) struct CertRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl CertRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    /// Insert or replace the cert for `domain`. Returns whether a row for
    /// this domain already existed (i.e. this was a renewal/replacement).
    pub(crate) async fn put_cert(
        &self,
        domain: &str,
        fullchain_pem: &str,
        key_pem: &str,
    ) -> Result<bool, Error> {
        let key_pem_enc = crypto::cipher().encrypt(key_pem)?;
        let not_after = parse_not_after(fullchain_pem)
            .ok_or("failed to parse notAfter from fullchain_pem")
            .handle_err(location!())?;
        let updated_at = super::now();

        let new_cert = NewCertificate {
            domain,
            fullchain_pem,
            key_pem_enc: &key_pem_enc,
            not_after,
            updated_at,
        };

        let mut conn = self.conn.lock().await;
        let existed = certificates::table
            .find(domain)
            .count()
            .get_result::<i64>(&mut *conn)
            .await
            .handle_err(location!())?
            > 0;
        diesel::insert_into(certificates::table)
            .values(&new_cert)
            .on_conflict(certificates::domain)
            .do_update()
            .set(&new_cert)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(existed)
    }

    pub(crate) async fn get(&self, domain: &str) -> Result<Option<CertRecord>, Error> {
        let mut conn = self.conn.lock().await;
        let row = certificates::table
            .find(domain)
            .first::<Certificate>(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;
        row.map(CertRecord::from_row).transpose()
    }

    pub(crate) async fn list(&self) -> Result<Vec<CertRecord>, Error> {
        let mut conn = self.conn.lock().await;
        let rows = certificates::table
            .load::<Certificate>(&mut *conn)
            .await
            .handle_err(location!())?;
        rows.into_iter().map(CertRecord::from_row).collect()
    }

    pub(crate) async fn delete(&self, domain: &str) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(certificates::table.find(domain))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    pub(crate) async fn domains(&self) -> Result<Vec<String>, Error> {
        let mut conn = self.conn.lock().await;
        certificates::table
            .select(certificates::domain)
            .load(&mut *conn)
            .await
            .handle_err(location!())
    }

    pub(crate) async fn expiry(&self, domain: &str) -> Result<Option<i64>, Error> {
        let mut conn = self.conn.lock().await;
        certificates::table
            .find(domain)
            .select(certificates::not_after)
            .first(&mut *conn)
            .await
            .optional()
            .handle_err(location!())
    }

    /// Store DNS-provider credentials (`creds_json`) encrypted at rest so the
    /// cert can be auto-renewed without re-supplying the token.
    pub(crate) async fn put_dns_credentials(
        &self,
        domain: &str,
        creds_json: &str,
    ) -> Result<(), Error> {
        let creds_json_enc = crypto::cipher().encrypt(creds_json)?;
        let new_creds = NewDnsCredential {
            domain,
            creds_json_enc: &creds_json_enc,
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(dns_credentials::table)
            .values(&new_creds)
            .on_conflict(dns_credentials::domain)
            .do_update()
            .set(&new_creds)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Load + decrypt the stored DNS-provider credentials JSON for `domain`, if any.
    pub(crate) async fn dns_credentials(&self, domain: &str) -> Result<Option<String>, Error> {
        let mut conn = self.conn.lock().await;
        let enc: Option<String> = dns_credentials::table
            .find(domain)
            .select(dns_credentials::creds_json_enc)
            .first(&mut *conn)
            .await
            .optional()
            .handle_err(location!())?;
        enc.map(|enc| crypto::cipher().decrypt(&enc)).transpose()
    }
}

/// Best-effort leaf `notAfter` (unix seconds) parsed out of a `fullchain_pem`.
fn parse_not_after(fullchain_pem: &str) -> Option<i64> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(fullchain_pem.as_bytes()).ok()?;
    let cert = pem.parse_x509().ok()?;
    Some(cert.validity().not_after.timestamp())
}

#[cfg(test)]
mod tests {
    use crate::db::Db;

    fn ensure_crypto() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            // SAFETY: runs once, before any other test thread reads the env var.
            unsafe { std::env::set_var("CERT_ENCRYPTION_KEY", "a".repeat(32)) };
            let _ = crate::crypto::init_from_env();
        });
    }

    async fn test_db() -> Db {
        ensure_crypto();
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-certs-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    fn self_signed_pem() -> (String, String) {
        let cert = rcgen::generate_simple_self_signed(vec!["example.com".to_string()]).unwrap();
        (cert.cert.pem(), cert.signing_key.serialize_pem())
    }

    #[tokio::test]
    async fn put_get_list_delete_round_trip() {
        let db = test_db().await;
        let repo = db.certs();
        let (fullchain_pem, key_pem) = self_signed_pem();

        let existed = repo
            .put_cert("example.com", &fullchain_pem, &key_pem)
            .await
            .unwrap();
        assert!(!existed, "first insert should not report an existing row");

        let existed_again = repo
            .put_cert("example.com", &fullchain_pem, &key_pem)
            .await
            .unwrap();
        assert!(existed_again, "second insert is a replacement");

        let fetched = repo.get("example.com").await.unwrap().unwrap();
        assert_eq!(fetched.fullchain_pem, fullchain_pem);
        assert_eq!(fetched.key_pem, key_pem);
        assert!(fetched.not_after > 0);

        assert_eq!(repo.domains().await.unwrap(), vec!["example.com"]);
        assert_eq!(repo.list().await.unwrap().len(), 1);
        assert_eq!(
            repo.expiry("example.com").await.unwrap(),
            Some(fetched.not_after)
        );

        repo.delete("example.com").await.unwrap();
        assert!(repo.get("example.com").await.unwrap().is_none());
        assert!(repo.domains().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dns_credentials_round_trip() {
        let db = test_db().await;
        let repo = db.certs();
        let (fullchain_pem, key_pem) = self_signed_pem();
        repo.put_cert("example.com", &fullchain_pem, &key_pem)
            .await
            .unwrap();

        assert!(repo.dns_credentials("example.com").await.unwrap().is_none());

        repo.put_dns_credentials("example.com", r#"{"token":"secret"}"#)
            .await
            .unwrap();
        assert_eq!(
            repo.dns_credentials("example.com")
                .await
                .unwrap()
                .as_deref(),
            Some(r#"{"token":"secret"}"#)
        );
    }
}
