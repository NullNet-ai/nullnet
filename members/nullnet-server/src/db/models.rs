use crate::db::schema::{
    certificates, dns_credentials, events, login_attempts, refresh_tokens, routes,
    service_dependencies, service_triggers, services, stacks, user_scopes, users,
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
#[diesel(table_name = stacks)]
#[diesel(primary_key(name))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct Stack {
    pub(crate) name: String,
    pub(crate) updated_at: i64,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = stacks)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewStack<'a> {
    pub(crate) name: &'a str,
    pub(crate) updated_at: i64,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = services)]
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct ServiceRow {
    pub(crate) id: i32,
    pub(crate) stack: String,
    pub(crate) name: String,
    pub(crate) docker_container: Option<String>,
    pub(crate) process_path: Option<String>,
    pub(crate) port: Option<i32>,
    pub(crate) timeout: Option<i64>,
    pub(crate) max_networks: Option<i32>,
    pub(crate) protocol: Option<String>,
    pub(crate) listen_port: Option<i32>,
    pub(crate) egress_blocked_countries: Option<String>,
    pub(crate) egress_allowed_countries: Option<String>,
    pub(crate) ingress_blocked_countries: Option<String>,
    pub(crate) ingress_allowed_countries: Option<String>,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = services)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewServiceRow<'a> {
    pub(crate) stack: &'a str,
    pub(crate) name: &'a str,
    pub(crate) docker_container: Option<&'a str>,
    pub(crate) process_path: Option<&'a str>,
    pub(crate) port: Option<i32>,
    pub(crate) timeout: Option<i64>,
    pub(crate) max_networks: Option<i32>,
    pub(crate) protocol: Option<&'a str>,
    pub(crate) listen_port: Option<i32>,
    pub(crate) egress_blocked_countries: Option<String>,
    pub(crate) egress_allowed_countries: Option<String>,
    pub(crate) ingress_blocked_countries: Option<String>,
    pub(crate) ingress_allowed_countries: Option<String>,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = service_triggers)]
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct ServiceTriggerRow {
    pub(crate) id: i32,
    pub(crate) service_id: i32,
    pub(crate) port: i32,
    pub(crate) chain: String,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = service_triggers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewServiceTriggerRow {
    pub(crate) service_id: i32,
    pub(crate) port: i32,
    pub(crate) chain: String,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = service_dependencies)]
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct ServiceDependencyRow {
    pub(crate) id: i32,
    pub(crate) service_id: i32,
    pub(crate) branch_index: i32,
    pub(crate) chain: String,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = service_dependencies)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewServiceDependencyRow {
    pub(crate) service_id: i32,
    pub(crate) branch_index: i32,
    pub(crate) chain: String,
}

#[derive(Queryable, Selectable, Identifiable, Debug, Clone)]
#[diesel(table_name = routes)]
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct RouteRow {
    pub(crate) id: i32,
    pub(crate) stack: String,
    pub(crate) host: String,
    pub(crate) path: String,
    pub(crate) target_kind: String,
    pub(crate) target_service: Option<String>,
    pub(crate) strip_prefix: bool,
    pub(crate) redirect_to: Option<String>,
    pub(crate) redirect_status: Option<i32>,
    pub(crate) preserve_path: bool,
    pub(crate) preserve_query: bool,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = routes)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewRouteRow<'a> {
    pub(crate) stack: &'a str,
    pub(crate) host: &'a str,
    pub(crate) path: &'a str,
    pub(crate) target_kind: &'a str,
    pub(crate) target_service: Option<&'a str>,
    pub(crate) strip_prefix: bool,
    pub(crate) redirect_to: Option<&'a str>,
    pub(crate) redirect_status: Option<i32>,
    pub(crate) preserve_path: bool,
    pub(crate) preserve_query: bool,
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
