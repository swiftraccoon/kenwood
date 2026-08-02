import Foundation

/// Attempts the raw-USB control paths that documentation says are closed to
/// iPhone apps, and records exactly how iOS refuses, providing empirical
/// evidence to replace quoted verdicts. Every attempt is read-only and failure-tolerant:
/// the point is to observe denials, so nothing here must ever crash the app.
///
/// Three probes:
///  1. IOKit: the public `IOServiceGetMatchingServices` / `IOServiceOpen`
///     C API (linked via the bridging header). Can a sandboxed app even
///     *see* USB services, and what happens when it tries to *open* one?
///  2. Private framework: `dlopen` the on-device `IOUSBHost` private
///     framework and check whether its classes are loadable at all.
///  3. Entitlements: what USB/DriverKit entitlements does this app's own
///     provisioning profile actually carry? (Predicted: none exist to grant.)
enum ControlProbe {
    static func run() -> [String] {
        var out = ["=== USB control-path probe ==="]
        #if targetEnvironment(simulator)
        out.append("!! SIMULATOR: a sim process is a Mac process sharing macOS's")
        out.append("!! UNSANDBOXED IOKit registry; these results reflect this Mac,")
        out.append("!! NOT iOS. Only an on-DEVICE run tests the iPhone sandbox.")
        #else
        out.append("[device] results below reflect the real iPhone app sandbox")
        #endif
        out += probeIOKitUSB()
        out.append("[IOKit] registry properties (can an app IDENTIFY the radio?):")
        for className in ["IOUSBHostDevice", "IOUSBHostInterface"] {
            out += PrivateUSBAttempt.dumpProperties(forClass: className, limit: 4)
        }
        out += PrivateUSBAttempt.dumpUSBDeviceTree()
        out += probeSerialDrivers()
        out += probeSerialDeviceNodes()
        out += probePrivateFramework()
        out += PrivateUSBAttempt.attemptPrivateHostDeviceOpen()
        out += probeEntitlements()
        out.append("=== end control-path probe ===")
        return out
    }

    // MARK: IOKit raw-USB attempt

    /// USB-relevant IOKit class names to match against, plus `IOService`
    /// (matches everything) as a positive control that matching works at all.
    private static let usbClasses = [
        "IOUSBHostDevice",
        "IOUSBHostInterface",
        "IOUSBDevice",
        "IOUSBInterface",
        "AppleUSBHostController",
        "IOService",
    ]

    private static func probeIOKitUSB() -> [String] {
        var lines = ["[IOKit] IOServiceGetMatchingServices / IOServiceOpen from the app sandbox:"]
        for name in usbClasses {
            guard let matching = IOServiceMatching(name) else {
                lines.append("  \(name): IOServiceMatching returned nil")
                continue
            }
            var iterator: io_iterator_t = 0
            // IOServiceGetMatchingServices consumes the matching dict ref.
            let kr = IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator)
            guard kr == KERN_SUCCESS else {
                lines.append("  \(name): matching failed kr=\(hex(kr))")
                continue
            }
            var count = 0
            var firstService: io_service_t = 0
            while case let service = IOIteratorNext(iterator), service != 0 {
                if count == 0 {
                    firstService = service
                } else {
                    _ = IOObjectRelease(service)
                }
                count += 1
            }
            _ = IOObjectRelease(iterator)

            if name == "IOService" {
                // Positive control: a huge count proves the registry and
                // matching are reachable, so a zero USB count below means
                // "USB filtered/absent", not "IOKit blocked wholesale".
                lines.append("  \(name): \(count) services matched (positive control)")
                if firstService != 0 { _ = IOObjectRelease(firstService) }
                continue
            }

            guard count > 0, firstService != 0 else {
                lines.append("  \(name): 0 matched")
                continue
            }
            // Can we read a property (enumerate), distinct from opening?
            let readable = readableProperty(firstService)
            // The real test: attempt to open a user client.
            var connect: io_connect_t = 0
            let openKr = IOServiceOpen(firstService, mach_task_self_, 0, &connect)
            if openKr == KERN_SUCCESS {
                lines.append("  \(name): \(count) matched, property=\(readable), IOServiceOpen SUCCEEDED (connect=\(connect)) !!")
                _ = IOServiceClose(connect)
            } else {
                lines.append("  \(name): \(count) matched, property=\(readable), IOServiceOpen DENIED kr=\(hex(openKr)) \(returnName(openKr))")
            }
            _ = IOObjectRelease(firstService)
        }
        return lines
    }

    private static func readableProperty(_ service: io_service_t) -> String {
        if let prop = IORegistryEntryCreateCFProperty(service, "IOClass" as CFString, kCFAllocatorDefault, 0) {
            let value = prop.takeRetainedValue()
            if let str = value as? String {
                return "IOClass=\(str)"
            }
            return "read ok"
        }
        return "unreadable"
    }

    // MARK: private framework load attempt

    /// Serial / CDC driver classes. If iOS shipped a CDC-ACM class driver,
    /// it would have claimed the radio's two CDC interfaces and there would
    /// be a serial service here to talk to. Zero matches everywhere is the
    /// mechanical reason CAT is unreachable, independent of entitlements.
    private static let serialClasses = [
        "IOSerialBSDClient",
        "IOSerialDriverSync",
        "IOModemSerialStreamSync",
        "IORS232SerialStreamSync",
        "AppleUSBCDC",
        "AppleUSBCDCACMData",
        "AppleUSBCDCACMControl",
        "AppleUSBCDCNCMData",
        "IOUserSerial",
        "AppleUSBHostCompositeDevice",
    ]

    /// If iOS ships the CDC-ACM driver that macOS uses, the radio's data
    /// interface would get an `IOSerialBSDClient` and therefore a `/dev`
    /// node. A POSIX `open()` is gated by the filesystem sandbox, which is
    /// a completely separate mechanism from the IOKit user-client gate that
    /// denied us above, so it is worth attempting independently.
    private static func probeSerialDeviceNodes() -> [String] {
        var lines = ["[POSIX] /dev nodes (did iOS create a tty for the CDC interface?):"]
        guard let dir = opendir("/dev") else {
            lines.append("  opendir(/dev) failed errno=\(errno): sandbox hides /dev entirely")
            return lines
        }
        defer { closedir(dir) }

        var all: [String] = []
        while let entry = readdir(dir) {
            var raw = entry.pointee.d_name
            let name = withUnsafePointer(to: &raw) { pointer in
                pointer.withMemoryRebound(to: CChar.self, capacity: Int(NAME_MAX)) {
                    String(cString: $0)
                }
            }
            if name != "." && name != ".." {
                all.append(name)
            }
        }
        lines.append("  /dev readable: \(all.count) entries (positive control)")

        let candidates = all.filter { name in
            let lower = name.lowercased()
            return lower.hasPrefix("cu.") || lower.hasPrefix("tty.")
                || lower.contains("usbmodem") || lower.contains("serial")
                || lower.contains("acm") || lower.contains("modem")
        }.sorted()
        if candidates.isEmpty {
            lines.append("  serial-looking nodes: NONE (no CDC-ACM driver bound a tty)")
            // Show the whole list once: it is short on iOS and proves the
            // absence is real rather than a filter bug.
            lines.append("  all /dev entries: \(all.sorted().joined(separator: " "))")
            return lines
        }
        lines.append("  serial-looking nodes: \(candidates.joined(separator: ", "))")
        for name in candidates {
            let path = "/dev/" + name
            let fd = open(path, O_RDWR | O_NONBLOCK | O_NOCTTY)
            if fd >= 0 {
                lines.append("  \(path): open() SUCCEEDED (fd=\(fd)) !! serial pipe reachable")
                close(fd)
            } else {
                let code = errno
                let message = String(cString: strerror(code))
                lines.append("  \(path): open() failed errno=\(code) (\(message))")
            }
        }
        return lines
    }

    private static func probeSerialDrivers() -> [String] {
        var lines = ["[IOKit] serial/CDC driver classes present in this registry:"]
        for name in serialClasses {
            guard let matching = IOServiceMatching(name) else {
                lines.append("  \(name): matching dict nil")
                continue
            }
            var iterator: io_iterator_t = 0
            let kr = IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator)
            guard kr == KERN_SUCCESS else {
                lines.append("  \(name): matching failed \(hex(kr))")
                continue
            }
            var count = 0
            while case let service = IOIteratorNext(iterator), service != 0 {
                count += 1
                _ = IOObjectRelease(service)
            }
            _ = IOObjectRelease(iterator)
            lines.append("  \(name): \(count)")
        }
        return lines
    }

    private static func probePrivateFramework() -> [String] {
        var lines = ["[private] dlopen IOUSBHost + class visibility:"]
        let path = "/System/Library/PrivateFrameworks/IOUSBHost.framework/IOUSBHost"
        if let handle = dlopen(path, RTLD_NOW) {
            lines.append("  dlopen ok (handle non-null)")
            dlclose(handle)
        } else {
            let err = dlerror().map { String(cString: $0) } ?? "unknown"
            lines.append("  dlopen FAILED: \(err)")
        }
        for cls in ["IOUSBHostDevice", "IOUSBHostInterface", "IOUSBHostPipe"] {
            if let type = NSClassFromString(cls) {
                lines.append("  class \(cls): visible (\(type))")
            } else {
                lines.append("  class \(cls): not loadable (NSClassFromString nil)")
            }
        }
        return lines
    }

    // MARK: entitlement introspection

    private static func probeEntitlements() -> [String] {
        var lines = ["[entitlements] this app's provisioning profile:"]
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url)
        else {
            lines.append("  no embedded.mobileprovision (simulator or ad-hoc build); nothing to inspect")
            return lines
        }
        guard let start = data.range(of: Data("<plist".utf8)),
              let end = data.range(of: Data("</plist>".utf8))
        else {
            lines.append("  could not locate embedded plist in profile")
            return lines
        }
        let plistData = data.subdata(in: start.lowerBound..<end.upperBound)
        guard let plist = try? PropertyListSerialization.propertyList(from: plistData, format: nil),
              let dict = plist as? [String: Any],
              let entitlements = dict["Entitlements"] as? [String: Any]
        else {
            lines.append("  could not parse profile entitlements")
            return lines
        }
        let keys = entitlements.keys.sorted()
        lines.append("  \(keys.count) entitlement keys granted")
        let interesting = keys.filter { key in
            let lower = key.lowercased()
            return lower.contains("usb") || lower.contains("driverkit")
                || lower.contains("iokit") || lower.contains("accessory")
                || lower.contains("serial") || lower.contains("communicates")
        }
        if interesting.isEmpty {
            lines.append("  USB/DriverKit/IOKit/accessory entitlements: NONE")
        } else {
            for key in interesting {
                lines.append("  • \(key) = \(entitlements[key] ?? "?")")
            }
        }
        return lines
    }

    // MARK: helpers

    private static func hex(_ value: Int32) -> String {
        "0x" + String(format: "%08x", UInt32(bitPattern: value))
    }

    /// Names the handful of `kIOReturn*` codes this probe expects, so the
    /// console reads without a lookup table.
    private static func returnName(_ kr: Int32) -> String {
        switch UInt32(bitPattern: kr) {
        case 0xe000_02e2: "(kIOReturnNotPermitted)"
        case 0xe000_02c0: "(kIOReturnUnsupported)"
        case 0xe000_02d5: "(kIOReturnNoResources)"
        case 0xe000_02c7: "(kIOReturnNoDevice)"
        case 0xe000_02d4: "(kIOReturnNotPrivileged)"
        case 0xe000_02bc: "(kIOReturnError)"
        default: ""
        }
    }
}
