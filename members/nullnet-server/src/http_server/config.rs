use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::Response;

pub(super) async fn config_handler(Path(stack): Path<String>) -> Response {
    // Reject path traversal / nested paths: stack must be a single bare name.
    if stack.is_empty() || stack.contains(['/', '\\', '.']) {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    let path = format!("./services/{stack}.toml");
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(axum::body::Body::from(content))
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::empty())
            .unwrap(),
    }
}
