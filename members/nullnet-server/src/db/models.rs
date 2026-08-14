use crate::db::schema::{
    certificates, dns_credentials, events, login_attempts, refresh_tokens, services, user_scopes,
    users,
};
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

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = users)]
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) username: String,
    pub(crate) password_hash: String,
    pub(crate) role: String,
    pub(crate) mfa_secret_enc: Option<String>,
    pub(crate) mfa_confirmed_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewUser<'a> {
    pub(crate) id: &'a str,
    pub(crate) username: &'a str,
    pub(crate) password_hash: &'a str,
    pub(crate) role: &'a str,
    pub(crate) mfa_secret_enc: Option<&'a str>,
    pub(crate) mfa_confirmed_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// Partial update: every field is `Option`, and diesel's `AsChangeset` skips
/// setting any column whose field is `None` — so callers only pass what
/// they're actually changing. `updated_at` is always `Some(..)` in practice.
#[derive(AsChangeset, Debug, Clone, Default)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct UserUpdate<'a> {
    pub(crate) username: Option<&'a str>,
    pub(crate) role: Option<&'a str>,
    pub(crate) password_hash: Option<&'a str>,
    pub(crate) updated_at: Option<i64>,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = user_scopes)]
#[diesel(primary_key(user_id, scope))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct UserScope {
    pub(crate) user_id: String,
    pub(crate) scope: String,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = user_scopes)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewUserScope<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) scope: &'a str,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = refresh_tokens)]
#[diesel(primary_key(token_hash))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct RefreshToken {
    pub(crate) token_hash: String,
    pub(crate) user_id: String,
    pub(crate) expires_at: i64,
    pub(crate) created_at: i64,
    pub(crate) revoked_at: Option<i64>,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = refresh_tokens)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewRefreshToken<'a> {
    pub(crate) token_hash: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) expires_at: i64,
    pub(crate) created_at: i64,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = login_attempts)]
#[diesel(primary_key(username))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct LoginAttempt {
    pub(crate) username: String,
    pub(crate) failed_count: i32,
    pub(crate) locked_until: Option<i64>,
    pub(crate) updated_at: i64,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = login_attempts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewLoginAttempt<'a> {
    pub(crate) username: &'a str,
    pub(crate) failed_count: i32,
    pub(crate) locked_until: Option<i64>,
    pub(crate) updated_at: i64,
}

/// One persisted event row. `payload` is the event's own JSON serialization
/// (via `crate::events::Event`'s `Serialize` impl) — `kind`/`severity`/
/// `timestamp` are pulled out as real columns purely so they're indexable;
/// everything else stays in `payload` rather than one column per variant field.
#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = events)]
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct EventRow {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) severity: String,
    pub(crate) timestamp: i64,
    pub(crate) payload: String,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewEventRow<'a> {
    pub(crate) kind: &'a str,
    pub(crate) severity: &'a str,
    pub(crate) timestamp: i64,
    pub(crate) payload: &'a str,
}
