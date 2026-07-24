use crate::db::schema::{certificates, dns_credentials, services};
use diesel::prelude::*;

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = certificates)]
#[diesel(primary_key(domain))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct Certificate {
    pub(crate) domain: String,
    pub(crate) fullchain_pem: String,
    pub(crate) key_pem_enc: String,
    pub(crate) not_after: i64,
    pub(crate) updated_at: i64,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = certificates)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewCertificate<'a> {
    pub(crate) domain: &'a str,
    pub(crate) fullchain_pem: &'a str,
    pub(crate) key_pem_enc: &'a str,
    pub(crate) not_after: i64,
    pub(crate) updated_at: i64,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = dns_credentials)]
#[diesel(primary_key(domain))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct DnsCredential {
    pub(crate) domain: String,
    pub(crate) creds_json_enc: String,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = dns_credentials)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewDnsCredential<'a> {
    pub(crate) domain: &'a str,
    pub(crate) creds_json_enc: &'a str,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = services)]
#[diesel(primary_key(stack))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct Service {
    pub(crate) stack: String,
    pub(crate) service_json: String,
    pub(crate) updated_at: i64,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = services)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewService<'a> {
    pub(crate) stack: &'a str,
    pub(crate) service_json: &'a str,
    pub(crate) updated_at: i64,
}
