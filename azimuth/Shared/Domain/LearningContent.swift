// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

import Foundation

enum LearningCollection: String, CaseIterable, Identifiable, Sendable {
    case essentials
    case operate
    case understand
    case safety

    var id: String { rawValue }

    var title: String {
        switch self {
        case .essentials: return "Start here"
        case .operate: return "Operate"
        case .understand: return "Understand the D75"
        case .safety: return "Work safely"
        }
    }
}

struct LearningChapter: Identifiable, Hashable, Sendable {
    let id: String
    let collection: LearningCollection
    let title: String
    let eyebrow: String
    let summary: String
    let symbol: String
    let sections: [Section]
    let relatedGroups: [RadioSettingGroup]

    struct Section: Hashable, Sendable {
        let heading: String
        let body: String
        let points: [String]
    }

    func matches(_ query: String) -> Bool {
        let term = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !term.isEmpty else { return true }
        let searchable = ([title, eyebrow, summary]
            + sections.flatMap { [$0.heading, $0.body] + $0.points })
            .joined(separator: " ")
        return searchable.localizedCaseInsensitiveContains(term)
    }
}

/// Original, task-oriented guidance derived from the capabilities Azimuth
/// exposes. It is not a replacement for regulatory knowledge or the Kenwood
/// reference manual; it teaches the relationships an operator must understand
/// before using a control or changing a setting.
enum AzimuthLearningLibrary {
    static let chapters: [LearningChapter] = [
        LearningChapter(
            id: "d75-map",
            collection: .essentials,
            title: "Your D75, mapped",
            eyebrow: "Five-minute orientation",
            summary: "See how radio operation, memories, digital voice, APRS/GPS, and computer control fit together.",
            symbol: "map",
            sections: [
                .init(
                    heading: "One radio, several systems",
                    body: "The TH-D75 combines two receiver sides, stored channels, digital voice, position reporting, and computer-facing modes. Azimuth separates those jobs so a live operating control never looks like a stored setting.",
                    points: [
                        "Radio mirrors the live screen and complete front panel.",
                        "Settings shows definitions, live before values, and staged after values.",
                        "Assistant creates a proposal that waits for Accept or Decline.",
                        "Learn explains the consequence and prerequisites of each workflow.",
                    ]
                ),
                .init(
                    heading: "Trust the source badge",
                    body: "Schema metadata says what a field may contain; a radio snapshot says what this radio contained at a particular time; live telemetry says what is happening now. Azimuth labels these sources instead of filling gaps with plausible data.",
                    points: []
                ),
            ],
            relatedGroups: [.radio, .memory, .digitalVoice, .aprs, .gps, .connectivity]
        ),
        LearningChapter(
            id: "analog-basics",
            collection: .essentials,
            title: "Receive clearly, transmit deliberately",
            eyebrow: "Analog fundamentals",
            summary: "Separate frequency, mode, squelch, and transmit power so each control solves the right problem.",
            symbol: "waveform.path.ecg",
            sections: [
                .init(
                    heading: "Build the receive path first",
                    body: "Frequency chooses where to listen, mode chooses how to decode the signal, and squelch decides when audio opens. Raising squelch hides weak signals; it does not improve them. Confirm useful receive audio before considering transmit settings.",
                    points: [
                        "Match FM or narrow FM to the channel plan in use.",
                        "Use monitor to distinguish a quiet channel from squelch that is set too tightly.",
                        "Volume changes listening level; it does not change receiver sensitivity.",
                    ]
                ),
                .init(
                    heading: "Treat transmit as a separate decision",
                    body: "A frequency that can be received is not automatically legal or supported for transmission. Confirm band privileges, mode, repeater parameters, and local rules, then use the lowest power that reliably completes the path.",
                    points: []
                ),
            ],
            relatedGroups: [.radio, .audio]
        ),
        LearningChapter(
            id: "bands-and-modes",
            collection: .understand,
            title: "Bands, sides, and modes",
            eyebrow: "Read the live screen",
            summary: "Understand why receiver side A or B, tuned frequency, and operating mode are three different pieces of state.",
            symbol: "dial.medium",
            sections: [
                .init(
                    heading: "A and B are receiver sides",
                    body: "The active side determines which receiver a front-panel action affects. It is not itself an RF band. The tuned frequency and mode on that side determine what signal can be received, while supported ranges and transmit eligibility vary by market and radio state.",
                    points: [
                        "FM and NFM (narrow FM) are common analog voice choices; choose the one expected by the channel.",
                        "AM, WFM, LSB, USB, and CW may be useful receive modes on suitable signals.",
                        "DV and DR belong to D-STAR operation and carry routing state beyond frequency alone.",
                        "Read back frequency and mode after changing sides so the next key affects the intended receiver.",
                    ]
                ),
            ],
            relatedGroups: [.radio, .memory, .digitalVoice]
        ),
        LearningChapter(
            id: "usb-c",
            collection: .essentials,
            title: "Connect over USB-C",
            eyebrow: "iPad and Mac",
            summary: "Establish a wired control path and recognize the evidence that each radio capability is actually ready.",
            symbol: "cable.connector",
            sections: [
                .init(
                    heading: "Prove the physical path",
                    body: "Use a data-capable cable, keep the radio powered, and make sure another application does not own its interface. A charge-only cable can provide power without exposing any control channel.",
                    points: [
                        "On iPad, control uses wired USB-C rather than Bluetooth serial.",
                        "Wait for Azimuth to identify the device and transport.",
                        "Avoid hubs or adapters while diagnosing an intermittent connection.",
                    ]
                ),
                .init(
                    heading: "Connected is only the first gate",
                    body: "An open byte transport does not prove that screen capture, key control, settings read, and settings write are all available. Each capability negotiates separately and keeps its own status in Radio.",
                    points: []
                ),
            ],
            relatedGroups: [.connectivity]
        ),
        LearningChapter(
            id: "remote-operation",
            collection: .operate,
            title: "Operate from the remote surface",
            eyebrow: "Screen and front panel",
            summary: "Use live color frames and all 25 automated keys without losing track of radio state.",
            symbol: "rectangle.on.rectangle",
            sections: [
                .init(
                    heading: "A frame is evidence",
                    body: "The remote surface displays only authenticated 240 × 180 color frames delivered by the control core. Check frame age before acting; Azimuth deliberately shows no frequency or meter when a live frame is unavailable.",
                    points: [
                        "Confirm the highlighted side before entering a frequency or recalling memory.",
                        "Let one key action finish before sending a rapid sequence.",
                        "Use the named MODE, MENU, A/B, function, keypad, and microphone PF controls just as deliberately as the physical keys.",
                    ]
                ),
                .init(
                    heading: "Transmission remains explicit",
                    body: "The general remote panel does not provide a push-to-talk shortcut. Transmit workflows require a separate surface that can show state, authorization context, and a deliberate confirmation.",
                    points: []
                ),
            ],
            relatedGroups: [.radio, .display, .audio]
        ),
        LearningChapter(
            id: "scan-and-resume",
            collection: .operate,
            title: "Build a useful scan",
            eyebrow: "Find activity without surprises",
            summary: "Choose what to scan, set squelch sensibly, and make resume behavior match the kind of activity you seek.",
            symbol: "arrow.trianglehead.2.clockwise.rotate.90",
            sections: [
                .init(
                    heading: "A scan is a list plus a stop rule",
                    body: "Scanning becomes useful only when its range or memory set contains relevant channels. Exclude persistent interference and verify squelch first; an always-open receiver prevents useful movement through the list.",
                    points: [
                        "Time resume continues after a timed pause even if activity remains.",
                        "Carrier resume waits for received activity to clear before continuing.",
                        "Seek behavior stops on a found signal so the operator can decide what to do next.",
                    ]
                ),
                .init(
                    heading: "Test with known activity",
                    body: "Run a short scan while watching the live screen. Confirm which side is scanning, which groups participate, why it stopped, and whether the selected resume rule behaves as intended before leaving it unattended.",
                    points: []
                ),
            ],
            relatedGroups: [.memory, .radio, .audio]
        ),
        LearningChapter(
            id: "memory-channels",
            collection: .operate,
            title: "Turn repeatable setups into memories",
            eyebrow: "Channels and names",
            summary: "Store the full operating context, not just a frequency, then organize it for recall and scanning.",
            symbol: "square.stack.3d.up",
            sections: [
                .init(
                    heading: "A memory is an operating recipe",
                    body: "A useful channel can include receive frequency, mode, tuning step, transmit offset, tone or code behavior, and a readable name. Review every component before saving so recall does not restore an old or unintended transmit path.",
                    points: [
                        "Use names that remain distinct on the radio's compact display.",
                        "Keep related channels together when that improves scan selection.",
                        "After editing, recall the channel and verify the live result instead of trusting the label alone.",
                    ]
                ),
                .init(
                    heading: "Know whether you are in VFO or memory recall",
                    body: "A front-panel frequency entry and a recalled memory have different persistence. Before changing a value, confirm whether you intend a temporary VFO setup or a stored channel update.",
                    points: []
                ),
            ],
            relatedGroups: [.memory, .radio]
        ),
        LearningChapter(
            id: "repeaters-and-tones",
            collection: .operate,
            title: "Configure a repeater path",
            eyebrow: "Offset, tone, and verification",
            summary: "Assemble receive frequency, transmit offset, and access signaling without confusing tone systems.",
            symbol: "arrow.up.arrow.down.circle",
            sections: [
                .init(
                    heading: "Start from published repeater data",
                    body: "Enter the receive frequency, offset direction and amount, then the access method specified by the repeater. A CTCSS transmit tone, tone squelch, and DCS are different behaviors; enabling the wrong one can silence receive audio or prevent access.",
                    points: [
                        "Confirm the live transmit frequency with reverse or offset information before keying.",
                        "Use monitor to check whether receive filtering is hiding otherwise audible traffic.",
                        "Store the verified result as a memory only after both receive and access behavior are correct.",
                    ]
                ),
                .init(
                    heading: "Diagnose one layer at a time",
                    body: "If a repeater is heard but not accessed, verify offset and transmit tone before increasing power. If it opens but audio remains silent, inspect receive tone or code filtering separately.",
                    points: []
                ),
            ],
            relatedGroups: [.radio, .memory, .audio]
        ),
        LearningChapter(
            id: "aprs-identity-and-beacons",
            collection: .understand,
            title: "Build an APRS beacon",
            eyebrow: "Identity, position, and policy",
            summary: "Combine callsign/SSID, a valid position, symbol or status, and a responsible beacon trigger.",
            symbol: "point.3.connected.trianglepath.dotted",
            sections: [
                .init(
                    heading: "A beacon answers four questions",
                    body: "The APRS callsign and SSID identify the station, GPS or a stored position says where it is, symbol and status add context, and beacon policy decides when a packet is sent. Configure and verify each part independently.",
                    points: [
                        "Manual beaconing is the simplest way to validate identity and position.",
                        "Interval beaconing trades freshness against channel use and battery drain.",
                        "SmartBeaconing varies reports with movement; review its thresholds before relying on it.",
                        "A GPS fix alone never transmits a packet.",
                    ]
                ),
                .init(
                    heading: "Watch the whole packet path",
                    body: "APRS mode and KISS mode give the host and radio different responsibilities. Confirm which mode owns packet handling before expecting the radio's automatic beacon settings or an app-driven packet to operate.",
                    points: []
                ),
            ],
            relatedGroups: [.aprs, .gps, .connectivity]
        ),
        LearningChapter(
            id: "aprs-messages",
            collection: .operate,
            title: "Send and follow APRS messages",
            eyebrow: "Addressed packet text",
            summary: "Use exact station identity, concise text, and delivery state instead of treating APRS like instant chat.",
            symbol: "message.badge.waveform",
            sections: [
                .init(
                    heading: "Address the station, not a contact card",
                    body: "An APRS message targets a callsign and SSID. Confirm the exact destination seen on the network, keep text concise, and avoid assuming that a transmitted packet reached the recipient.",
                    points: [
                        "Watch acknowledgement and retry state when the workflow exposes it.",
                        "A heard station may be reachable only through a particular packet path or at another time.",
                        "Do not resend rapidly when the channel is busy or delivery is uncertain.",
                    ]
                ),
                .init(
                    heading: "Separate messages from beacons",
                    body: "Changing beacon interval does not control addressed message delivery. Identity, TNC mode, packet path, and acknowledgement behavior matter independently.",
                    points: []
                ),
            ],
            relatedGroups: [.aprs, .connectivity]
        ),
        LearningChapter(
            id: "gps-and-position",
            collection: .understand,
            title: "Turn a GPS fix into useful position data",
            eyebrow: "Location without accidental transmission",
            summary: "Distinguish the GPS receiver, displayed fix, stored position, PC output, and APRS use of that position.",
            symbol: "location.north.line",
            sections: [
                .init(
                    heading: "A fix is a source, not an action",
                    body: "Enabling the built-in GPS starts position acquisition. It does not enable APRS or transmit anything by itself. Wait for a credible fix, then confirm datum, time, and displayed coordinates before another feature consumes them.",
                    points: [
                        "Poor sky view can make the first fix slow or unstable.",
                        "A saved position remains static even when the radio moves.",
                        "PC or NMEA output is a separate setting from the receiver itself.",
                        "Review privacy and local rules before attaching position to a transmitted packet.",
                    ]
                ),
                .init(
                    heading: "Choose the intended consumer",
                    body: "The live display, APRS beaconing, logging, and a connected computer can each use position differently. Enable only the output paths needed for the current task.",
                    points: []
                ),
            ],
            relatedGroups: [.gps, .aprs, .connectivity]
        ),
        LearningChapter(
            id: "dstar-callsigns-and-routing",
            collection: .understand,
            title: "Read a D-STAR route",
            eyebrow: "MYCALL, URCALL, and repeaters",
            summary: "Treat D-STAR callsign fields as routing instructions carried with digital voice, not as ordinary labels.",
            symbol: "signpost.right.and.left",
            sections: [
                .init(
                    heading: "Each callsign field has a job",
                    body: "MYCALL identifies your station. URCALL expresses the destination or general call, while repeater fields describe the local and gateway route. A familiar frequency with stale routing fields can still send a digital call somewhere unintended.",
                    points: [
                        "Use CQCQCQ for a general call only when that is the intended route.",
                        "Confirm the local repeater module and gateway field together.",
                        "Read back all routing fields after recalling a D-STAR memory or DR entry.",
                        "Keep station identity separate from reflector or gateway destination.",
                    ]
                ),
                .init(
                    heading: "Verify before transmitting",
                    body: "The live screen should agree with the route you planned. If it does not, stop and correct the route rather than relying on a memory name or a previous successful call.",
                    points: []
                ),
            ],
            relatedGroups: [.digitalVoice, .memory]
        ),
        LearningChapter(
            id: "dstar-gateway-modes",
            collection: .operate,
            title: "Use D-STAR gateway modes",
            eyebrow: "Terminal, access point, and control state",
            summary: "Understand why a gateway session changes the bytes on the USB link and temporarily displaces normal control.",
            symbol: "waveform.badge.mic",
            sections: [
                .init(
                    heading: "The transport changes roles",
                    body: "In ordinary control mode, the link carries CAT or MCP operations. In a D-STAR terminal or access-point workflow, the same path can carry MMDVM-framed digital voice instead. Normal frequency, screen, or settings commands should not be expected on a link currently owned by gateway traffic.",
                    points: [
                        "Finish or unwind the current gateway session before requesting control operations.",
                        "Allow the radio time to reboot or change modes before reconnecting.",
                        "Re-identify the model, firmware, and capability state after the transition.",
                    ]
                ),
                .init(
                    heading: "Identity still matters",
                    body: "Gateway mode transports frames; it does not replace MYCALL or correct an invalid D-STAR route. Validate station identity and the selected reflector or destination before starting the session.",
                    points: []
                ),
            ],
            relatedGroups: [.digitalVoice, .connectivity, .audio]
        ),
        LearningChapter(
            id: "usb-audio-and-control",
            collection: .understand,
            title: "Choose the USB role",
            eyebrow: "Control, storage, and audio",
            summary: "Know which radio function owns USB before diagnosing a missing control port or unexpected audio stream.",
            symbol: "waveform.and.mic",
            sections: [
                .init(
                    heading: "USB is not one undifferentiated pipe",
                    body: "The radio can expose control and audio behavior or a storage-oriented role. Changing USB function can restart or replace the interface, so an existing session may disappear even though the cable remains attached.",
                    points: [
                        "Select the role required by the task before opening Azimuth's control session.",
                        "After a USB function change, wait for the device to re-enumerate and reconnect by identity.",
                        "Audio source and audio level are separate from the serial control protocol.",
                    ]
                ),
                .init(
                    heading: "AF, IF, and detect are different signals",
                    body: "AF follows listenable audio; IF and detect outputs serve signal-processing workflows and have stricter radio-state requirements. The established control path requires single-band operation on side B for IF or detect output, so verify those prerequisites before treating a refusal as a cable problem.",
                    points: []
                ),
            ],
            relatedGroups: [.connectivity, .audio, .radio]
        ),
        LearningChapter(
            id: "battery-and-power",
            collection: .operate,
            title: "Plan for battery and heat",
            eyebrow: "Power is an operating parameter",
            summary: "Balance receive responsiveness, display use, GPS, transmit power, and automatic shutdown for the session at hand.",
            symbol: "battery.75percent",
            sections: [
                .init(
                    heading: "Every active subsystem has a cost",
                    body: "Bright backlight, GPS, continuous scanning, USB activity, audio, and frequent transmission draw power differently. Start with a charged battery and watch live status during long remote or gateway sessions.",
                    points: [
                        "Battery saver can extend standby time but may change receive responsiveness.",
                        "Auto power off prevents forgotten idle operation but can end an unattended task.",
                        "Higher transmit power increases drain and heat; use only what the path requires.",
                        "External power does not remove the need for ventilation and a stable USB connection.",
                    ]
                ),
                .init(
                    heading: "Match policy to the workflow",
                    body: "A short portable monitoring session, an APRS outing, and a desk-bound gateway session need different saver, backlight, GPS, and shutdown choices. Review them as a group instead of optimizing one setting in isolation.",
                    points: []
                ),
            ],
            relatedGroups: [.radio, .display, .gps, .connectivity]
        ),
        LearningChapter(
            id: "display-and-accessibility",
            collection: .operate,
            title: "Make state easy to read",
            eyebrow: "Display and accessibility",
            summary: "Tune brightness, timeout, color, names, and the remote view so important radio state stays visible.",
            symbol: "textformat.size",
            sections: [
                .init(
                    heading: "Legibility is part of safe operation",
                    body: "Choose brightness and color that make the active side, mode, route, and warning icons readable in the current environment. A short timeout saves power, but it should not hide state during a critical sequence.",
                    points: [
                        "Use concise, distinct memory names that survive the small radio display.",
                        "The iPad or Mac remote surface can enlarge the real frame without inventing state.",
                        "Keep key labels, status badges, and before/after values visible before confirming an action.",
                        "Reduce brightness at night without making warnings or the active-side indicator ambiguous.",
                    ]
                ),
                .init(
                    heading: "Audio cues are independent",
                    body: "Key beeps can provide confirmation when the screen is not the primary cue, but beep enable and volume are separate from received audio. Configure them for the operator and environment without masking live radio audio.",
                    points: []
                ),
            ],
            relatedGroups: [.display, .audio, .memory, .radio]
        ),
        LearningChapter(
            id: "settings-backup-and-write-safety",
            collection: .safety,
            title: "Back up, review, then write",
            eyebrow: "Recoverable configuration",
            summary: "Start from a complete live snapshot, preserve a recovery point, and keep every write as an inspectable diff.",
            symbol: "externaldrive.badge.checkmark",
            sections: [
                .init(
                    heading: "Definitions are not live values",
                    body: "A catalog defines accepted fields and domains. Before editing, Azimuth must verify the connected model and firmware, read a consistent radio snapshot, and show the actual before value. Preserve that snapshot as a recovery point before a broad change.",
                    points: [
                        "Confirm the snapshot source, device identity, and capture time.",
                        "Stage changes so every before and after value can be reviewed together.",
                        "Keep the radio powered and USB stable until the write and verification finish.",
                        "Treat a partial, rejected, stale, or rolled-back result as unfinished work, not silent success.",
                    ]
                ),
                .init(
                    heading: "Verify the result on the radio",
                    body: "After applying, refresh the snapshot and inspect the relevant live screen or behavior. A successful transport call is not the same as proving the intended operating result.",
                    points: []
                ),
            ],
            relatedGroups: RadioSettingGroup.allCases
        ),
        LearningChapter(
            id: "assistant-approval",
            collection: .safety,
            title: "Approve an Assistant proposal",
            eyebrow: "On-device intelligence with human authority",
            summary: "Turn a natural-language request into one validated before-and-after batch, then explicitly Accept or Decline it.",
            symbol: "apple.intelligence",
            sections: [
                .init(
                    heading: "The model proposes; deterministic code validates",
                    body: "Apple's on-device model selects candidate changes, but it cannot write the radio. Azimuth rejects unknown or duplicate IDs, values outside the reviewed domain, specialized binary fields, missing live before values, and no-op-only plans before approval is possible.",
                    points: [
                        "Read every before and target value; the summary is not a substitute for the diff.",
                        "A Needs Clarification proposal cannot be accepted.",
                        "Decline discards the proposal without calling the controller.",
                        "Accept sends one validated batch only after connection and write capability are ready.",
                    ]
                ),
                .init(
                    heading: "Approval is not the end of verification",
                    body: "Azimuth checks that live values have not changed since proposal generation, reports per-setting progress, and preserves failures or rollbacks. Review the final controller report and refreshed radio state before considering the request complete.",
                    points: []
                ),
            ],
            relatedGroups: RadioSettingGroup.allCases
        ),
    ]
}
