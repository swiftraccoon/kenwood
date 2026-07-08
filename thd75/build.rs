//! Build script — compiles the macOS `IOBluetooth` RFCOMM wrapper.

// `cc` is only exercised on macOS (see the `cfg` block below). On every other
// target the build script is a no-op, so the `cc` build-dependency would trip
// `unused_crate_dependencies`. Mark it used with an anonymous import there; on
// macOS `cc::Build` itself satisfies the lint, so the import is cfg'd out.
#[cfg(not(target_os = "macos"))]
use cc as _;

fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/transport/bluetooth_mac.m");

        // The `ar -D` (deterministic) flag from cc crate causes cosmetic warnings
        // on macOS Xcode ar. These are harmless — the archive is still created correctly.
        // Suppressed by redirecting ar stderr in the cc crate is not possible, so we
        // accept the warnings. They do not affect the build.
        cc::Build::new()
            .file("src/transport/bluetooth_mac.m")
            .flag("-fobjc-arc")
            .compile("bluetooth_mac");

        println!("cargo:rustc-link-lib=framework=IOBluetooth");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
