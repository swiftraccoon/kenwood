//! Build script: compiles native Bluetooth and Vision wrappers for macOS targets.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    println!("cargo:rerun-if-changed=src/transport/bluetooth_mac.m");
    println!("cargo:rerun-if-changed=src/screen/vision_mac.m");

    // The deterministic archive flag from the `cc` crate causes cosmetic
    // warnings with Xcode's `ar`; the archives themselves remain valid.
    cc::Build::new()
        .file("src/transport/bluetooth_mac.m")
        .flag("-fobjc-arc")
        .compile("bluetooth_mac");

    cc::Build::new()
        .file("src/screen/vision_mac.m")
        .flag("-fobjc-arc")
        .compile("vision_mac");

    println!("cargo:rustc-link-lib=framework=IOBluetooth");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=Vision");
}
