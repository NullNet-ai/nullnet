use pingora_core::Result;
use pingora_http::ResponseHeader;

pub(crate) fn apply(response: &mut ResponseHeader, tls: bool, enabled: bool) -> Result<()> {
    if tls && enabled {
        response.insert_header("Strict-Transport-Security", "max-age=31536000")?;
    } else {
        response.remove_header("Strict-Transport-Security");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_backend_policy_on_https_success_redirect_and_error() {
        for status in [200, 301, 403, 404, 500, 502] {
            let mut response = ResponseHeader::build(status, None).unwrap();
            response
                .append_header("Strict-Transport-Security", "max-age=0")
                .unwrap();
            response
                .append_header("Strict-Transport-Security", "max-age=60; includeSubDomains")
                .unwrap();
            apply(&mut response, true, true).unwrap();
            let values: Vec<_> = response
                .headers
                .get_all("strict-transport-security")
                .iter()
                .collect();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0], "max-age=31536000");
            assert_eq!(response.status.as_u16(), status);
        }
    }

    #[test]
    fn removes_backend_policy_on_http_or_when_disabled() {
        for (tls, enabled) in [(false, true), (false, false), (true, false)] {
            let mut response = ResponseHeader::build(200, None).unwrap();
            response
                .insert_header("Strict-Transport-Security", "max-age=31536000")
                .unwrap();
            apply(&mut response, tls, enabled).unwrap();
            assert!(!response.headers.contains_key("strict-transport-security"));
        }
    }
}
