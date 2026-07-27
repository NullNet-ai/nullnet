//! Core authentication/authorization logic: roles, scopes, password hashing,
//! TOTP MFA, JWT issuing/verification, and refresh-token/bootstrap helpers.
//! No Axum here — the HTTP layer lives in `http_server::auth`.

pub(crate) mod bootstrap;
pub(crate) mod jwt;
pub(crate) mod mfa_crypto;
pub(crate) mod password;
pub(crate) mod session;
pub(crate) mod totp;

use nullnet_liberror::{Error, ErrorHandler, Location, location};

/// Unix seconds. Mirrors `db::now()` — kept separate since `auth` doesn't
/// otherwise depend on `db`.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Admin,
    User,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "admin" => Ok(Role::Admin),
            "user" => Ok(Role::User),
            other => Err::<Self, _>(format!("unknown role '{other}'")).handle_err(location!()),
        }
    }
}

/// A single resource-level permission. `admin`-role users implicitly have
/// every scope; `user`-role accounts get an explicit subset assigned by an
/// admin (persisted in the `user_scopes` table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    CertificatesRead,
    CertificatesWrite,
    ConfigRead,
    ConfigWrite,
    SessionsRead,
    SessionsWrite,
    NodesRead,
    EventsRead,
}

impl Scope {
    pub(crate) const ALL: [Scope; 8] = [
        Scope::CertificatesRead,
        Scope::CertificatesWrite,
        Scope::ConfigRead,
        Scope::ConfigWrite,
        Scope::SessionsRead,
        Scope::SessionsWrite,
        Scope::NodesRead,
        Scope::EventsRead,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Scope::CertificatesRead => "certificates:read",
            Scope::CertificatesWrite => "certificates:write",
            Scope::ConfigRead => "config:read",
            Scope::ConfigWrite => "config:write",
            Scope::SessionsRead => "sessions:read",
            Scope::SessionsWrite => "sessions:write",
            Scope::NodesRead => "nodes:read",
            Scope::EventsRead => "events:read",
        }
    }

    pub(crate) fn from_str_opt(s: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|scope| scope.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::{Role, Scope};
    use std::str::FromStr;

    #[test]
    fn role_round_trips() {
        assert_eq!(Role::from_str("admin").unwrap().as_str(), "admin");
        assert_eq!(Role::from_str("user").unwrap().as_str(), "user");
        assert!(Role::from_str("nope").is_err());
    }

    #[test]
    fn scope_round_trips() {
        for scope in Scope::ALL {
            assert_eq!(Scope::from_str_opt(scope.as_str()), Some(scope));
        }
        assert_eq!(Scope::from_str_opt("bogus:scope"), None);
    }
}
