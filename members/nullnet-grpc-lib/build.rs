const NULLNET_GRPC_PATH: &str = "./proto/nullnet_grpc.proto";
const PROTOBUF_DIR_PATH: &str = "./proto";

fn main() {
    tonic_prost_build::configure()
        .out_dir("./src/proto")
        // async-trait generates a bare `#[must_use]` on each method of the
        // server trait, which clippy flags as redundant on top of the
        // already-must_use boxed Future it returns.
        .trait_attribute("NullnetGrpc", "#[allow(clippy::double_must_use)]")
        .compile_protos(&[NULLNET_GRPC_PATH], &[PROTOBUF_DIR_PATH])
        .expect("Protobuf files generation failed");
}
