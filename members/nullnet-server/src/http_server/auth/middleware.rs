use super::{ACCESS_COOKIE, AuthContext, unauthorized};
use crate::auth::jwt;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::cookie::CookieJar;

/// Guards every route in the `protected` router: verifies the `nn_access`
/// cookie's JWT and, on success, inserts an [`AuthContext`] into the
/// request's extensions for handlers (and scope checks) to read back out.
/// Stateless — access-token verification never touches the database.
pub(crate) async fn require_auth(jar: CookieJar, mut req: Request, next: Next) -> Response {
    let Some(cookie) = jar.get(ACCESS_COOKIE) else {
        return unauthorized("missing access token");
    };
    let claims = match jwt::verify_access_token(cookie.value()) {
        Ok(c) => c,
        Err(_) => return unauthorized("invalid or expired access token"),
    };
    let Ok(role) = claims.role.parse() else {
        return unauthorized("invalid token");
    };
    req.extensions_mut().insert(AuthContext {
        user_id: claims.sub,
        role,
        scopes: claims.scopes,
    });
    next.run(req).await
}
