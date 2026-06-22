use nullnet_grpc_lib::nullnet_grpc::Net;

pub static NET_TYPE: std::sync::LazyLock<Net> = std::sync::LazyLock::new(|| {
    let str = std::env::var("NET_TYPE").unwrap_or_else(|_| {
        println!("'NET_TYPE' environment variable not set");
        String::new()
    });

    match str.to_uppercase().as_str() {
        "VXLAN" => Net::Vxlan,
        "VLAN" => Net::Vlan,
        _ => Net::default(),
    }
});
