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

pub static CERTS_DIR: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("CERTS_DIR").unwrap_or_else(|_| {
        println!("'CERTS_DIR' environment variable not set; falling back to local 'certs' dir");
        "certs".to_string()
    })
});
