//! Compile the optional isolated native Bluetooth helper for macOS only.

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-changed=src/bluetooth_mac.m");
    println!("cargo:rerun-if-changed=src/bluetooth_startup_tests.m");
    println!("cargo:rerun-if-changed=src/bluetooth_open_failure_tests.m");
    #[cfg(feature = "native-bluetooth")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/bluetooth_mac.m")
            .flag("-fobjc-arc")
            .compile("kenwood_bluetooth_mac");
        println!("cargo:rustc-link-lib=framework=IOBluetooth");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
