//! Build script — compiles the macOS `IOBluetooth` RFCOMM wrapper.

fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/transport/bluetooth_mac.m");

        cc::Build::new()
            .file("src/transport/bluetooth_mac.m")
            .flag("-fobjc-arc")
            .compile("bluetooth_mac");

        println!("cargo:rustc-link-lib=framework=IOBluetooth");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
