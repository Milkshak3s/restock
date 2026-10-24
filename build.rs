use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    // Release builds ask for elevation once up front so a bulk install doesn't
    // prompt UAC per installer. Debug builds stay as-invoker so `cargo run` works
    // from a normal terminal.
    let level = if std::env::var("PROFILE").as_deref() == Ok("release") {
        ExecutionLevel::RequireAdministrator
    } else {
        ExecutionLevel::AsInvoker
    };

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("Restock").requested_execution_level(level))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
