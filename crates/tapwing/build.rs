//! Generates the Maestro Android driver gRPC client (`maestro_android.proto`).
//!
//! Needs `protoc`: `$PROTOC` wins, then `protoc` on `PATH`, then the usual
//! Homebrew / system locations.

use std::path::PathBuf;

fn find_protoc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PROTOC") {
        return Some(PathBuf::from(p));
    }
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("protoc"))
        .find(|p| p.is_file());
    on_path.or_else(|| {
        ["/opt/homebrew/bin/protoc", "/usr/local/bin/protoc", "/usr/bin/protoc"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

fn main() {
    println!("cargo:rerun-if-changed=proto/maestro_android.proto");
    println!("cargo:rerun-if-env-changed=PROTOC");
    let Some(protoc) = find_protoc() else {
        panic!("`protoc` not found: install it (e.g. `brew install protobuf`) or set $PROTOC");
    };
    // SAFETY: build scripts are single-threaded at this point.
    unsafe { std::env::set_var("PROTOC", protoc) };
    tonic_prost_build::configure()
        // The server side is only used by in-process fakes in tests.
        .build_server(true)
        .compile_protos(&["proto/maestro_android.proto"], &["proto"])
        .expect("compiling maestro_android.proto");
}
