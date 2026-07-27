pub static CONTROL_SERVICE_ADDR: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("CONTROL_SERVICE_ADDR").unwrap_or_else(|_| {
        println!("'CONTROL_SERVICE_ADDR' environment variable not set");
        "0.0.0.0".to_string()
    })
});

pub static CONTROL_SERVICE_PORT: std::sync::LazyLock<u16> = std::sync::LazyLock::new(|| {
    let str = std::env::var("CONTROL_SERVICE_PORT").unwrap_or_else(|_| {
        println!("'CONTROL_SERVICE_PORT' environment variable not set");
        String::new()
    });

    str.parse().unwrap_or(50051)
});

/// Path to the control server's private CA root (its `grpc-tls/ca-cert.pem`
/// — generated automatically on the server, copy it here) — pins the control
/// channel to that CA for full standard chain validation. Defaults to the
/// repo root's `ca-cert.pem` (`../../ca-cert.pem`, relative to the service's
/// `members/nullnet-proxy` working directory) if unset; the connection
/// fails at startup if the file is missing or not a valid cert.
pub static CONTROL_SERVICE_CA_CERT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("CONTROL_SERVICE_CA_CERT").unwrap_or_else(|_| "../../ca-cert.pem".to_string())
});
