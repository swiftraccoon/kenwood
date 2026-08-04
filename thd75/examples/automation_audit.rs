//! Exhaustive, screen-authenticated TH-D75 V1.03 menu audit.
//!
//! The runner derives the 217-leaf menu manifest from the reviewed V1.03 user
//! manual, makes exact V1.03.AZM qualification its first CAT/MCP operation, and then
//! uses only allowlisted context-aware key sequences. Before any numeric keypad
//! route is dispatched, V1.03.AZM captures one fresh CRC-authenticated screen,
//! requires byte equality with the qualified top-level Menu frame, and binds
//! the complete three-digit route to that capture's generation, CRC, command
//! count, seqlock, and a short maximum lease age. One firmware transaction
//! samples the full framebuffer once before any route input, refuses a
//! mismatch with an empty prefix, then synchronously emits all three zero-hold
//! digits with no host turn. The stock radio legitimately redraws its numeric
//! entry state after digit one, so that intended transition is not compared
//! with the original Menu frame. A concurrent framebuffer writer can still
//! change an already-compared word before dispatch; that residual TOCTOU is
//! explicit. On V1.03, the proven
//! complete-number direct-access behavior opens a numeric menu leaf without
//! an additional confirmation key; ordinary value pages are therefore observed by
//! entering all three digits, capturing, and sending `[MODE]` as the next and
//! only key. Fourteen separately reviewed row-only pages are likewise
//! entered with the stock `[A/B]`/`OK` action when direct access lands on a
//! numbered row, validated by a read-only page-specific oracle, and left with
//! one `[MODE]`; 16 destructive/external actions and 25 multi-record/editor pages
//! are located but never entered.
//! Menu 710 is the one stock-V1.03 singleton-submenu exception: `Memory` is the
//! sole reviewed child of the exact `FM Broadcasting` / `71-` page. Activating
//! that selected submenu can enter the FM-radio multi-record list, so its exact
//! title, prefix, 24-pixel selected row, Back/OK controls, and one-leaf manifest
//! relationship are the terminal locator and no activation key is sent.
//!
//! Every key or route receipt, raw-frame CRC, BMP, OCR observation, exact selected-row
//! pixel band, semantic assertion, and restoration is appended to JSONL. To
//! keep all host latency out of numeric entry, evidence for the route lease is
//! persisted only after the guarded transaction returns; there is no host turn
//! at all between its three firmware-dispatched digits.
//! Before normalization or any menu audit, the runner also authenticates a
//! missing-snapshot `GM G` status-02 refusal. Before the first audited menu it
//! captures the top-level Menu, returns to the reviewed masked dual-band home
//! profile, and proves a second status-02 refusal did not reopen Menu. It then
//! dispatches harmless route 991 in one zero-hold command, proves the exact
//! Version / V1.03.AZM page, and restores the same masked home profile.
//! Full-frame equality remains evidence, not a verdict, because the stock home
//! screen contains a clock and volatile status icons. Only that live canary
//! qualifies zero-hold routing for the exhaustive audit.
//! A read-only MCP snapshot of the exact 350 pages spanned by the 400 generated
//! V1.03 menu-field descriptors is byte-compared before and after the audit.
//! The before snapshot also supplies expected values for the safe-inspection
//! pages where a persistent field exists. Every capture owned by the four
//! explicitly recognized high-risk menu audits (516, 651, 935, and 946)
//! represents body and selected OCR text only by SHA-256 in JSONL. That
//! fail-safe decision follows the known audit context, so missing or duplicate
//! title OCR cannot disable it. Other pages and records can still contain
//! incidental callsigns, messages, or device data.
//! The other 1,605 MCP pages and all non-MCP transient/volatile state are
//! explicitly outside that snapshot. Menu 134 has one separately bounded
//! exception: stock firmware refuses its page when the Pri special memory is
//! empty. The runner fsyncs complete owner-private copies of MCP pages 0x0031
//! and 0x00F7, validates stock WX1 as an exact 162.550 MHz FM/simplex donor
//! with its retained special-channel flag byte `0x00`, copies only its 40-byte
//! record and that one flag byte into Pri (data page first, validity byte last),
//! audits only Menu 134, then restores the flag page first and data page last.
//! Both complete pages must read back byte-identical before
//! V1.03.AZM is requalified and the exact home oracle is proved. An already valid Pri
//! is only compared and never written. Long-run restoration uses a reviewed
//! V1.03 dual-band oracle: exact equality outside three volatile full-width
//! row bands and the live RF S-meter rectangle, plus exact ordered
//! frequency/mode OCR text anchors. Vision bounds
//! are retained as evidence but never used for a verdict because repeated live
//! recognition moved them while the stable framebuffer bytes were identical. The runner
//! requalifies V1.03.AZM and applies that oracle after both MCP sessions.
//! Treat the complete JSONL, BMP, and binary-snapshot bundle as private and
//! keep it in an owner-private directory.
//!
//! Menu numbering, layout, and setting semantics remain stock-V1.03
//! compatible. Only the authenticated key transport and framebuffer capture
//! require the custom V1.03.AZM overlay; its `GM` and `GW` collisions are recorded
//! as custom-only exceptions in the session evidence.
//! A full-manifest verdict means all 217 rows were located, 162 value or
//! information pages plus 14 reviewed read-only inspection pages were entered
//! and validated, and 41 destructive or multi-record/editor leaves were only
//! located. It never represents those 41 current values as audited.
//!
//! ```text
//! cargo run -p kenwood-thd75 --release --example automation_audit -- \
//!   --port /dev/cu.usbmodem1234 --output-dir /private/path/new-audit-directory
//! cargo run -p kenwood-thd75 --release --example automation_audit -- \
//!   --device TH-D75 --output-dir /private/path/new-audit-directory
//! cargo run -p kenwood-thd75 --release --example automation_audit -- \
//!   --port /dev/cu.usbmodem1234 --output-dir /private/path/smoke --menu 991
//! ```

// Keep every workspace example dependency represented under the workspace's
// strict unused-dependency lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::BTreeSet;
    use std::error::Error as StdError;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, BufWriter, Write};
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use kenwood_thd75::Radio;
    use kenwood_thd75::memory::{
        DecodedFieldValue, FieldCodec, MCP_D75_MENU_FIELDS, MCP_D75_SCHEMA_FIRMWARE,
        MCP_D75_SCHEMA_MODEL, MCP_D75_SCHEMA_VERSION, MCP_D75_SOURCE_SHA256, MenuField, menu_field,
    };
    use kenwood_thd75::protocol::programming;
    use kenwood_thd75::radio::automation::{
        AutomationMetadata, AutomationSession, AutomationSnapshot, FrontPanelKey,
        GUARDED_ROUTE_MAX_DURATION, GUARDED_SNAPSHOT_MAX_AGE, GuardedDecimalRoute,
        GuardedDecimalRouteOutcome, GuardedKeyOutcome, GuardedKeyResult,
    };
    use kenwood_thd75::radio::programming::{McpPage, McpPageExchange, WritableMcpPage};
    use kenwood_thd75::screen::SCREEN_WIDTH;
    use kenwood_thd75::screen::ScreenFrame;
    use kenwood_thd75::screen::ui::{
        CheckboxState, selected_text, v103_checkbox_state, v103_selected_checkbox,
        v103_selection_bands,
    };
    use kenwood_thd75::screen::vision::{NormalizedBounds, TextObservation, require_unique_text};
    use kenwood_thd75::transport::{
        BluetoothTransport, EitherTransport, SerialTransport, Transport,
    };
    use kenwood_thd75::types::{
        Band, BandMode, ChannelMode, ShiftDirection, StoredChannel, TuningMode,
    };
    use serde_json::{Value, json};

    type AuditError = Box<dyn StdError + Send + Sync>;
    type AuditResult<T> = Result<T, AuditError>;

    const REVIEWED_MANUAL: &str = include_str!("../data/automation_menu_manifest.txt");
    const SETTLE_DELAY: Duration = Duration::from_millis(140);
    const QUIESCENCE_DELAY: Duration = Duration::from_millis(100);
    const EXPECTED_MENU_COUNT: usize = 217;
    const EXPECTED_CATEGORY_COUNT: usize = 9;
    const MIN_OCR_CONFIDENCE: f32 = 0.90;
    const SCREEN_WIDTH_F32: f32 = 240.0;
    const SCREEN_HEIGHT_F32: f32 = 180.0;
    const EXPECTED_CONFIGURATION_SNAPSHOT_FIELD_COUNT: usize = 400;
    const EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT: usize = 350;
    const EXPECTED_MCP_TOTAL_PAGE_COUNT: usize = 1955;
    const EXPECTED_VALUE_OR_INFORMATION_COUNT: usize = 162;
    const EXPECTED_ROW_ONLY_COUNT: usize = 55;
    const EXPECTED_SAFE_INSPECTION_COUNT: usize = 14;
    const EXPECTED_LOCATED_NOT_ENTERED_COUNT: usize = 41;
    const MENU_134_FLAG_PAGE: u16 = 0x0031;
    const MENU_134_DATA_PAGE: u16 = 0x00F7;
    const MENU_134_PRI_CHANNEL: u16 = 1100;
    const MENU_134_WX1_CHANNEL: u16 = 1101;
    const MENU_134_PRI_FLAG_OFFSET: usize = 0x30;
    const MENU_134_WX1_FLAG_OFFSET: usize = 0x34;
    const MENU_134_PRI_RECORD_OFFSET: usize = 0x50;
    const MENU_134_WX1_RECORD_OFFSET: usize = 0x78;
    const MENU_134_WX1_RX_HZ: u32 = 162_550_000;
    const MENU_134_BAND_B_RX_RANGE_HZ: std::ops::RangeInclusive<u32> = 100_000..=523_995_000;
    const CONFIGURATION_SNAPSHOT_SCOPE: &str = "exact-final-byte-equality-of-350-full-pages-spanned-by-400-MCP_D75_MENU_FIELDS-descriptors; excludes-1605-other-MCP-pages-and-all-non-MCP-transient-or-volatile-state";
    const HOME_MASK_ID: &str = "th-d75-v1.03-dual-band-home-v1";
    const HOME_MASK_EXCLUDED_ROWS: [(usize, usize); 3] = [(0, 20), (31, 45), (131, 145)];
    const HOME_MASK_SIGNAL_METER_RECT: (usize, usize, usize, usize) = (0, 90, 151, 11);
    const HOME_MASK_INCLUDED_PIXELS: usize = 30_019;
    const HOME_MASK_EXCLUDED_PIXELS: usize = 13_181;
    const SHA256_ROUND_CONSTANTS: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    const CTCSS_FREQUENCIES: [&str; 50] = [
        "67.0", "69.3", "71.9", "74.4", "77.0", "79.7", "82.5", "85.4", "88.5", "91.5", "94.8",
        "97.4", "100.0", "103.5", "107.2", "110.9", "114.8", "118.8", "123.0", "127.3", "131.8",
        "136.5", "141.3", "146.2", "151.4", "156.7", "159.8", "162.2", "165.5", "167.9", "171.3",
        "173.8", "177.3", "179.9", "183.5", "186.2", "189.9", "192.8", "196.6", "199.5", "203.5",
        "206.5", "210.7", "218.1", "225.7", "229.1", "233.6", "241.8", "250.3", "254.1",
    ];
    // Exact stock V1.03 Menu 501 display strings. The manual's abridged table
    // omits Fire Truck, RACES, and ARRL; all 68 are present in the stock
    // firmware's fixed-width icon-name table.
    const APRS_ICON_NAMES: [&str; 68] = [
        "Person",
        "Bicycle",
        "Motorcycle",
        "Car",
        "Bus",
        "Railroad Engine",
        "Home",
        "Yagi@QTH",
        "KENWOOD",
        "Radio",
        "RV",
        "Van",
        "Jeep",
        "Truck",
        "Truck(18-wheeler)",
        "Police",
        "Ambulance",
        "Fire Truck",
        "Canoe",
        "Boat",
        "Sailboat",
        "Balloon",
        "Glider",
        "Helicopter",
        "Aircraft",
        "Large Aircraft",
        "Shuttle",
        "Satellite",
        "Rover",
        "Eyeball",
        "Portable (Tent)",
        "HAM Store",
        "School",
        "Hospital",
        "Red Cross",
        "Lighthouse",
        "Speed Signpost",
        "WorkZone",
        "Wreck/Obstruction",
        "Sheriff",
        "Fire",
        "Sunny",
        "Gale Flags",
        "Tornado",
        "National WX Service",
        "WX(Weather station)",
        "Digipeater",
        "Mic-E Repeater",
        "QSO Repeater",
        "Circle",
        "IRLP",
        "EchoLink",
        "Node",
        "GATEway",
        "DF station",
        "Dish Antenna",
        "PC user",
        "SSTV",
        "ATV",
        "BBS",
        "APRStt",
        "RACES",
        "ARRL",
        "X",
        "Triangle",
        "Small Circle",
        "Red Dot",
        "Big Question Mark",
    ];
    const POSITION_COMMENTS: [&str; 15] = [
        "Off Duty",
        "Enroute",
        "In Service",
        "Returning",
        "Committed",
        "Special",
        "PRIORITY",
        "CUSTOM0",
        "CUSTOM1",
        "CUSTOM2",
        "CUSTOM3",
        "CUSTOM4",
        "CUSTOM5",
        "CUSTOM6",
        "EMERGENCY!",
    ];

    // V: ordinary value page. G: guarded/high-impact value page. I:
    // information page. R: row-only editor/list/action page. This partition
    // was reviewed against the V1.03 manual; its tests require exact coverage
    // of all 217 menu leaves with no overlap.
    const VALUE_NUMBERS: &str = "101 103 104 105 111 112 120 121 122 130 131 132 133 135 140 141 142 151 152 160 161 170 171 181 202 302 311 402 404 406 412 413 414 501 505 506 507 508 530 531 532 533 534 535 540 541 542 550 551 563 570 571 573 574 575 593 611 615 616 617 618 620 621 640 641 642 643 644 645 701 900 901 902 904 905 906 907 910 912 913 914 915 916 917 918 919 91A 920 940 941 942 943 944 945 970 971 972 973 974";
    const GUARDED_NUMBERS: &str = "102 110 134 136 143 150 153 162 180 301 400 403 405 410 502 509 510 511 512 513 514 515 520 521 522 523 561 580 581 582 584 586 587 590 591 592 612 613 614 619 630 631 632 650 700 921 923 930 936 960 961 962 963 980 981 982 983 984 985 990";
    const INFORMATION_NUMBERS: &str = "840 922 991";
    const ROW_ONLY_NUMBERS: &str = "100 163 164 200 201 203 204 210 220 230 300 310 312 401 411 500 503 504 516 560 562 564 572 583 585 588 594 595 600 610 651 652 653 654 710 800 801 802 803 810 811 812 813 820 830 903 911 931 932 933 934 935 946 950 999";
    const SAFE_INSPECTION_NUMBERS: &str = "100 401 500 503 504 516 562 572 585 588 651 911 935 950";
    const DESTRUCTIVE_ACTION_NUMBERS: &str =
        "411 800 801 802 803 810 811 812 813 820 830 931 932 933 934 999";
    const MULTI_RECORD_EDITOR_NUMBERS: &str = "163 164 200 201 203 204 210 220 230 300 310 312 560 564 583 594 595 600 610 652 653 654 710 903 946";
    const SPECIALIZED_PAYLOAD_NUMBERS: &str = "181 406 509 530 551 591 631 840 912 913 922";
    // These stock V1.03 scalar editors render one centered current value and
    // no blue selection band. Their read-only evidence is the exact page
    // title, one typed value locus in the body, the Back control, and the
    // subsequent MODE transition to the exact numbered row.
    const CENTERED_SCALAR_NUMBERS: &str =
        "120 121 122 132 133 140 170 413 414 523 531 532 533 534 535 550 581 593 615 621 901 91A";
    const FILTER_TYPE_ROWS: [&str; 7] = [
        "Weather",
        "Digipeater",
        "Mobile",
        "Object/Item",
        "NAVITRA",
        "1-Way",
        "Others",
    ];
    const TOP_MENU_CATEGORY_LABELS: [&str; 11] = [
        "TX/RX",
        "MEM",
        "Audio File",
        "GPS",
        "APRS",
        "Digital",
        "FM Broadcasting",
        "FM Radio",
        "SD Card",
        "microSD",
        "Configuration",
    ];
    const FRONT_PF_ASSIGNMENTS: &[&str] = &[
        "Recording",
        "Voice Message 1",
        "Voice Message 2",
        "Voice Message 3",
        "Voice Message 4",
        "Voice Guidance",
        "Battery Level",
        "VOX",
        "Group Name",
        "Balance",
        "GPS",
        "Track LOG",
        "SQL",
        "SHIFT",
        "STEP",
        "LOW",
        "Key Lock",
        "Lockout",
        "M>V",
        "T. SEL",
        "NEW",
        "Voice Alert",
        "LCD Brightness",
        "DTMF CH0",
        "EchoLink CH0",
        "1750Hz Tone",
        "M. IN",
    ];
    const MIC_PF_ASSIGNMENTS: &[&str] = &[
        "Recording",
        "Voice Message 1",
        "Voice Message 2",
        "Voice Message 3",
        "Voice Message 4",
        "Voice Guidance",
        "Battery Level",
        "VOX",
        "Group Name",
        "Balance",
        "GPS",
        "Track LOG",
        "SQL",
        "SHIFT",
        "STEP",
        "LOW",
        "Key Lock",
        "Lockout",
        "M>V",
        "T. SEL",
        "NEW",
        "Voice Alert",
        "LCD Brightness",
        "DTMF CH0",
        "EchoLink CH0",
        "1750Hz Tone",
        "Screen Capture",
        "MODE",
        "MENU",
        "A/B",
        "VFO",
        "MR",
        "CALL",
        "MSG",
        "LIST",
        "BCON",
        "REV",
        "TONE",
        "MHz",
        "MARK",
        "DUAL",
        "APRS",
        "OBJ",
        "ATT",
        "FINE",
        "POS",
        "BAND",
        "MONI",
        "UP",
        "DOWN",
    ];
    const DV_GPS_SENTENCE_ROWS: [&str; 7] = [
        "$GPGGA",
        "$GPGLL",
        "$GPGSA",
        "$GPGSV",
        "$GPRMC",
        "$GPVTG",
        "APRS Sentence",
    ];
    const MY_POSITION_ROWS: [&str; 6] = [
        "My Position 1",
        "My Position 2",
        "My Position 3",
        "My Position 4",
        "My Position 5",
        "GPS",
    ];
    // Exact stock V1.03 active-row rendering. The manual inserts a space
    // before the status-text number and describes packet-path values without
    // their on-screen `Type:` prefix; the read-only pages do neither.
    const STATUS_TEXT_ROWS: [&str; 5] = [
        "Status Text1",
        "Status Text2",
        "Status Text3",
        "Status Text4",
        "Status Text5",
    ];
    const PACKET_PATH_ROWS: [&str; 6] = [
        "Type: New-N",
        "Type: Relay",
        "Type: Region",
        "Type: Others1",
        "Type: Others2",
        "Type: Others3",
    ];
    const OBJECT_ROWS: [&str; 3] = ["Object1", "Object2", "Object3"];

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Endpoint {
        Bluetooth(String),
        Usb(String),
    }

    impl Endpoint {
        const fn kind(&self) -> &'static str {
            match self {
                Self::Bluetooth(_) => "bluetooth",
                Self::Usb(_) => "usb-cdc",
            }
        }

        fn device_name(&self) -> Option<&str> {
            match self {
                Self::Bluetooth(device_name) => Some(device_name),
                Self::Usb(_) => None,
            }
        }

        fn port(&self) -> Option<&str> {
            match self {
                Self::Bluetooth(_) => None,
                Self::Usb(port) => Some(port),
            }
        }

        const fn pre_mcp_transport_policy(&self) -> PreMcpTransportPolicy {
            match self {
                Self::Bluetooth(_) => PreMcpTransportPolicy::ReuseQualifiedLink,
                Self::Usb(_) => PreMcpTransportPolicy::ReopenUsbCdcAndIdentify,
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PreMcpTransportPolicy {
        ReuseQualifiedLink,
        ReopenUsbCdcAndIdentify,
    }

    impl PreMcpTransportPolicy {
        const fn action(self) -> &'static str {
            match self {
                Self::ReuseQualifiedLink => "reuse-qualified-link",
                Self::ReopenUsbCdcAndIdentify => "close-reopen-and-prove-exact-identity",
            }
        }
    }

    #[derive(Debug)]
    struct Config {
        endpoint: Endpoint,
        output_dir: PathBuf,
        only_menu: Option<String>,
        start_menu: Option<String>,
        limit: Option<usize>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum AuditClass {
        Value,
        Guarded,
        Information,
        RowOnly,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RowOnlyPolicy {
        SafeInspection,
        DestructiveAction,
        MultiRecordEditor,
    }

    impl RowOnlyPolicy {
        const fn as_str(self) -> &'static str {
            match self {
                Self::SafeInspection => "safe-inspection",
                Self::DestructiveAction => "destructive-or-external-action-located-not-entered",
                Self::MultiRecordEditor => "multi-record-or-editor-located-not-entered",
            }
        }

        const fn is_located_not_entered(self) -> bool {
            !matches!(self, Self::SafeInspection)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SafeInspectionOracle {
        ProgrammableVfo,
        ActiveChoice {
            field: &'static str,
            labels: &'static [&'static str],
        },
        ShortText {
            field: &'static str,
            blank_display: Option<&'static str>,
        },
        DvGatewayCallsign,
        EqualizerCheckboxes,
        BluetoothInformation,
        DynamicDateTime,
    }

    impl AuditClass {
        const fn as_str(self) -> &'static str {
            match self {
                Self::Value => "value",
                Self::Guarded => "guarded-value",
                Self::Information => "information",
                Self::RowOnly => "row-only",
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum CoverageScope {
        FullManifest,
        SingleMenu,
        PartialManifest,
    }

    impl CoverageScope {
        const fn as_str(self) -> &'static str {
            match self {
                Self::FullManifest => "full-217-menu-manifest",
                Self::SingleMenu => "single-menu",
                Self::PartialManifest => "partial-manifest",
            }
        }

        const fn pass_label(self) -> &'static str {
            match self {
                Self::FullManifest => "FULL_217_ROWS_162_VALUES_14_SAFE_INSPECTIONS_PASS",
                Self::SingleMenu | Self::PartialManifest => "SCOPED_PASS",
            }
        }
    }

    #[derive(Debug, Clone)]
    struct MenuEntry {
        number: String,
        label: String,
        category_path: String,
        description: String,
        setting_values: String,
        class: AuditClass,
    }

    #[derive(Debug, Clone)]
    struct EvidenceScope {
        coverage: CoverageScope,
        manifest_total: usize,
        selected_total: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ConfigurationSnapshot {
        pages: Vec<(u16, [u8; programming::PAGE_SIZE])>,
        sha256: [u8; 32],
        artifact: String,
    }

    #[derive(Debug, Clone)]
    struct HomeTextAnchor {
        canonical: String,
        bounds: NormalizedBounds,
    }

    #[derive(Debug)]
    struct HomeComparison {
        full_differing_pixels: usize,
        masked_differing_pixels: usize,
        baseline_masked_sha256: String,
        candidate_masked_sha256: String,
        frequency_anchors: Vec<HomeTextAnchor>,
        stable_anchors: Vec<HomeTextAnchor>,
        semantic_profile_valid: bool,
    }

    impl HomeComparison {
        const fn restored(&self) -> bool {
            self.masked_differing_pixels == 0 && self.semantic_profile_valid
        }
    }

    #[derive(Debug, Clone)]
    enum ValueDomain {
        ExactChoices(Vec<String>),
        DocumentedChoices {
            choices: Vec<String>,
            units: Vec<String>,
        },
        DiscreteWithSuffix {
            choices: Vec<String>,
            suffixes: &'static [&'static str],
        },
        Integer {
            minimum: u16,
            maximum: u16,
            width: Option<usize>,
            prefix: Option<&'static str>,
            suffixes: &'static [&'static str],
        },
        IndexedOpaqueChoices {
            minimum: u8,
            maximum: u8,
        },
        Hundredths {
            minimum: u16,
            maximum: u16,
            suffixes: &'static [&'static str],
        },
        OffsetFrequency,
        DistanceLimit,
        FrontAssignment,
        MicrophoneAssignment,
        Specialized,
    }

    #[derive(Debug)]
    struct Journal {
        writer: BufWriter<File>,
        record_index: u64,
        capture_index: u64,
        evidence_scope: EvidenceScope,
        active_menu_number: Option<String>,
    }

    impl Journal {
        fn create(output_dir: &Path, evidence_scope: EvidenceScope) -> AuditResult<Self> {
            let path = output_dir.join("audit.jsonl");
            let mut options = OpenOptions::new();
            let configured = options.write(true).create_new(true);
            #[cfg(unix)]
            let configured = configured.mode(0o600);
            Ok(Self {
                writer: BufWriter::new(configured.open(path)?),
                record_index: 0,
                capture_index: 0,
                evidence_scope,
                active_menu_number: None,
            })
        }

        fn append(&mut self, mut value: Value) -> AuditResult<()> {
            let object = value
                .as_object_mut()
                .ok_or_else(|| io::Error::other("audit record must be a JSON object"))?;
            let menu_number = object
                .get("menu_number")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| self.active_menu_number.clone());
            if !object.contains_key("menu_number")
                && let Some(menu_number) = menu_number.as_deref()
            {
                drop(object.insert("menu_number".to_owned(), json!(menu_number)));
            }
            let menu_compatibility = menu_number.as_deref().map_or("not-applicable", |number| {
                if number == "980" {
                    "stock-v1.03-number-title-and-schema; custom-automation-usb-storage-apply-path"
                } else {
                    "stock-v1.03-compatible"
                }
            });
            let value_kind = menu_number
                .as_deref()
                .map_or("not-applicable", menu_value_kind);
            drop(object.insert(
                "evidence_scope".to_owned(),
                json!({
                    "running_firmware": "custom-automation-overlay",
                    "underlying_firmware_base": "exact-stock-v1.03",
                    "automation_transport_and_frame_capture": "custom-automation-only",
                    "menu_schema": "stock-v1.03-compatible",
                    "mcp_configuration_offsets": "stock-v1.03-compatible",
                    "absolute_runtime_addresses": "custom-automation-build-specific",
                    "hardware": "TH-D75A-qualified",
                    "region": "unqualified",
                    "coverage": self.evidence_scope.coverage.as_str(),
                    "manifest_total": self.evidence_scope.manifest_total,
                    "selected_total": self.evidence_scope.selected_total,
                    "persistent_nonmutation_scope": CONFIGURATION_SNAPSHOT_SCOPE,
                    "menu_compatibility": menu_compatibility,
                    "value_kind": value_kind,
                }),
            ));
            drop(object.insert("schema_version".to_owned(), json!(2)));
            drop(object.insert("record_index".to_owned(), json!(self.record_index)));
            drop(object.insert(
                "timestamp_unix_ms".to_owned(),
                json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()),
            ));
            serde_json::to_writer(&mut self.writer, &value)?;
            self.writer.write_all(b"\n")?;
            self.writer.flush()?;
            self.record_index = self.record_index.saturating_add(1);
            Ok(())
        }

        fn next_capture_name(&mut self, suffix: &str) -> String {
            let name = format!("{:05}-{suffix}.bmp", self.capture_index);
            self.capture_index = self.capture_index.saturating_add(1);
            name
        }

        fn set_active_menu(&mut self, menu_number: Option<&str>) {
            self.active_menu_number = menu_number.map(str::to_owned);
        }

        fn sync_all(&mut self) -> AuditResult<()> {
            self.writer.flush()?;
            self.writer.get_ref().sync_all()?;
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct CapturedScreen {
        frame: ScreenFrame,
        observations: Vec<TextObservation>,
        selected: Vec<String>,
        crc32: u32,
    }

    #[derive(Debug)]
    struct InitialHomeState {
        dual_band_baseline: CapturedScreen,
        operation_band: Band,
        single_band_baseline: Option<CapturedScreen>,
    }

    #[derive(Debug)]
    struct NumericRouteEvidence {
        snapshot: AutomationSnapshot,
        route: GuardedDecimalRoute,
        requested_keys: Vec<FrontPanelKey>,
        capture_started_unix_ms: u128,
        capture_round_trip: Duration,
        dispatch_elapsed: Duration,
        outcome: GuardedDecimalRouteOutcome,
    }

    #[derive(Debug, Default)]
    struct Summary {
        attempted: usize,
        located_rows: usize,
        value_or_information_validated: usize,
        row_only_safe_inspected: usize,
        row_only_located_not_entered: usize,
        restored: usize,
        inconclusive: usize,
    }

    #[derive(Debug, Clone, Copy)]
    struct TransientRadioState {
        original_band: Band,
        original_band_a_tuning_mode: TuningMode,
        normalized_for_menu_100: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Menu134PriDisposition {
        ExistingValid,
        StagedFromStockWx1,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Menu134PriPlan {
        flag_before: [u8; programming::PAGE_SIZE],
        data_before: [u8; programming::PAGE_SIZE],
        flag_staged: [u8; programming::PAGE_SIZE],
        data_staged: [u8; programming::PAGE_SIZE],
        disposition: Menu134PriDisposition,
    }

    impl Menu134PriPlan {
        const fn temporary_write_required(&self) -> bool {
            matches!(self.disposition, Menu134PriDisposition::StagedFromStockWx1)
        }

        fn setup_exchanges(&self) -> AuditResult<[McpPageExchange; 2]> {
            Ok([
                McpPageExchange::new(
                    WritableMcpPage::new(MENU_134_DATA_PAGE)?,
                    self.data_before,
                    self.data_staged,
                ),
                McpPageExchange::new(
                    WritableMcpPage::new(MENU_134_FLAG_PAGE)?,
                    self.flag_before,
                    self.flag_staged,
                ),
            ])
        }

        fn direct_restore_exchanges(&self) -> AuditResult<[McpPageExchange; 2]> {
            Ok([
                McpPageExchange::new(
                    WritableMcpPage::new(MENU_134_FLAG_PAGE)?,
                    self.flag_staged,
                    self.flag_before,
                ),
                McpPageExchange::new(
                    WritableMcpPage::new(MENU_134_DATA_PAGE)?,
                    self.data_staged,
                    self.data_before,
                ),
            ])
        }
    }

    struct Menu134AuditOutcome {
        primary: AuditResult<()>,
        cleanup: AuditResult<()>,
    }

    fn menu_134_page_bytes<const N: usize>(
        page: &[u8; programming::PAGE_SIZE],
        offset: usize,
        description: &str,
    ) -> AuditResult<[u8; N]> {
        page.get(offset..offset.saturating_add(N))
            .ok_or_else(|| {
                invalid_input(format!(
                    "Menu 134 {description} at page offset 0x{offset:02X} exceeds one MCP page"
                ))
            })?
            .try_into()
            .map_err(|_| invalid_input(format!("Menu 134 {description} had an invalid length")))
    }

    const fn menu_134_is_recognized_programmed_flag(flag: u8) -> bool {
        matches!(
            flag,
            programming::FLAG_VHF | programming::FLAG_220 | programming::FLAG_UHF
        )
    }

    fn require_menu_134_priority_scan_off(priority_scan: bool) -> AuditResult<()> {
        if priority_scan {
            Err(io::Error::other(
                "Menu 134 prerequisite refuses to run while Priority Scan is already On",
            )
            .into())
        } else {
            Ok(())
        }
    }

    fn validate_menu_134_existing_pri(
        flag: [u8; programming::FLAG_RECORD_SIZE],
        record: &[u8; programming::CHANNEL_RECORD_SIZE],
    ) -> AuditResult<()> {
        let channel = StoredChannel::from_bytes(record)?;
        if !MENU_134_BAND_B_RX_RANGE_HZ.contains(&channel.receive_frequency.as_hz()) {
            return Err(io::Error::other(format!(
                "programmed Pri channel RX frequency {} Hz is outside documented Band-B receive coverage",
                channel.receive_frequency.as_hz()
            ))
            .into());
        }
        if !menu_134_is_recognized_programmed_flag(flag[0]) {
            return Err(io::Error::other(format!(
                "programmed Pri channel flag byte 0x{:02X} is not a recognized non-empty value",
                flag[0]
            ))
            .into());
        }
        Ok(())
    }

    fn validate_menu_134_wx1_donor(
        flag: [u8; programming::FLAG_RECORD_SIZE],
        record: &[u8; programming::CHANNEL_RECORD_SIZE],
    ) -> AuditResult<()> {
        if flag[0] != programming::FLAG_VHF {
            return Err(io::Error::other(format!(
                "stock WX1 channel {MENU_134_WX1_CHANNEL} special-channel flag byte was 0x{:02X}, expected retained hardware value 0x{:02X}",
                flag[0],
                programming::FLAG_VHF
            ))
            .into());
        }
        let channel = StoredChannel::from_bytes(record)?;
        if channel.receive_frequency.as_hz() != MENU_134_WX1_RX_HZ
            || channel.mode != ChannelMode::Fm
            || channel.split
            || channel.shift != ShiftDirection::Simplex
            || channel.transmit_offset_or_frequency.as_hz() != 0
        {
            return Err(io::Error::other(format!(
                "stock WX1 donor did not match the retained receive-only fixture (RX {} Hz, mode {}, split {}, shift {:?}, offset {} Hz)",
                channel.receive_frequency.as_hz(),
                channel.mode,
                channel.split,
                channel.shift,
                channel.transmit_offset_or_frequency.as_hz()
            ))
            .into());
        }
        Ok(())
    }

    fn plan_menu_134_pri_pages(
        flag_before: [u8; programming::PAGE_SIZE],
        data_before: [u8; programming::PAGE_SIZE],
    ) -> AuditResult<Menu134PriPlan> {
        let pri_flag = menu_134_page_bytes::<{ programming::FLAG_RECORD_SIZE }>(
            &flag_before,
            MENU_134_PRI_FLAG_OFFSET,
            "Pri flag",
        )?;
        let pri_record = menu_134_page_bytes::<{ programming::CHANNEL_RECORD_SIZE }>(
            &data_before,
            MENU_134_PRI_RECORD_OFFSET,
            "Pri channel record",
        )?;
        if pri_flag[0] != programming::FLAG_EMPTY {
            validate_menu_134_existing_pri(pri_flag, &pri_record)?;
            return Ok(Menu134PriPlan {
                flag_staged: flag_before,
                data_staged: data_before,
                flag_before,
                data_before,
                disposition: Menu134PriDisposition::ExistingValid,
            });
        }

        let wx1_flag = menu_134_page_bytes::<{ programming::FLAG_RECORD_SIZE }>(
            &flag_before,
            MENU_134_WX1_FLAG_OFFSET,
            "stock WX1 flag",
        )?;
        let wx1_record = menu_134_page_bytes::<{ programming::CHANNEL_RECORD_SIZE }>(
            &data_before,
            MENU_134_WX1_RECORD_OFFSET,
            "stock WX1 channel record",
        )?;
        validate_menu_134_wx1_donor(wx1_flag, &wx1_record)?;

        let mut flag_staged = flag_before;
        flag_staged[MENU_134_PRI_FLAG_OFFSET] = wx1_flag[0];
        let mut data_staged = data_before;
        data_staged[MENU_134_PRI_RECORD_OFFSET
            ..MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
            .copy_from_slice(&wx1_record);

        Ok(Menu134PriPlan {
            flag_before,
            data_before,
            flag_staged,
            data_staged,
            disposition: Menu134PriDisposition::StagedFromStockWx1,
        })
    }

    fn plan_menu_134_restore_pages(
        plan: &Menu134PriPlan,
        live_flag: [u8; programming::PAGE_SIZE],
        live_data: [u8; programming::PAGE_SIZE],
    ) -> AuditResult<[McpPageExchange; 2]> {
        if live_flag != plan.flag_before && live_flag != plan.flag_staged {
            return Err(io::Error::other(
                "Menu 134 flag page changed outside the exact before/staged states; refusing cleanup write",
            )
            .into());
        }
        if live_data != plan.data_before && live_data != plan.data_staged {
            return Err(io::Error::other(
                "Menu 134 data page changed outside the exact before/staged states; refusing cleanup write",
            )
            .into());
        }
        Ok([
            McpPageExchange::new(
                WritableMcpPage::new(MENU_134_FLAG_PAGE)?,
                live_flag,
                plan.flag_before,
            ),
            McpPageExchange::new(
                WritableMcpPage::new(MENU_134_DATA_PAGE)?,
                live_data,
                plan.data_before,
            ),
        ])
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the top-level audit coordinator keeps its evidence-scope counts and final session record together"
    )]
    #[expect(
        clippy::redundant_pub_crate,
        reason = "the private platform module must expose its entry point to the parent binary module"
    )]
    pub(super) async fn run() -> AuditResult<()> {
        let config = parse_args()?;
        let all_entries = parse_menu_manifest(REVIEWED_MANUAL)?;
        validate_manifest(&all_entries)?;
        let entries = select_entries(&all_entries, &config)?;
        let coverage = coverage_scope(&all_entries, &entries);
        let expected_value_total = entries
            .iter()
            .filter(|entry| entry.class != AuditClass::RowOnly)
            .count();
        let expected_safe_inspection_total = entries
            .iter()
            .filter(|entry| {
                entry.class == AuditClass::RowOnly
                    && matches!(
                        row_only_policy(&entry.number),
                        Ok(RowOnlyPolicy::SafeInspection)
                    )
            })
            .count();
        let expected_located_not_entered_total = entries
            .len()
            .saturating_sub(expected_value_total)
            .saturating_sub(expected_safe_inspection_total);
        let snapshot_pages = configuration_snapshot_pages()?;
        prepare_output_dir(&config.output_dir)?;
        let mut journal = Journal::create(
            &config.output_dir,
            EvidenceScope {
                coverage,
                manifest_total: all_entries.len(),
                selected_total: entries.len(),
            },
        )?;

        record_session_start(
            &mut journal,
            &config,
            all_entries.len(),
            entries.len(),
            expected_value_total,
            expected_safe_inspection_total,
            expected_located_not_entered_total,
            coverage,
        )?;

        let transport_started = Instant::now();
        let transport = open_transport(&config.endpoint)?;
        journal.append(json!({
            "type": "transport-open",
            "transport": config.endpoint.kind(),
            "device_name": config.endpoint.device_name(),
            "port": config.endpoint.port(),
            "elapsed_ms": millis(transport_started.elapsed()),
            "transport_policy": match &config.endpoint {
                Endpoint::Bluetooth(_) => "library-owned-two-attempt-maximum; one fresh-helper retry only after NotFound",
                Endpoint::Usb(_) => "exact-explicit-serial-path-at-default-USB-CDC-baud",
            },
            "daemon_or_global_bluetooth_process_manipulated": false,
            "result": "pass",
        }))?;
        let mut radio = Radio::new(transport);
        let mut summary = Summary::default();
        let pre_mcp_transport_policy = config.endpoint.pre_mcp_transport_policy();
        let result = execute_audit(
            &mut radio,
            pre_mcp_transport_policy,
            &config.output_dir,
            &entries,
            expected_value_total,
            expected_safe_inspection_total,
            expected_located_not_entered_total,
            &snapshot_pages,
            &mut journal,
            &mut summary,
        )
        .await;

        journal.set_active_menu(None);
        let disconnect_result = radio.disconnect().await;
        journal.append(json!({
            "type": "session-end",
            "result": if result.is_ok() && disconnect_result.is_ok() {
                coverage.pass_label()
            } else {
                "FAIL"
            },
            "coverage": coverage.as_str(),
            "summary": {
                "attempted": summary.attempted,
                "located_rows": summary.located_rows,
                "value_or_information_validated": summary.value_or_information_validated,
                "row_only_safe_inspected": summary.row_only_safe_inspected,
                "row_only_located_not_entered": summary.row_only_located_not_entered,
                "restored": summary.restored,
                "inconclusive": summary.inconclusive,
                "selected_total": entries.len(),
                "expected_value_or_information_total": expected_value_total,
                "expected_safe_inspection_total": expected_safe_inspection_total,
                "expected_located_not_entered_total": expected_located_not_entered_total,
            },
            "error": result.as_ref().err().map(ToString::to_string),
            "disconnect_error": disconnect_result.as_ref().err().map(ToString::to_string),
        }))?;
        let disconnect_cleanup =
            disconnect_result.map_err(|error| -> AuditError { Box::new(error) });
        combine_primary_and_cleanup_errors(result, [("radio-disconnect", disconnect_cleanup)])?;

        println!(
            "audit={} coverage:{} selected:{} attempted:{} located_rows:{} value_or_information_validated:{}/{} row_only_safe_inspected:{}/{} row_only_located_not_entered:{}/{} restored:{} inconclusive:{}",
            coverage.pass_label(),
            coverage.as_str(),
            entries.len(),
            summary.attempted,
            summary.located_rows,
            summary.value_or_information_validated,
            expected_value_total,
            summary.row_only_safe_inspected,
            expected_safe_inspection_total,
            summary.row_only_located_not_entered,
            expected_located_not_entered_total,
            summary.restored,
            summary.inconclusive
        );
        println!("evidence_dir={}", config.output_dir.display());
        Ok(())
    }

    fn record_session_start(
        journal: &mut Journal,
        config: &Config,
        manifest_total: usize,
        selected_total: usize,
        expected_value_total: usize,
        expected_safe_inspection_total: usize,
        expected_located_not_entered_total: usize,
        coverage: CoverageScope,
    ) -> AuditResult<()> {
        journal.append(json!({
            "type": "session-start",
            "transport": config.endpoint.kind(),
            "device_name": config.endpoint.device_name(),
            "port": config.endpoint.port(),
            "output_dir": config.output_dir,
            "manifest_total": manifest_total,
            "selected_total": selected_total,
            "expected_value_or_information_total": expected_value_total,
            "expected_row_only_safe_inspection_total": expected_safe_inspection_total,
            "expected_row_only_located_not_entered_total": expected_located_not_entered_total,
            "coverage": coverage.as_str(),
            "compatibility": {
                "running_firmware": "custom-automation-overlay",
                "underlying_base_firmware": "exact-stock-v1.03",
                "menu_numbers_option_order_ui_resources_and_settings_semantics": "stock-v1.03-compatible",
                "mcp_configuration_offsets": "stock-v1.03-compatible",
                "captured_setting_values": "162-value-or-information-pages-plus-14-read-only-safe-inspection-pages-current-radio-state-not-default",
                "row_only_scope": "14-reviewed-safe-pages-entered-and-screen-validated; 16-destructive-or-external-actions-plus-25-multi-record-or-editor-pages-located-but-never-entered",
                "automation_transport_and_frame_capture": "custom-automation-only",
                "absolute_firmware_and_runtime_addresses": "exact-v1.03-image-or-custom-automation-build-specific",
                "hardware_scope": ["th-d75a-hardware-qualified", "region-unqualified"],
                "custom_only_exceptions": {
                    "command_collisions": ["GM", "GW"],
                    "features": ["usb-mass-storage-recovery", "gm-virtual-aperture", "screen-capture", "key-dispatch", "guarded-decimal-route"],
                },
            },
            "safety_policy": {
                "numeric_gate": {
                    "mode": "single-command-automation-start-guarded-atomic-decimal-route",
                    "each_complete_numeric_route_requires_one-exact-qualified-top-menu-frame": true,
                    "bound_metadata": ["generation", "crc32", "command_count", "seqlock", "packed-route", "guard-count", "completed-taps", "event-mask"],
                    "maximum_host_lease_age_after_validated_capture_ms": millis(GUARDED_SNAPSHOT_MAX_AGE),
                    "maximum_route_command_reply_duration_ms": millis(GUARDED_ROUTE_MAX_DURATION),
                    "host_transport_capture_metadata_or_evidence_work_between-digits": false,
                    "firmware_conditional_dispatch": true,
                    "snapshot_lease_consumed_once": true,
                    "context_changed_behavior": "authenticated-zero-prefix-refusal-before-any-route-input-no-retry",
                    "guard_invariant": "one-firmware-observed-full-frame-match-before-all-three-synchronous-route-taps",
                    "host_ocr_io_to_key_race_removed": true,
                    "residual_concurrent_framebuffer_writer_toctou": true,
                },
                "pre-audit-automation-canaries": {
                    "missing-snapshot": "status-02-command-3-result-2-no-release",
                    "changed-context": "top-menu-snapshot-then-MENU-to-home-then-refused-MENU-probe",
                    "command-4-zero-prefix-refusal": "top-menu-snapshot-then-MENU-to-home-then-R991-status-02-guard1-completed0-mask00",
                    "zero-hold-route": "one-start-guard-then-atomic-991-to-exact-Version-V1.03.AZM-page-then-masked-dual-band-home-restoration",
                    "zero-hold-qualified-only-after-live-canary": true,
                    "post-refusal-screen": "reviewed-v1.03-masked-dual-band-home-oracle",
                },
                "value_page_next_key_is_mode_to_exact_numbered_row": true,
                "row_only_pages": {
                    "safe_inspection": "14-pages-entered-read-only-with-no-edit-or-navigation-key-then-one-MODE-to-exact-numbered-row",
                    "destructive_or_external_action": "16-pages-located-never-entered",
                    "multi_record_or_editor": "25-pages-located-never-entered",
                },
                "transmit_ptt_power_reset_and_destructive_confirmation": false,
                "persistent_configuration_final_bytes_must_match_before_and_after": true,
                "persistent_configuration_scope": CONFIGURATION_SNAPSHOT_SCOPE,
                "temporary_persistent_prerequisites": {
                    "menu_134": {
                        "reason": "stock-v1.03-refuses-Priority-Scan-page-when-Pri-special-memory-is-empty",
                        "separate_pages_outside_350-page-snapshot": ["0x0031", "0x00F7"],
                        "before_evidence": "owner-private-full-pages-fsynced-before-any-write",
                        "empty_pri_setup": "exact-stock-WX1-40-byte-record-to-Pri-data-first-then-retained-special-channel-flag-byte-0x00-last",
                        "donor_safety_basis": "stock-WX1-162.550-MHz-FM-simplex-no-split-zero-offset-and-outside-documented-A/E-transmit-intervals; no-TX-disable-bit-claimed",
                        "existing_valid_pri": "compare-only-no-write",
                        "restore": "Pri-validity-flag-page-first-then-data-page; exact-full-page-readback",
                        "qualified_home_proof_before_continuing": true,
                    },
                },
                "rendered_home_oracle": {
                    "profile": HOME_MASK_ID,
                    "masked_rgb565_pixels_must_match": HOME_MASK_INCLUDED_PIXELS,
                    "volatile_full_width_row_ranges": HOME_MASK_EXCLUDED_ROWS,
                    "volatile_signal_meter_rectangle": {
                        "x": HOME_MASK_SIGNAL_METER_RECT.0,
                        "y": HOME_MASK_SIGNAL_METER_RECT.1,
                        "width": HOME_MASK_SIGNAL_METER_RECT.2,
                        "height": HOME_MASK_SIGNAL_METER_RECT.3,
                    },
                    "ordered_frequency_and_mode_anchor_text_must_match": true,
                    "vision_anchor_bounds_used_for_verdict": false,
                    "full_frame_crc_and_diff_are_evidence_not_verdict": true,
                },
                "hidden_volatile_state_covered": false,
            },
        }))
    }

    async fn read_menu_134_pri_pages(
        radio: &mut Radio<EitherTransport>,
    ) -> AuditResult<([u8; programming::PAGE_SIZE], [u8; programming::PAGE_SIZE])> {
        let pages = radio
            .read_sparse_memory_pages(&[
                McpPage::new(MENU_134_FLAG_PAGE)?,
                McpPage::new(MENU_134_DATA_PAGE)?,
            ])
            .await?;
        let [(flag_page, flag), (data_page, data)] = pages.as_slice() else {
            return Err(io::Error::other(format!(
                "Menu 134 MCP read returned {} pages instead of exactly two",
                pages.len()
            ))
            .into());
        };
        if flag_page.as_raw() != MENU_134_FLAG_PAGE || data_page.as_raw() != MENU_134_DATA_PAGE {
            return Err(
                io::Error::other("Menu 134 MCP read returned unexpected page addresses").into(),
            );
        }
        Ok((*flag, *data))
    }

    fn persist_menu_134_pri_evidence(
        output_dir: &Path,
        journal: &mut Journal,
        flag: &[u8; programming::PAGE_SIZE],
        data: &[u8; programming::PAGE_SIZE],
    ) -> AuditResult<()> {
        let pages = [(MENU_134_FLAG_PAGE, *flag), (MENU_134_DATA_PAGE, *data)];
        let raw = serialize_configuration_snapshot(&pages);
        let artifact = "menu-134-pri-pages-before.bin";
        let artifact_path = output_dir.join(artifact);
        let mut options = OpenOptions::new();
        let configured = options.write(true).create_new(true);
        #[cfg(unix)]
        let configured = configured.mode(0o600);
        let mut artifact_file = configured.open(&artifact_path)?;
        artifact_file.write_all(&raw)?;
        artifact_file.sync_all()?;
        File::open(output_dir)?.sync_all()?;

        journal.append(json!({
            "type": "menu-134-pri-prerequisite-snapshot",
            "menu_number": "134",
            "raw_artifact": artifact,
            "raw_artifact_mode": "owner-private-0600-under-owner-private-output-directory",
            "raw_artifact_format": "ordered-records-of-u16le-page-number-followed-by-256-raw-page-bytes-no-header",
            "raw_bytes_in_jsonl": false,
            "pages": [
                {
                    "page": format!("0x{MENU_134_FLAG_PAGE:04X}"),
                    "sha256": sha256_hex(flag)?,
                },
                {
                    "page": format!("0x{MENU_134_DATA_PAGE:04X}"),
                    "sha256": sha256_hex(data)?,
                },
            ],
            "durability_before_any_write": "artifact-file-fsync-directory-fsync-journal-fsync",
            "result": "pass",
        }))?;
        journal.sync_all()
    }

    async fn setup_menu_134_pri(
        radio: &mut Radio<EitherTransport>,
        journal: &mut Journal,
        plan: &Menu134PriPlan,
    ) -> AuditResult<()> {
        let exchanges = plan.setup_exchanges()?;
        journal.append(json!({
            "type": "menu-134-pri-prerequisite-setup-intent",
            "menu_number": "134",
            "pri_channel": MENU_134_PRI_CHANNEL,
            "disposition": match plan.disposition {
                Menu134PriDisposition::ExistingValid => "existing-valid-pri-no-write",
                Menu134PriDisposition::StagedFromStockWx1 => "temporary-copy-of-stock-wx1",
            },
            "compare_order": ["0x00F7-data", "0x0031-flag"],
            "write_order": if plan.temporary_write_required() {
                json!(["0x00F7-data-first", "0x0031-validity-flag-last"])
            } else {
                json!([])
            },
            "copied_record": if plan.temporary_write_required() {
                Some("exact-40-byte-stock-WX1-record")
            } else {
                None
            },
            "copied_flag_bytes": if plan.temporary_write_required() {
                Some("only-the-one-retained-WX1-special-channel-flag-byte")
            } else {
                None
            },
            "result": "pending",
        }))?;
        let result = radio.compare_exchange_memory_pages(&exchanges).await;
        match result {
            Ok(written) => {
                let expected = if plan.temporary_write_required() {
                    vec![
                        WritableMcpPage::new(MENU_134_DATA_PAGE)?,
                        WritableMcpPage::new(MENU_134_FLAG_PAGE)?,
                    ]
                } else {
                    Vec::new()
                };
                if written != expected {
                    return Err(io::Error::other(format!(
                        "Menu 134 setup reported write order {written:?}, expected {expected:?}"
                    ))
                    .into());
                }
                journal.append(json!({
                    "type": "menu-134-pri-prerequisite-setup",
                    "menu_number": "134",
                    "written_pages": written.iter().map(|page| format!("0x{page:04X}")).collect::<Vec<_>>(),
                    "exact_page_readback_verified_by_mcp_primitive": true,
                    "result": "pass",
                }))?;
                Ok(())
            }
            Err(error) => {
                let error_text = error.to_string();
                let possibly_written = error
                    .possibly_written_pages()
                    .iter()
                    .map(|page| format!("0x{page:04X}"))
                    .collect::<Vec<_>>();
                let journal_result = journal.append(json!({
                    "type": "menu-134-pri-prerequisite-setup",
                    "menu_number": "134",
                    "possibly_written_pages": possibly_written,
                    "error": error_text,
                    "result": "fail-restoration-required",
                }));
                combine_primary_and_cleanup_errors(
                    Err(Box::new(error)),
                    [("setup-failure-journal", journal_result)],
                )
            }
        }
    }

    fn validate_menu_134_written_pages(
        written: &[WritableMcpPage],
        expected: &[u16],
        phase: &str,
    ) -> AuditResult<()> {
        let written = written
            .iter()
            .copied()
            .map(WritableMcpPage::as_raw)
            .collect::<Vec<_>>();
        if written == expected {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Menu 134 {phase} reported write order {written:?}, expected {expected:?}"
            ))
            .into())
        }
    }

    async fn restore_menu_134_pri(
        radio: &mut Radio<EitherTransport>,
        journal: &mut Journal,
        plan: &Menu134PriPlan,
        setup_succeeded: bool,
    ) -> AuditResult<()> {
        let intent_journal = journal.append(json!({
            "type": "menu-134-pri-restoration-intent",
            "menu_number": "134",
            "temporary_write_required": plan.temporary_write_required(),
            "restore_order": if plan.temporary_write_required() {
                json!(["0x0031-validity-flag-first", "0x00F7-data-last"])
            } else {
                json!([])
            },
            "final_requirement": "exact-full-page-equality-with-fsynced-before-artifact",
        }));

        let mut direct_error: Option<AuditError> = None;
        if plan.temporary_write_required() && setup_succeeded {
            let direct = plan.direct_restore_exchanges()?;
            match radio.compare_exchange_memory_pages(&direct).await {
                Ok(written) => {
                    if let Err(error) = validate_menu_134_written_pages(
                        &written,
                        &[MENU_134_FLAG_PAGE, MENU_134_DATA_PAGE],
                        "direct restoration",
                    ) {
                        direct_error = Some(error);
                    }
                }
                Err(error) => direct_error = Some(Box::new(error)),
            }
        }

        let needs_mcp_recovery = !setup_succeeded || direct_error.is_some();
        let needs_state_aware_restore = plan.temporary_write_required() && needs_mcp_recovery;
        let interrupted_recovery = if needs_mcp_recovery {
            radio
                .recover_from_interrupted_mcp()
                .await
                .map_err(|error| -> AuditError { Box::new(error) })
        } else {
            Ok(())
        };
        let fallback_restore = if needs_state_aware_restore {
            async {
                let (live_flag, live_data) = read_menu_134_pri_pages(radio).await?;
                let exchanges = plan_menu_134_restore_pages(plan, live_flag, live_data)?;
                let written = radio.compare_exchange_memory_pages(&exchanges).await?;
                let expected = exchanges
                    .iter()
                    .filter(|exchange| exchange.expected() != exchange.replacement())
                    .map(|exchange| exchange.page().as_raw())
                    .collect::<Vec<_>>();
                validate_menu_134_written_pages(&written, &expected, "fallback restoration")
            }
            .await
        } else {
            Ok(())
        };

        let exact_final = async {
            let (flag, data) = read_menu_134_pri_pages(radio).await?;
            if flag != plan.flag_before || data != plan.data_before {
                return Err(io::Error::other(
                    "Menu 134 cleanup did not restore both full MCP pages byte-for-byte",
                )
                .into());
            }
            Ok(())
        }
        .await;
        let passed = direct_error.is_none()
            && interrupted_recovery.is_ok()
            && fallback_restore.is_ok()
            && exact_final.is_ok();
        let journal_result = journal.append(json!({
            "type": "menu-134-pri-restoration",
            "menu_number": "134",
            "direct_restore_error": direct_error.as_ref().map(ToString::to_string),
            "interrupted_mcp_recovery_error": interrupted_recovery.as_ref().err().map(ToString::to_string),
            "fallback_restore_error": fallback_restore.as_ref().err().map(ToString::to_string),
            "final_exact_equality_error": exact_final.as_ref().err().map(ToString::to_string),
            "full_pages_equal_to_before_artifact": exact_final.is_ok(),
            "result": if passed { "pass" } else { "fail" },
        }));

        let direct_result = direct_error.map_or(Ok(()), Err);
        combine_primary_and_cleanup_errors(
            direct_result,
            [
                ("restoration-intent-journal", intent_journal),
                ("interrupted-mcp-recovery", interrupted_recovery),
                ("state-aware-restoration", fallback_restore),
                ("exact-final-page-verification", exact_final),
                ("restoration-journal", journal_result),
            ],
        )
    }

    async fn qualify_menu_134_home<'a>(
        radio: &'a mut Radio<EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        phase: &str,
    ) -> AuditResult<AutomationSession<'a, EitherTransport>> {
        let qualification_started = Instant::now();
        let mut session = radio.qualify_automation().await?;
        journal.append(json!({
            "type": "qualification",
            "phase": phase,
            "first_cat_or_mcp_operation": false,
            "elapsed_ms": millis(qualification_started.elapsed()),
            "abi": {
                "version": session.abi().version,
                "features": session.abi().features,
                "max_key": session.abi().max_key,
                "max_phase": session.abi().max_phase,
            },
        }))?;
        let observed = normalize_to_home(&mut session, output_dir, journal).await?;
        let comparison = compare_dual_band_home(&observed, baseline)?;
        let expected_band = observed_operation_band(baseline).ok_or_else(|| {
            io::Error::other("Menu 134 baseline omitted one unambiguous operation-band marker")
        })?;
        let observed_band = observed_operation_band(&observed);
        journal_home_comparison(
            journal,
            phase,
            Some("134"),
            &observed,
            baseline,
            &comparison,
        )?;
        journal.append(json!({
            "type": "menu-134-post-mcp-home-proof",
            "menu_number": "134",
            "phase": phase,
            "operation_band": observed_band.map(|band| format!("{band:?}")),
            "expected_operation_band": format!("{expected_band:?}"),
            "exact_reviewed_home_restored": comparison.restored(),
            "result": if comparison.restored() && observed_band == Some(expected_band) {
                "pass"
            } else {
                "fail"
            },
        }))?;
        if !comparison.restored() || observed_band != Some(expected_band) {
            return Err(io::Error::other(
                "Menu 134 MCP transition did not restore the exact reviewed home and operation band",
            )
            .into());
        }
        Ok(session)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the Menu 134 transaction keeps its durable intent, qualification, one-page audit, and unconditional cleanup in one visible fail-safe sequence"
    )]
    async fn audit_menu_134_transaction(
        radio: &mut Radio<EitherTransport>,
        pre_mcp_transport_policy: PreMcpTransportPolicy,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        baseline: &CapturedScreen,
        before: &ConfigurationSnapshot,
        summary: &mut Summary,
    ) -> Menu134AuditOutcome {
        let attempted_before = summary.attempted;
        journal.set_active_menu(Some("134"));
        let inspection = async {
            prepare_transport_for_mcp(
                radio,
                pre_mcp_transport_policy,
                journal,
                "after-menu-automation-before-menu-134-prerequisite-read",
            )
            .await?;
            require_menu_134_priority_scan_off(snapshot_bool_field(
                before,
                "radio.PriorityScan",
            )?)?;
            let (flag_before, data_before) = read_menu_134_pri_pages(radio).await?;
            persist_menu_134_pri_evidence(
                output_dir,
                journal,
                &flag_before,
                &data_before,
            )?;
            let plan = plan_menu_134_pri_pages(flag_before, data_before)?;
            journal.append(json!({
                "type": "menu-134-pri-prerequisite-validation",
                "menu_number": "134",
                "priority_scan_before": false,
                "pri_channel": MENU_134_PRI_CHANNEL,
                "pri_state": match plan.disposition {
                    Menu134PriDisposition::ExistingValid => "existing-valid-no-temporary-write",
                    Menu134PriDisposition::StagedFromStockWx1 => "empty-stage-required",
                },
                "donor": plan.temporary_write_required().then(|| json!({
                    "channel": MENU_134_WX1_CHANNEL,
                    "identity": "stock-WX1",
                    "rx_hz": MENU_134_WX1_RX_HZ,
                    "mode": "FM",
                    "special_channel_flag_byte": format!("0x{:02X}", programming::FLAG_VHF),
                    "record_fixture": "simplex-no-split-zero-offset",
                    "reception_only_basis": "special-stock-WX1-at-162.550-MHz-outside-every-documented-TH-D75A-and-TH-D75E-transmit-interval; no-record-level-TX-disable-bit-is-claimed",
                })),
                "private_channel_fields_in_jsonl": false,
                "result": "pass",
            }))?;
            Ok::<Menu134PriPlan, AuditError>(plan)
        }
        .await;
        let plan = match inspection {
            Ok(plan) => plan,
            Err(error) => {
                if summary.attempted == attempted_before {
                    summary.attempted = summary.attempted.saturating_add(1);
                }
                return Menu134AuditOutcome {
                    primary: Err(error),
                    cleanup: Ok(()),
                };
            }
        };

        let setup_result = setup_menu_134_pri(radio, journal, &plan).await;
        let setup_succeeded = setup_result.is_ok();
        let mut automation_reconnect_required = false;
        let mut ui_recovery = Ok(());
        let primary = match setup_result {
            Err(error) => Err(error),
            Ok(()) => match qualify_menu_134_home(
                radio,
                output_dir,
                journal,
                baseline,
                "menu-134-after-pri-setup",
            )
            .await
            {
                Err(error) => {
                    automation_reconnect_required = true;
                    Err(error)
                }
                Ok(mut session) => {
                    let audit_result = audit_entry(
                        &mut session,
                        output_dir,
                        journal,
                        entry,
                        baseline,
                        before,
                        summary,
                    )
                    .await;
                    if audit_result.is_err() && session.is_valid() {
                        ui_recovery = best_effort_home_recovery(
                            &mut session,
                            output_dir,
                            journal,
                            baseline,
                            "menu-134-before-pri-restoration",
                        )
                        .await;
                    }
                    automation_reconnect_required = !session.is_valid();
                    audit_result
                }
            },
        };

        let automation_reconnect = if automation_reconnect_required {
            radio
                .reconnect()
                .await
                .map_err(|error| -> AuditError { Box::new(error) })
        } else {
            prepare_transport_for_mcp(
                radio,
                pre_mcp_transport_policy,
                journal,
                "after-menu-134-automation-before-pri-restoration",
            )
            .await
        };
        let restoration = restore_menu_134_pri(radio, journal, &plan, setup_succeeded).await;
        let cleanup = combine_primary_and_cleanup_errors(
            Ok(()),
            [
                ("pre-restoration-ui-recovery", ui_recovery),
                ("pre-restoration-automation-reconnect", automation_reconnect),
                ("exact-pri-page-restoration", restoration),
            ],
        );
        if summary.attempted == attempted_before {
            summary.attempted = summary.attempted.saturating_add(1);
        }
        Menu134AuditOutcome { primary, cleanup }
    }

    async fn record_recoverable_menu_failure(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        entry: &MenuEntry,
        error: AuditError,
    ) -> AuditResult<String> {
        let error = error.to_string();
        summary_inconclusive_for_failure(journal, entry, session.is_valid(), &error)?;
        if !session.is_valid() {
            return Err(io::Error::other(format!(
                "menu {} invalidated the qualified automation session: {error}",
                entry.number
            ))
            .into());
        }
        best_effort_home_recovery(
            session,
            output_dir,
            journal,
            baseline,
            "after-recoverable-entry-failure",
        )
        .await
        .map_err(|recovery_error| {
            io::Error::other(format!(
                "menu {} failed: {error}; exact home recovery also failed: {recovery_error}",
                entry.number
            ))
        })?;
        journal.append(json!({
            "type": "menu-entry-failure-recovery",
            "menu_number": entry.number,
            "failed_operation_replayed": false,
            "exact_dual_band_home_restored": true,
            "result": "pass",
        }))?;
        Ok(format!("{}: {error}", entry.number))
    }

    fn summary_inconclusive_for_failure(
        journal: &mut Journal,
        entry: &MenuEntry,
        session_valid: bool,
        error: &str,
    ) -> AuditResult<()> {
        journal.append(json!({
            "type": "menu-entry-failure",
            "menu_number": entry.number,
            "error": error,
            "session_valid": session_valid,
            "policy": "continue-only-after-exact-dual-band-home-recovery",
        }))
    }

    async fn audit_menu_chunk(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entries: &[&MenuEntry],
        baseline: &CapturedScreen,
        before: &ConfigurationSnapshot,
        summary: &mut Summary,
    ) -> AuditResult<Vec<String>> {
        let mut failures = Vec::new();
        for entry in entries {
            if entry.number == "134" {
                return Err(invalid_input(
                    "Menu 134 must run only inside its dedicated Pri-page transaction",
                ));
            }
            let result = if entry.number == "102" {
                audit_menu_102(
                    session, output_dir, journal, entry, baseline, before, summary,
                )
                .await
            } else {
                audit_entry(
                    session, output_dir, journal, entry, baseline, before, summary,
                )
                .await
            };
            if let Err(error) = result {
                summary.inconclusive = summary.inconclusive.saturating_add(1);
                failures.push(
                    record_recoverable_menu_failure(
                        session, output_dir, journal, baseline, entry, error,
                    )
                    .await?,
                );
            }
        }
        Ok(failures)
    }

    fn split_menu_134_entries<'a>(
        entries: &'a [&'a MenuEntry],
    ) -> (
        &'a [&'a MenuEntry],
        Option<&'a MenuEntry>,
        &'a [&'a MenuEntry],
    ) {
        let Some(index) = entries.iter().position(|entry| entry.number == "134") else {
            return (entries, None, &[]);
        };
        let before = entries.get(..index).unwrap_or_default();
        let tail = entries.get(index..).unwrap_or_default();
        let Some((entry, after)) = tail.split_first() else {
            return (entries, None, &[]);
        };
        (before, Some(*entry), after)
    }

    fn recoverable_menu_failures_result(failures: &[String]) -> AuditResult<()> {
        if failures.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{} recoverable menu audit failure(s): {}",
                failures.len(),
                failures.join(" | ")
            ))
            .into())
        }
    }

    async fn prepare_transient_menu_state(
        radio: &mut Radio<EitherTransport>,
        entries: &[&MenuEntry],
        journal: &mut Journal,
    ) -> AuditResult<Option<TransientRadioState>> {
        let normalize_for_menu_100 = entries.iter().any(|entry| entry.number == "100");
        if !normalize_for_menu_100 {
            return Ok(None);
        }

        let state = TransientRadioState {
            original_band: radio.get_band().await?,
            original_band_a_tuning_mode: radio.get_tuning_mode(Band::A).await?,
            normalized_for_menu_100: normalize_for_menu_100,
        };
        journal.append(json!({
            "type": "transient-radio-state-observation",
            "phase": "before-menu-audit-normalization",
            "reasons": {
                "menu_100": normalize_for_menu_100.then_some("stock-v1.03-menu-100-requires-operation-band-A-and-band-A-VFO"),
            },
            "operation_band": format!("{:?}", state.original_band),
            "band_a_tuning_mode": format!("{:?}", state.original_band_a_tuning_mode),
            "persistent_mcp_configuration_will_be_temporarily_changed": false,
        }))?;

        let preparation = async {
            if state.normalized_for_menu_100
                && state.original_band_a_tuning_mode != TuningMode::Vfo
            {
                journal.append(json!({
                    "type": "transient-radio-state-intent",
                    "action": "set-band-a-tuning-mode",
                    "from": format!("{:?}", state.original_band_a_tuning_mode),
                    "to": format!("{:?}", TuningMode::Vfo),
                    "wire_semantics": "VM 0,0",
                }))?;
                radio
                    .set_tuning_mode(Band::A, TuningMode::Vfo)
                    .await?;
            }
            if state.normalized_for_menu_100 && state.original_band != Band::A {
                journal.append(json!({
                    "type": "transient-radio-state-intent",
                    "action": "set-operation-band",
                    "from": format!("{:?}", state.original_band),
                    "to": format!("{:?}", Band::A),
                    "wire_semantics": "BC 0",
                }))?;
                radio.set_band(Band::A).await?;
            }

            let verified_tuning_mode = radio.get_tuning_mode(Band::A).await?;
            let verified_band = radio.get_band().await?;
            if state.normalized_for_menu_100
                && (verified_tuning_mode != TuningMode::Vfo || verified_band != Band::A)
            {
                return Err(io::Error::other(format!(
                    "transient Menu 100 normalization expected Band A/VFO, got {verified_band:?}/{verified_tuning_mode:?}"
                ))
                .into());
            }
            journal.append(json!({
                "type": "transient-radio-state-verification",
                "phase": "before-menu-audit",
                "operation_band": format!("{:?}", verified_band),
                "band_a_tuning_mode": format!("{:?}", verified_tuning_mode),
                "result": "pass",
            }))?;
            Ok::<(), AuditError>(())
        }
        .await;

        if let Err(primary) = preparation {
            let rollback = restore_transient_menu_state(
                radio,
                state,
                journal,
                "normalization-failure-rollback",
            )
            .await;
            return combine_primary_and_cleanup_errors(
                Err(primary),
                [("transient-state-rollback", rollback)],
            )
            .map(|()| None);
        }
        Ok(Some(state))
    }

    async fn restore_transient_menu_state(
        radio: &mut Radio<EitherTransport>,
        state: TransientRadioState,
        journal: &mut Journal,
        phase: &str,
    ) -> AuditResult<()> {
        journal.append(json!({
            "type": "transient-radio-state-restore-intent",
            "phase": phase,
            "operation_band": format!("{:?}", state.original_band),
            "band_a_tuning_mode": format!("{:?}", state.original_band_a_tuning_mode),
        }))?;

        if state.normalized_for_menu_100
            && radio.get_tuning_mode(Band::A).await? != state.original_band_a_tuning_mode
        {
            radio
                .set_tuning_mode(Band::A, state.original_band_a_tuning_mode)
                .await?;
        }
        if state.normalized_for_menu_100 && radio.get_band().await? != state.original_band {
            radio.set_band(state.original_band).await?;
        }
        let verified_tuning_mode = radio.get_tuning_mode(Band::A).await?;
        let verified_band = radio.get_band().await?;
        if state.normalized_for_menu_100
            && (verified_tuning_mode != state.original_band_a_tuning_mode
                || verified_band != state.original_band)
        {
            return Err(io::Error::other(format!(
                "transient-state restore expected {:?}/{:?}, got {verified_band:?}/{verified_tuning_mode:?}",
                state.original_band, state.original_band_a_tuning_mode
            ))
            .into());
        }
        journal.append(json!({
            "type": "transient-radio-state-restore-verification",
            "phase": phase,
            "operation_band": format!("{:?}", verified_band),
            "band_a_tuning_mode": format!("{:?}", verified_tuning_mode),
            "result": "pass",
        }))?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the hardware audit's qualification, independent before/after MCP evidence, UI cleanup, final requalification, and error aggregation are deliberately visible in one linear fail-closed coordinator"
    )]
    #[expect(
        clippy::drop_non_drop,
        reason = "the explicit drop ends AutomationSession's exclusive Radio borrow before the bounded Menu 134 MCP transaction"
    )]
    async fn execute_audit(
        radio: &mut Radio<EitherTransport>,
        pre_mcp_transport_policy: PreMcpTransportPolicy,
        output_dir: &Path,
        entries: &[&MenuEntry],
        expected_value_total: usize,
        expected_safe_inspection_total: usize,
        expected_located_not_entered_total: usize,
        snapshot_pages: &[u16],
        journal: &mut Journal,
        summary: &mut Summary,
    ) -> AuditResult<()> {
        enum InitialSessionOutcome {
            Ready(InitialHomeState),
            Failed {
                primary: AuditError,
                in_session_recovery: AuditResult<()>,
                reconnect_needed: bool,
                in_session_phase: &'static str,
                reconnect_phase: &'static str,
                dual_band_baseline: Option<CapturedScreen>,
                single_band_baseline: Option<CapturedScreen>,
            },
        }

        enum MenuAuditSessionOutcome {
            QualificationFailed(AuditError),
            Completed {
                audit_result: AuditResult<()>,
                in_session_recovery: AuditResult<()>,
                reconnect_needed: bool,
                audit_baseline: Option<CapturedScreen>,
            },
        }

        let initial_session_outcome = {
            let qualification_started = Instant::now();
            let mut session = radio.qualify_automation().await?;
            let mut single_band_baseline = None;
            let mut home_normalization_started = false;
            let abi = session.abi();
            journal.append(json!({
                "type": "qualification",
                "phase": "initial-first-cat-or-mcp-operation",
                "first_cat_or_mcp_operation": true,
                "elapsed_ms": millis(qualification_started.elapsed()),
                "abi": {
                    "version": abi.version,
                    "features": abi.features,
                    "max_key": abi.max_key,
                    "max_phase": abi.max_phase,
                },
            }))?;

            let initial_preparation = async {
                let missing_started = Instant::now();
                journal.append(json!({
                "type": "automation-guard-canary-intent",
                "scenario": "missing-snapshot",
                "guarded_probe_key": format!("{:?}", FrontPanelKey::Menu),
                "raw_key": FrontPanelKey::Menu.as_raw(),
                "required_before_first_screen_capture": true,
                }))?;
                let missing = session
                    .verify_missing_snapshot_refusal(FrontPanelKey::Menu)
                    .await?;
                require_single_guarded_refusal(
                    &missing,
                    FrontPanelKey::Menu,
                    "missing-snapshot",
                )?;
                journal_guarded_refusal(
                    journal,
                    "missing-snapshot",
                    None,
                    FrontPanelKey::Menu,
                    missing_started.elapsed(),
                    &missing,
                    &json!({
                    "firmware_snapshot_state": "no-valid-snapshot-capture_result-1",
                    "screen_proof": "exact-qualified-automation-runtime-plus-status-02-proves-input_dispatch-not-called",
                    "screen_capture_before_probe": false,
                    }),
                )?;

                home_normalization_started = true;
                let baseline = normalize_to_home_tracking_single_band(
                    &mut session,
                    output_dir,
                    journal,
                    &mut single_band_baseline,
                )
                .await?;
                let baseline_profile = compare_dual_band_home(&baseline, &baseline)?;
                journal_home_comparison(
                    journal,
                    "qualified-initial-baseline",
                    None,
                    &baseline,
                    &baseline,
                    &baseline_profile,
                )?;
                journal.append(json!({
                "type": "baseline-assertion",
                "crc32": format!("{:08X}", baseline.crc32),
                "comparison_policy": "reviewed-v1.03-dual-band-mask-plus-ordered-frequency-mode-anchor-text-after-every-menu",
                "mask_id": HOME_MASK_ID,
                "hidden_volatile_state_covered": false,
                "result": "pass",
                }))?;
                let operation_band = observed_operation_band(&baseline).ok_or_else(|| {
                    io::Error::other(
                        "initial home screen did not expose one unambiguous PTT/operation-band marker",
                    )
                })?;
                journal.append(json!({
                    "type": "operation-band-screen-oracle",
                    "phase": "initial-home",
                    "operation_band": format!("{operation_band:?}"),
                    "basis": "PTT-marker-rendered-in-exactly-one-reviewed-band-status-row",
                    "result": "pass",
                }))?;
                Ok::<(CapturedScreen, Band), AuditError>((baseline, operation_band))
            }
            .await;
            match initial_preparation {
                Ok((baseline, operation_band)) => {
                    let canary_result = async {
                        verify_changed_context_guard_canary(
                            &mut session,
                            output_dir,
                            journal,
                            &baseline,
                        )
                        .await?;
                        verify_zero_prefix_batch_refusal_canary(
                            &mut session,
                            output_dir,
                            journal,
                            &baseline,
                        )
                        .await?;
                        verify_zero_hold_batch_route_canary(
                            &mut session,
                            output_dir,
                            journal,
                            &baseline,
                        )
                        .await
                    }
                    .await;
                    match canary_result {
                        Ok(()) => InitialSessionOutcome::Ready(InitialHomeState {
                            dual_band_baseline: baseline,
                            operation_band,
                            single_band_baseline,
                        }),
                        Err(primary) => {
                            let in_session_recovery = if session.is_valid() {
                                let dual_band_recovery = best_effort_home_recovery(
                                    &mut session,
                                    output_dir,
                                    journal,
                                    &baseline,
                                    "after-pre-audit-canary-failure",
                                )
                                .await;
                                let display_mode_recovery = if dual_band_recovery.is_ok() {
                                    if let Some(single_band_baseline) =
                                        single_band_baseline.as_ref()
                                    {
                                        restore_startup_single_band_profile(
                                            &mut session,
                                            output_dir,
                                            journal,
                                            single_band_baseline,
                                            "after-pre-audit-canary-failure",
                                        )
                                        .await
                                    } else {
                                        Ok(())
                                    }
                                } else {
                                    Ok(())
                                };
                                combine_primary_and_cleanup_errors(
                                    Ok(()),
                                    [
                                        ("dual-band-home-recovery", dual_band_recovery),
                                        ("startup-display-mode-restoration", display_mode_recovery),
                                    ],
                                )
                            } else {
                                Ok(())
                            };
                            let reconnect_needed =
                                !session.is_valid() || in_session_recovery.is_err();
                            InitialSessionOutcome::Failed {
                                primary,
                                in_session_recovery,
                                reconnect_needed,
                                in_session_phase: "pre-audit-canary-in-session-ui-recovery",
                                reconnect_phase: "after-pre-audit-canary-reconnect",
                                dual_band_baseline: Some(baseline),
                                single_band_baseline,
                            }
                        }
                    }
                }
                Err(primary_error) => {
                    journal.set_active_menu(None);
                    let in_session_recovery = if session.is_valid() && home_normalization_started {
                        let dual_band_recovery = normalize_to_home_tracking_single_band(
                            &mut session,
                            output_dir,
                            journal,
                            &mut single_band_baseline,
                        )
                        .await
                        .map(|_| ());
                        let display_mode_recovery = if dual_band_recovery.is_ok() {
                            if let Some(single_band_baseline) = single_band_baseline.as_ref() {
                                restore_startup_single_band_profile(
                                    &mut session,
                                    output_dir,
                                    journal,
                                    single_band_baseline,
                                    "initial-preparation-failure",
                                )
                                .await
                            } else {
                                Ok(())
                            }
                        } else {
                            Ok(())
                        };
                        combine_primary_and_cleanup_errors(
                            Ok(()),
                            [
                                ("dual-band-home-recovery", dual_band_recovery),
                                ("startup-display-mode-restoration", display_mode_recovery),
                            ],
                        )
                    } else {
                        Ok(())
                    };
                    let reconnect_needed = !session.is_valid() || in_session_recovery.is_err();
                    InitialSessionOutcome::Failed {
                        primary: primary_error,
                        in_session_recovery,
                        reconnect_needed,
                        in_session_phase: "initial-preparation-in-session-ui-recovery",
                        reconnect_phase: "initial-preparation-reconnect",
                        dual_band_baseline: None,
                        single_band_baseline,
                    }
                }
            }
        };
        let initial_home = match initial_session_outcome {
            InitialSessionOutcome::Ready(initial_home) => initial_home,
            InitialSessionOutcome::Failed {
                primary,
                in_session_recovery,
                reconnect_needed,
                in_session_phase,
                reconnect_phase,
                dual_band_baseline,
                single_band_baseline,
            } => {
                let reconnect_recovery = if reconnect_needed {
                    reconnect_and_restore_initial_home_profile(
                        radio,
                        output_dir,
                        journal,
                        dual_band_baseline.as_ref(),
                        single_band_baseline.as_ref(),
                        reconnect_phase,
                    )
                    .await
                } else {
                    Ok(())
                };
                return combine_primary_and_cleanup_errors(
                    Err(primary),
                    [
                        (in_session_phase, in_session_recovery),
                        ("initial-session-reconnect-ui-recovery", reconnect_recovery),
                    ],
                );
            }
        };
        let InitialHomeState {
            dual_band_baseline: initial_baseline,
            operation_band: initial_operation_band,
            single_band_baseline,
        } = initial_home;

        let before_result = async {
            prepare_transport_for_mcp(
                radio,
                pre_mcp_transport_policy,
                journal,
                "after-initial-automation-before-audit-snapshot",
            )
            .await?;
            read_configuration_snapshot(radio, output_dir, snapshot_pages, journal, "before-audit")
                .await
        }
        .await;

        let (audit_result, recovery_result, transient_state_restore_result) = match &before_result {
            Err(error) => (
                Err(io::Error::other(format!("before-audit MCP snapshot failed: {error}")).into()),
                Ok(()),
                Ok(()),
            ),
            Ok(before) => match prepare_transient_menu_state(radio, entries, journal).await {
                Err(error) => (Err(error), Ok(()), Ok(())),
                Ok(transient_state) => {
                    let qualification_started = Instant::now();
                    let session_outcome = match radio.qualify_automation().await {
                        Err(error) => MenuAuditSessionOutcome::QualificationFailed(error.into()),
                        Ok(mut session) => 'menu_session: {
                            let mut audit_baseline = None;
                            let (entries_before_134, menu_134, entries_after_134) =
                                split_menu_134_entries(entries);
                            let mut entry_failures = Vec::new();
                            let mut audit_result = async {
                            let abi = session.abi();
                            journal.append(json!({
                                "type": "qualification",
                                "phase": "post-mcp-requalification",
                                "first_cat_or_mcp_operation": false,
                                "missing_snapshot_canary_repeated": false,
                                "elapsed_ms": millis(qualification_started.elapsed()),
                                "abi": {
                                    "version": abi.version,
                                    "features": abi.features,
                                    "max_key": abi.max_key,
                                    "max_phase": abi.max_phase,
                                },
                            }))?;
                            let baseline =
                                normalize_to_home(&mut session, output_dir, journal).await?;
                            let original_comparison =
                                compare_dual_band_home(&baseline, &initial_baseline)?;
                            if let Some(state) = transient_state
                                && state.normalized_for_menu_100
                            {
                                if state.original_band != initial_operation_band {
                                    return Err(io::Error::other(format!(
                                        "CAT BC reported original band {:?}, but the initial screen PTT oracle reported {initial_operation_band:?}",
                                        state.original_band
                                    ))
                                    .into());
                                }
                                let frequencies_match = ordered_home_anchor_texts_match(
                                    &home_frequency_anchors(&initial_baseline),
                                    &home_frequency_anchors(&baseline),
                                );
                                if !frequencies_match {
                                    return Err(io::Error::other(
                                        "Menu 100 transient normalization changed the ordered home frequencies",
                                    )
                                    .into());
                                }
                                let visible_band = observed_operation_band(&baseline).ok_or_else(|| {
                                    io::Error::other(
                                        "Menu 100 normalization did not leave one unambiguous rendered PTT/operation-band marker",
                                    )
                                })?;
                                journal.append(json!({
                                    "type": "operation-band-screen-oracle",
                                    "phase": "after-menu-100-transient-normalization",
                                    "operation_band": format!("{:?}", visible_band),
                                    "expected_operation_band": format!("{:?}", Band::A),
                                    "basis": "PTT-marker-rendered-in-exactly-one-reviewed-band-status-row",
                                    "result": if visible_band == Band::A { "pass" } else { "fail" },
                                }))?;
                                if visible_band != Band::A {
                                    return Err(io::Error::other(format!(
                                        "CAT normalized Menu 100 to Band A, but the rendered PTT marker remained on {visible_band:?}"
                                    ))
                                    .into());
                                }
                                journal.append(json!({
                                    "type": "operation-band-home-transition-oracle",
                                    "phase": "post-before-mcp-requalification",
                                    "from_operation_band": format!("{:?}", initial_operation_band),
                                    "to_operation_band": format!("{:?}", visible_band),
                                    "ordered_frequency_anchors_match": frequencies_match,
                                    "normalized-home-mode-oracle": "both-rendered-band-mode-rows-remain-reviewed-analog-modes; PTT-prefix-moves-with-operation-band",
                                    "full_frame_differing_pixels": original_comparison.full_differing_pixels,
                                    "masked_frame_differing_pixels": original_comparison.masked_differing_pixels,
                                    "pixel_difference_expected_for_rendered-PTT-marker-move": true,
                                    "result": "pass",
                                }))?;
                            } else {
                                journal_home_comparison(
                                    journal,
                                    "post-before-mcp-requalification",
                                    None,
                                    &baseline,
                                    &initial_baseline,
                                    &original_comparison,
                                )?;
                                if !original_comparison.restored() {
                                    return Err(io::Error::other(
                                        "post-MCP V1.03.AZM requalification did not recover the reviewed V1.03 dual-band home profile",
                                    )
                                    .into());
                                }
                            }

                            let baseline_self_comparison =
                                compare_dual_band_home(&baseline, &baseline)?;
                            journal_home_comparison(
                                journal,
                                "normalized-menu-audit-baseline",
                                None,
                                &baseline,
                                &baseline,
                                &baseline_self_comparison,
                            )?;
                            if !baseline_self_comparison.restored() {
                                return Err(io::Error::other(
                                    "normalized menu-audit baseline failed its own reviewed V1.03 dual-band home oracle",
                                )
                                .into());
                            }
                            audit_baseline = Some(baseline.clone());

                            entry_failures.extend(
                                audit_menu_chunk(
                                    &mut session,
                                    output_dir,
                                    journal,
                                    entries_before_134,
                                    &baseline,
                                    before,
                                    summary,
                                )
                                .await?,
                            );
                            Ok(())
                        }
                        .await;

                            if audit_result.is_ok()
                                && let Some(entry) = menu_134
                            {
                                let baseline = audit_baseline.clone().ok_or_else(|| {
                                    io::Error::other(
                                        "Menu 134 transaction requires a qualified audit baseline",
                                    )
                                })?;
                                drop(session);
                                let Menu134AuditOutcome { primary, cleanup } =
                                    audit_menu_134_transaction(
                                        radio,
                                        pre_mcp_transport_policy,
                                        output_dir,
                                        journal,
                                        entry,
                                        &baseline,
                                        before,
                                        summary,
                                    )
                                    .await;
                                let continuation = qualify_menu_134_home(
                                    radio,
                                    output_dir,
                                    journal,
                                    &baseline,
                                    "menu-134-after-exact-pri-restoration",
                                )
                                .await;
                                let mut session = match continuation {
                                    Ok(session) => session,
                                    Err(qualification_error) => {
                                        let audit_result = combine_primary_and_cleanup_errors(
                                            primary,
                                            [
                                                ("menu-134-transaction-cleanup", cleanup),
                                                (
                                                    "menu-134-post-restoration-qualification",
                                                    Err(qualification_error),
                                                ),
                                            ],
                                        );
                                        break 'menu_session MenuAuditSessionOutcome::Completed {
                                            audit_result,
                                            in_session_recovery: Ok(()),
                                            reconnect_needed: true,
                                            audit_baseline: Some(baseline),
                                        };
                                    }
                                };

                                if let Err(cleanup_error) = cleanup {
                                    audit_result = combine_primary_and_cleanup_errors(
                                        primary,
                                        [("menu-134-transaction-cleanup", Err(cleanup_error))],
                                    );
                                } else {
                                    if let Err(error) = primary {
                                        summary.inconclusive =
                                            summary.inconclusive.saturating_add(1);
                                        match record_recoverable_menu_failure(
                                            &mut session,
                                            output_dir,
                                            journal,
                                            &baseline,
                                            entry,
                                            error,
                                        )
                                        .await
                                        {
                                            Ok(failure) => entry_failures.push(failure),
                                            Err(error) => audit_result = Err(error),
                                        }
                                    }
                                    if audit_result.is_ok() {
                                        match audit_menu_chunk(
                                            &mut session,
                                            output_dir,
                                            journal,
                                            entries_after_134,
                                            &baseline,
                                            before,
                                            summary,
                                        )
                                        .await
                                        {
                                            Ok(failures) => entry_failures.extend(failures),
                                            Err(error) => audit_result = Err(error),
                                        }
                                    }
                                    if audit_result.is_ok() {
                                        audit_result =
                                            recoverable_menu_failures_result(&entry_failures);
                                    }
                                }

                                let in_session_recovery =
                                    if audit_result.is_err() && session.is_valid() {
                                        best_effort_home_recovery(
                                            &mut session,
                                            output_dir,
                                            journal,
                                            &baseline,
                                            "after-entry-failure",
                                        )
                                        .await
                                    } else {
                                        Ok(())
                                    };
                                break 'menu_session MenuAuditSessionOutcome::Completed {
                                    audit_result,
                                    in_session_recovery,
                                    reconnect_needed: !session.is_valid(),
                                    audit_baseline: Some(baseline),
                                };
                            }

                            if audit_result.is_ok() {
                                audit_result = recoverable_menu_failures_result(&entry_failures);
                            }
                            let in_session_recovery = if audit_result.is_err() && session.is_valid()
                            {
                                if let Some(baseline) = audit_baseline.as_ref() {
                                    best_effort_home_recovery(
                                        &mut session,
                                        output_dir,
                                        journal,
                                        baseline,
                                        "after-entry-failure",
                                    )
                                    .await
                                } else {
                                    normalize_to_home(&mut session, output_dir, journal)
                                        .await
                                        .map(|_| ())
                                }
                            } else {
                                Ok(())
                            };
                            MenuAuditSessionOutcome::Completed {
                                audit_result,
                                in_session_recovery,
                                reconnect_needed: !session.is_valid(),
                                audit_baseline,
                            }
                        }
                    };

                    let (audit_result, recovery_result) = match session_outcome {
                        MenuAuditSessionOutcome::QualificationFailed(error) => {
                            let recovery = reconnect_and_normalize_home(
                                radio,
                                output_dir,
                                journal,
                                "post-mcp-qualification-failure-reconnect",
                            )
                            .await
                            .map(|_| ());
                            (Err(error), recovery)
                        }
                        MenuAuditSessionOutcome::Completed {
                            audit_result,
                            in_session_recovery,
                            reconnect_needed,
                            audit_baseline,
                        } => {
                            let reconnect_recovery = if audit_result.is_err() && reconnect_needed {
                                if let Some(baseline) = audit_baseline.as_ref() {
                                    reconnect_and_recover_home(
                                        radio,
                                        output_dir,
                                        journal,
                                        baseline,
                                        "after-entry-failure-reconnect",
                                    )
                                    .await
                                } else {
                                    reconnect_and_normalize_home(
                                        radio,
                                        output_dir,
                                        journal,
                                        "after-entry-failure-reconnect",
                                    )
                                    .await
                                    .map(|_| ())
                                }
                            } else {
                                Ok(())
                            };
                            let recovery_result = combine_primary_and_cleanup_errors(
                                Ok(()),
                                [
                                    ("in-session-ui-recovery", in_session_recovery),
                                    ("reconnect-ui-recovery", reconnect_recovery),
                                ],
                            );
                            (audit_result, recovery_result)
                        }
                    };
                    let transient_state_restore_result = if let Some(state) = transient_state {
                        restore_transient_menu_state(radio, state, journal, "after-menu-audit")
                            .await
                    } else {
                        Ok(())
                    };
                    (
                        audit_result,
                        recovery_result,
                        transient_state_restore_result,
                    )
                }
            },
        };

        // These cleanup checks are deliberately attempted even when a menu
        // entry fails. A primary navigation/validation error must never hide
        // whether the declared MCP configuration scope changed.
        let after_result = async {
            prepare_transport_for_mcp(
                radio,
                pre_mcp_transport_policy,
                journal,
                "after-menu-automation-before-final-snapshot",
            )
            .await?;
            read_configuration_snapshot(radio, output_dir, snapshot_pages, journal, "after-audit")
                .await
        }
        .await;
        let nonmutation_result = match (&before_result, &after_result) {
            (Ok(before), Ok(after)) => require_configuration_unchanged(before, after, journal),
            (Err(before_error), Err(after_error)) => Err(io::Error::other(format!(
                "before-audit MCP snapshot failed: {before_error}; after-audit MCP snapshot also failed: {after_error}"
            ))
            .into()),
            (Err(error), _) => Err(io::Error::other(format!(
                "before-audit MCP snapshot unavailable for nonmutation comparison: {error}"
            ))
            .into()),
            (_, Err(error)) => {
                Err(io::Error::other(format!("after-audit MCP snapshot failed: {error}")).into())
            }
        };

        // The MCP session invalidates automation qualification. Requalify one
        // final time and independently prove rendered-state restoration.
        let final_home_result = async {
            let qualification_started = Instant::now();
            let mut session = radio.qualify_automation().await?;
            journal.append(json!({
                "type": "qualification",
                "phase": "post-final-mcp-requalification",
                "first_cat_or_mcp_operation": false,
                "elapsed_ms": millis(qualification_started.elapsed()),
                "abi": {
                    "version": session.abi().version,
                    "features": session.abi().features,
                    "max_key": session.abi().max_key,
                    "max_phase": session.abi().max_phase,
                },
            }))?;
            let final_home = normalize_to_home(&mut session, output_dir, journal).await?;
            let comparison = compare_dual_band_home(&final_home, &initial_baseline)?;
            let final_operation_band = observed_operation_band(&final_home);
            journal_home_comparison(
                journal,
                "post-final-mcp",
                None,
                &final_home,
                &initial_baseline,
                &comparison,
            )?;
            journal.append(json!({
                "type": "operation-band-screen-oracle",
                "phase": "post-final-mcp",
                "operation_band": final_operation_band.map(|band| format!("{band:?}")),
                "expected_operation_band": format!("{:?}", initial_operation_band),
                "basis": "PTT-marker-rendered-in-exactly-one-reviewed-band-status-row",
                "result": if final_operation_band == Some(initial_operation_band) { "pass" } else { "fail" },
            }))?;
            let dual_band_restoration = if comparison.restored()
                && final_operation_band == Some(initial_operation_band)
            {
                Ok(())
            } else {
                Err(io::Error::other(
                    "final post-MCP screen did not restore the reviewed V1.03 dual-band home profile and operation-band marker",
                )
                .into())
            };
            let display_mode_restoration = if let Some(single_band_baseline) =
                single_band_baseline.as_ref()
            {
                restore_startup_single_band_profile(
                    &mut session,
                    output_dir,
                    journal,
                    single_band_baseline,
                    "post-final-mcp",
                )
                .await
            } else {
                Ok(())
            };
            combine_primary_and_cleanup_errors(
                Ok(()),
                [
                    ("dual-band-home-restoration", dual_band_restoration),
                    ("startup-display-mode-restoration", display_mode_restoration),
                ],
            )
        }
        .await;

        let verdict_result = if audit_result.is_ok() {
            require_conclusive(
                summary,
                entries.len(),
                expected_value_total,
                expected_safe_inspection_total,
                expected_located_not_entered_total,
            )
        } else {
            Ok(())
        };
        combine_primary_and_cleanup_errors(
            audit_result,
            [
                ("best-effort-ui-recovery", recovery_result),
                (
                    "transient-radio-state-restoration",
                    transient_state_restore_result,
                ),
                ("after-mcp-nonmutation", nonmutation_result),
                ("final-home-restoration", final_home_result),
                ("coverage-verdict", verdict_result),
            ],
        )
    }

    fn require_single_guarded_refusal(
        outcome: &GuardedKeyOutcome,
        expected_key: FrontPanelKey,
        scenario: &str,
    ) -> AuditResult<()> {
        let GuardedKeyOutcome::ContextChanged { metadata, receipts } = outcome else {
            return Err(io::Error::other(format!(
                "V1.03.AZM {scenario} canary did not return an authenticated context refusal"
            ))
            .into());
        };
        let [receipt] = receipts.as_slice() else {
            return Err(io::Error::other(format!(
                "V1.03.AZM {scenario} canary returned {} receipts instead of exactly one",
                receipts.len()
            ))
            .into());
        };
        if receipt.key != expected_key
            || receipt.result != GuardedKeyResult::ContextChanged
            || receipt.release_sequence.is_some()
            || receipt.command_count != metadata.command_count
            || receipt.seqlock != metadata.seqlock
            || metadata.last_command != 3
            || metadata.last_key_result != 2
            || metadata.last_key != u32::from(expected_key.as_raw())
            || metadata.last_phase != 0
        {
            return Err(io::Error::other(format!(
                "V1.03.AZM {scenario} canary did not prove status 02, command 3/result 2, one refused press, and no release"
            ))
            .into());
        }
        Ok(())
    }

    fn journal_guarded_refusal(
        journal: &mut Journal,
        scenario: &str,
        context_change_key: Option<FrontPanelKey>,
        guarded_probe_key: FrontPanelKey,
        elapsed: Duration,
        outcome: &GuardedKeyOutcome,
        screen_evidence: &Value,
    ) -> AuditResult<()> {
        let GuardedKeyOutcome::ContextChanged { metadata, receipts } = outcome else {
            return Err(
                io::Error::other("only a validated guarded refusal may be journaled").into(),
            );
        };
        let receipt = receipts.first().ok_or_else(|| {
            io::Error::other("guarded refusal journal requires exactly one receipt")
        })?;
        journal.append(json!({
            "type": "automation-guard-canary-receipt",
            "scenario": scenario,
            "context_change_key": context_change_key.map(|key| format!("{key:?}")),
            "guarded_probe_key": format!("{guarded_probe_key:?}"),
            "guarded_probe_raw_key": guarded_probe_key.as_raw(),
            "wire_reply_status": "02-exact-echo-authenticated",
            "firmware_command": 3,
            "firmware_result": 2,
            "press_sequence": receipt.press_sequence,
            "release_sequence": receipt.release_sequence,
            "dispatch": "refused-before-input_dispatch",
            "elapsed_ms": millis(elapsed),
            "metadata": metadata_json(metadata),
            "screen_evidence": screen_evidence,
            "result": "pass",
        }))
    }

    async fn verify_changed_context_guard_canary(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
    ) -> AuditResult<()> {
        let _menu_receipt = tap(
            session,
            journal,
            FrontPanelKey::Menu,
            "automation-canary-open-menu-context",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let menu_gate = capture_canary_quiescent(
            session,
            output_dir,
            journal,
            "automation-changed-context-menu-gate",
        )
        .await?;
        require_top_level_menu(&menu_gate)?;

        journal.append(json!({
            "type": "automation-guard-canary-intent",
            "scenario": "changed-context",
            "frozen_context": "top-level-Menu",
            "qualified_menu_crc32": format!("{:08X}", menu_gate.crc32),
            "context_change_key": format!("{:?}", FrontPanelKey::Menu),
            "guarded_probe_key": format!("{:?}", FrontPanelKey::Menu),
            "expected_live_context_after_change": "reviewed-v1.03-dual-band-home-profile",
            "expected_probe_behavior_if-incorrectly-dispatched": "reopen-top-level-Menu",
        }))?;

        // Refresh the firmware lease after all canary OCR/evidence work. OCR,
        // artifact output, and journal I/O all occurred against `menu_gate`;
        // none occurs between this raw capture and the guarded transaction.
        let frozen_menu = session.capture_screen().await?;
        if frozen_menu.frame != menu_gate.frame || frozen_menu.metadata.crc32 != menu_gate.crc32 {
            return Err(io::Error::other(
                "V1.03.AZM changed-context canary snapshot no longer matches the qualified top-level Menu frame",
            )
            .into());
        }
        let started = Instant::now();
        let outcome = session
            .verify_changed_context_refusal(&frozen_menu, FrontPanelKey::Menu, FrontPanelKey::Menu)
            .await?;
        require_single_guarded_refusal(&outcome, FrontPanelKey::Menu, "changed-context")?;
        journal_guarded_refusal(
            journal,
            "changed-context",
            Some(FrontPanelKey::Menu),
            FrontPanelKey::Menu,
            started.elapsed(),
            &outcome,
            &json!({
                "frozen_frame": "byte-identical-to-qualified-top-level-Menu-frame",
                "context_change": "MENU-returned-top-level-Menu-to-home-before-GM-G",
                "guarded_probe_if-dispatched": "MENU-would-reopen-top-level-Menu",
                "post_refusal_proof": "recorded-in-following-masked-dual-band-home-assertion",
            }),
        )?;

        tokio::time::sleep(SETTLE_DELAY).await;
        let post_refusal = capture_home_quiescent(
            session,
            output_dir,
            journal,
            "automation-changed-context-post-refusal-home",
        )
        .await?;
        let comparison = compare_dual_band_home(&post_refusal, baseline)?;
        journal_home_comparison(
            journal,
            "changed-context-canary-post-refusal",
            None,
            &post_refusal,
            baseline,
            &comparison,
        )?;
        if !comparison.restored() {
            return Err(io::Error::other(
                "V1.03.AZM changed-context canary did not restore the reviewed V1.03 dual-band home profile",
            )
            .into());
        }
        journal.append(json!({
            "type": "automation-guard-canary-screen-assertion",
            "scenario": "changed-context",
            "baseline_crc32": format!("{:08X}", baseline.crc32),
            "post_refusal_crc32": format!("{:08X}", post_refusal.crc32),
            "full_frame_equal": post_refusal.frame == baseline.frame,
            "comparison": "masked-stable-pixels-plus-ordered-frequency-mode-anchor-text",
            "mask_id": HOME_MASK_ID,
            "meaning": "the first MENU restored home and the refused guarded MENU probe did not reopen Menu",
            "result": "pass",
        }))
    }

    async fn verify_zero_hold_batch_route_canary(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
    ) -> AuditResult<()> {
        let manifest = parse_menu_manifest(REVIEWED_MANUAL)?;
        let firmware_entry = manifest_entry(&manifest, "991")?;
        let _menu_receipt = tap(
            session,
            journal,
            FrontPanelKey::Menu,
            "automation-zero-hold-canary-open-menu",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let menu_gate = capture_canary_quiescent(
            session,
            output_dir,
            journal,
            "automation-zero-hold-991-menu-gate",
        )
        .await?;
        require_top_level_menu(&menu_gate)?;

        journal.append(json!({
            "type": "automation-zero-hold-route-canary-intent",
            "route": "991",
            "expected_title": anchor_page_title(firmware_entry),
            "expected_value": "V1.03.AZM",
            "expected_restoration": "reviewed-v1.03-masked-dual-band-home-oracle",
            "runtime_qualified_before_canary": true,
            "zero_hold_behavior_qualified_before_canary": false,
        }))?;
        let started = Instant::now();
        dispatch_complete_menu_number(
            session,
            output_dir,
            journal,
            "startup-canary-991",
            "991",
            &menu_gate,
            "automation-zero-hold-route-live-canary",
        )
        .await?;
        let route_elapsed = started.elapsed();

        tokio::time::sleep(SETTLE_DELAY).await;
        let value = capture_canary_quiescent(
            session,
            output_dir,
            journal,
            "automation-zero-hold-991-value",
        )
        .await?;
        let expected_title = anchor_page_title(firmware_entry);
        let title_match = screen_matches_label(&value, expected_title);
        let payload = firmware_version_payload(&value);
        if !title_match || payload.is_none() {
            return Err(io::Error::other(
                "V1.03.AZM zero-hold route 991 did not produce the exact Version / V1.03.AZM information page",
            )
            .into());
        }
        journal.append(json!({
            "type": "automation-zero-hold-route-canary-screen",
            "route": "991",
            "expected_title": expected_title,
            "title_match": title_match,
            "validated_payload": payload,
            "route_and_deferred_evidence_elapsed_ms": millis(route_elapsed),
            "result": "pass",
        }))?;

        restore_home(
            session,
            output_dir,
            journal,
            "startup-zero-hold-991",
            baseline,
        )
        .await?;
        journal.append(json!({
            "type": "automation-zero-hold-route-qualification",
            "route": "991",
            "physical_behavior": "three-synchronous-zero-hold-press-release-pairs",
            "screen_result": "exact-Version-title-and-V1.03.AZM-value",
            "restoration": "reviewed-v1.03-masked-dual-band-home-oracle",
            "host_ocr_io_to_key_race_removed": true,
            "residual_concurrent_framebuffer_writer_toctou": true,
            "qualified_after_live_canary": true,
            "result": "pass",
        }))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the live command-4 canary keeps its intent, fresh lease, exact receipt checks, evidence, and masked home-screen proof together for auditability"
    )]
    async fn verify_zero_prefix_batch_refusal_canary(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
    ) -> AuditResult<()> {
        let _menu_receipt = tap(
            session,
            journal,
            FrontPanelKey::Menu,
            "automation-command-4-refusal-open-menu",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let menu_gate = capture_canary_quiescent(
            session,
            output_dir,
            journal,
            "automation-command-4-zero-prefix-menu-gate",
        )
        .await?;
        require_top_level_menu(&menu_gate)?;
        let route = GuardedDecimalRoute::new([9, 9, 1])?;
        journal.append(json!({
            "type": "automation-guard-canary-intent",
            "scenario": "command-4-zero-prefix-context-refusal",
            "frozen_context": "top-level-Menu",
            "context_change_key": format!("{:?}", FrontPanelKey::Menu),
            "guarded_route": route.to_string(),
            "required_receipt": {
                "wire_status": "02",
                "firmware_command": 4,
                "guard_count": 1,
                "completed_taps": 0,
                "event_mask": "00",
            },
        }))?;
        // Refresh only after all OCR and journal work so no filesystem I/O
        // consumes the short guarded-route lease.
        let frozen_menu = session.capture_screen().await?;
        if frozen_menu.frame != menu_gate.frame || frozen_menu.metadata.crc32 != menu_gate.crc32 {
            return Err(io::Error::other(
                "V1.03.AZM command-4 refusal snapshot no longer matches the qualified top-level Menu frame",
            )
            .into());
        }
        let started = Instant::now();
        let outcome = session
            .verify_decimal_route_changed_context_refusal(&frozen_menu, FrontPanelKey::Menu, route)
            .await?;
        let GuardedDecimalRouteOutcome::ContextChanged(receipt) = &outcome else {
            return Err(io::Error::other(
                "V1.03.AZM command-4 canary did not return an authenticated context refusal",
            )
            .into());
        };
        if receipt.route != route
            || receipt.guard_count != 1
            || receipt.completed_taps != 0
            || receipt.event_mask != 0
            || receipt.metadata.last_command != 4
            || receipt.metadata.last_key_result != 2
            || receipt.metadata.last_key != u32::from(FrontPanelKey::Pf1_9.as_raw())
            || receipt.metadata.last_phase != 0
        {
            return Err(io::Error::other(
                "V1.03.AZM command-4 refusal receipt was not the exact zero-prefix contract",
            )
            .into());
        }
        journal.append(json!({
            "type": "automation-guard-canary-receipt",
            "scenario": "command-4-zero-prefix-context-refusal",
            "wire_reply_status": "02-exact-echo-authenticated",
            "firmware_command": 4,
            "route": receipt.route.to_string(),
            "sequence": receipt.sequence,
            "guard_count": receipt.guard_count,
            "completed_taps": receipt.completed_taps,
            "event_mask": format!("{:02X}", receipt.event_mask),
            "first_refused_key": format!("{:?}", FrontPanelKey::Pf1_9),
            "dispatch": "zero-prefix-no-input-event",
            "session_remains_usable": !outcome.requires_recovery(),
            "elapsed_ms": millis(started.elapsed()),
            "metadata": metadata_json(&receipt.metadata),
            "result": "pass",
        }))?;

        tokio::time::sleep(SETTLE_DELAY).await;
        let post_refusal = capture_home_quiescent(
            session,
            output_dir,
            journal,
            "automation-command-4-zero-prefix-post-refusal-home",
        )
        .await?;
        let comparison = compare_dual_band_home(&post_refusal, baseline)?;
        journal_home_comparison(
            journal,
            "command-4-zero-prefix-canary-post-refusal",
            None,
            &post_refusal,
            baseline,
            &comparison,
        )?;
        if !comparison.restored() {
            return Err(io::Error::other(
                "V1.03.AZM command-4 zero-prefix refusal did not restore the reviewed V1.03 dual-band home profile",
            )
            .into());
        }
        journal.append(json!({
            "type": "automation-guard-canary-screen-assertion",
            "scenario": "command-4-zero-prefix-context-refusal",
            "baseline_crc32": format!("{:08X}", baseline.crc32),
            "post_refusal_crc32": format!("{:08X}", post_refusal.crc32),
            "full_frame_equal": post_refusal.frame == baseline.frame,
            "comparison": "masked-stable-pixels-plus-ordered-frequency-mode-anchor-text",
            "mask_id": HOME_MASK_ID,
            "result": "pass",
        }))
    }

    async fn capture_canary_quiescent(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        stem: &str,
    ) -> AuditResult<CapturedScreen> {
        let first =
            capture_screen(session, output_dir, journal, &format!("{stem}-q1"), None).await?;
        tokio::time::sleep(QUIESCENCE_DELAY).await;
        let second =
            capture_screen(session, output_dir, journal, &format!("{stem}-q2"), None).await?;
        tokio::time::sleep(QUIESCENCE_DELAY).await;
        let third =
            capture_screen(session, output_dir, journal, &format!("{stem}-q3"), None).await?;
        let stable =
            three_frames_are_identical(&first.frame, &second.frame, &third.frame).then_some(third);
        journal.append(json!({
            "type": "screen-quiescence",
            "scope": "pre-audit-automation-guard-canary",
            "stem": stem,
            "samples": 3,
            "crc32": stable.as_ref().map(|screen| format!("{:08X}", screen.crc32)),
            "result": if stable.is_some() { "pass" } else { "fail" },
        }))?;
        stable.ok_or_else(|| {
            io::Error::other(format!(
                "pre-audit V1.03.AZM canary screen {stem:?} was not identical across three consecutive captures"
            ))
            .into()
        })
    }

    async fn audit_menu_102(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        baseline: &CapturedScreen,
        before: &ConfigurationSnapshot,
        summary: &mut Summary,
    ) -> AuditResult<()> {
        let primary = async {
            prepare_menu_102_runtime(session, output_dir, journal, baseline).await?;
            audit_entry(
                session, output_dir, journal, entry, baseline, before, summary,
            )
            .await
        }
        .await;
        if primary.is_ok() || !session.is_valid() {
            return primary;
        }
        let cleanup = restore_menu_102_runtime(
            session,
            output_dir,
            journal,
            baseline,
            "after-menu-102-attempt-failure",
        )
        .await;
        combine_primary_and_cleanup_errors(primary, [("menu-102-runtime-restoration", cleanup)])
    }

    async fn prepare_menu_102_runtime(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
    ) -> AuditResult<()> {
        let baseline_comparison = compare_dual_band_home(baseline, baseline)?;
        let baseline_operation_band = observed_operation_band(baseline).ok_or_else(|| {
            io::Error::other(
                "Menu 102 preparation requires one unambiguous baseline PTT/operation-band marker",
            )
        })?;
        if !baseline_comparison.restored() {
            return Err(io::Error::other(
                "Menu 102 preparation did not start from the qualified dual-band home baseline",
            )
            .into());
        }
        journal.append(json!({
            "type": "menu-102-runtime-prerequisite-intent",
            "stock_v1_03_predicate": "Band-B plus single-band display in an ordinary idle non-DV/non-DR/non-FM-radio/non-KISS context",
            "existing_band_b_context": "reviewed analog home-mode anchor",
            "baseline_operation_band": format!("{baseline_operation_band:?}"),
            "transition": ["A/B if baseline operation band is A", "F", "A/B"],
            "persistent_mcp_configuration_changed": false,
            "usb_function_is_not_an_entry_prerequisite": true,
        }))?;

        if baseline_operation_band == Band::A {
            let _receipt = tap(
                session,
                journal,
                FrontPanelKey::Ab,
                "menu-102-select-operation-band-b",
            )
            .await?;
            tokio::time::sleep(SETTLE_DELAY).await;
            let band_b = capture_screen(
                session,
                output_dir,
                journal,
                "102-prerequisite-dual-band-b",
                Some("102"),
            )
            .await?;
            let comparison = compare_dual_band_home(&band_b, baseline)?;
            let visible_band = observed_operation_band(&band_b);
            let passed = comparison.semantic_profile_valid && visible_band == Some(Band::B);
            journal.append(json!({
                "type": "menu-102-runtime-prerequisite-screen",
                "phase": "dual-band-operation-band-b",
                "operation_band": visible_band.map(|band| format!("{band:?}")),
                "expected_operation_band": format!("{:?}", Band::B),
                "ordered_frequency_anchors_match": comparison.semantic_profile_valid,
                "masked_differing_pixels_from_baseline": comparison.masked_differing_pixels,
                "pixel_difference_expected_for_rendered_ptt_marker_move": true,
                "result": if passed { "pass" } else { "fail" },
            }))?;
            if !passed {
                return Err(io::Error::other(
                    "A/B did not produce the reviewed dual-band Band B operating context required by Menu 102",
                )
                .into());
            }
        }

        tap_function_ab_toggle(session, journal, "menu-102-enter-single-band-b").await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let single_band = capture_screen(
            session,
            output_dir,
            journal,
            "102-prerequisite-single-band-b",
            Some("102"),
        )
        .await?;
        let passed = is_reviewed_single_band_b_home(&single_band, baseline);
        journal.append(json!({
            "type": "menu-102-runtime-prerequisite-screen",
            "phase": "single-band-b",
            "frequency_anchors": home_frequency_anchors(&single_band).iter().map(home_anchor_json).collect::<Vec<_>>(),
            "known_analog_mode_anchor": screen_has_known_analog_mode(&single_band),
            "no_menu_or_overlay": !baseline_has_disallowed_home_layout(&single_band),
            "documented_front_panel_sequence": ["F", "A/B"],
            "persistent_mcp_configuration_changed": false,
            "result": if passed { "pass" } else { "fail" },
        }))?;
        if passed {
            Ok(())
        } else {
            Err(io::Error::other(
                "F then A/B did not produce the reviewed single-band Band B home context required by Menu 102",
            )
            .into())
        }
    }

    async fn audit_entry(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        baseline: &CapturedScreen,
        before: &ConfigurationSnapshot,
        summary: &mut Summary,
    ) -> AuditResult<()> {
        summary.attempted = summary.attempted.saturating_add(1);
        journal.set_active_menu(Some(&entry.number));
        journal.append(json!({
            "type": "menu-entry-start",
            "menu_number": entry.number,
            "label": entry.label,
            "category_path": entry.category_path,
            "description": entry.description,
            "setting_values": entry.setting_values,
            "class": entry.class.as_str(),
            "row_only_policy": (entry.class == AuditClass::RowOnly)
                .then(|| row_only_policy(&entry.number).map(RowOnlyPolicy::as_str))
                .transpose()?,
            "compatibility": if entry.number == "980" {
                "stock-v1.03-number-title-and-schema; custom-automation-usb-storage-apply-path"
            } else {
                "stock-v1.03-compatible"
            },
            "value_scope": match entry.class {
                AuditClass::RowOnly
                    if row_only_policy(&entry.number)? == RowOnlyPolicy::SafeInspection =>
                {
                    "read-only-page-screen-oracle-backed-by-before-audit-MCP-where-available"
                }
                AuditClass::RowOnly => "none-row-locator-only-never-entered",
                _ => "current-radio-value-or-live-information-not-default",
            },
        }))?;

        let _menu_receipt = tap(session, journal, FrontPanelKey::Menu, "open-menu").await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let gate = capture_quiescent(
            session,
            output_dir,
            journal,
            &format!("{}-menu-gate", entry.number),
            &entry.number,
        )
        .await?;
        require_top_level_menu(&gate)?;
        journal.append(json!({
            "type": "menu-context-gate",
            "menu_number": entry.number,
            "crc32": format!("{:08X}", gate.crc32),
            "host_samples": 1,
            "firmware_stable_snapshot": true,
            "result": "pass",
        }))?;

        let row_policy = (entry.class == AuditClass::RowOnly)
            .then(|| row_only_policy(&entry.number))
            .transpose()?;
        let (row_match, value_result) = if row_policy == Some(RowOnlyPolicy::SafeInspection) {
            audit_safe_inspection(session, output_dir, journal, entry, &gate, before).await?
        } else if entry.class == AuditClass::RowOnly {
            audit_row_only(session, output_dir, journal, entry, &gate).await?
        } else if entry.number == "91A" {
            audit_91a_value(session, output_dir, journal, entry, &gate, summary).await?
        } else {
            audit_direct_value(session, output_dir, journal, entry, &gate, summary).await?
        };
        if row_policy == Some(RowOnlyPolicy::SafeInspection) {
            if row_match {
                summary.located_rows = summary.located_rows.saturating_add(1);
                summary.row_only_safe_inspected = summary.row_only_safe_inspected.saturating_add(1);
            } else {
                summary.inconclusive = summary.inconclusive.saturating_add(1);
            }
        } else if entry.class == AuditClass::RowOnly {
            if row_match {
                summary.located_rows = summary.located_rows.saturating_add(1);
                summary.row_only_located_not_entered =
                    summary.row_only_located_not_entered.saturating_add(1);
            } else {
                summary.inconclusive = summary.inconclusive.saturating_add(1);
            }
        }

        restore_home(session, output_dir, journal, &entry.number, baseline).await?;
        summary.restored = summary.restored.saturating_add(1);
        journal.append(json!({
            "type": "menu-entry-end",
            "menu_number": entry.number,
            "row_result": if row_match { "pass" } else { "inconclusive" },
            "value_result": value_result,
            "restore_result": "pass",
        }))?;
        println!(
            "menu={} class={} row={} value={} restore=pass",
            entry.number,
            entry.class.as_str(),
            if row_match { "pass" } else { "inconclusive" },
            value_result
        );
        journal.set_active_menu(None);
        Ok(())
    }

    /// Dispatch one complete decimal menu number under one fresh V1.03.AZM
    /// firmware-enforced raw-frame route lease.
    ///
    /// One V1.03.AZM snapshot authenticates the start context, and one consumed
    /// `GM RDDD,SS` transaction conditionally dispatches all three zero-hold
    /// taps in one synchronous handler invocation after exactly one guard and
    /// before any host turn. The exclusive host issues commands sequentially;
    /// no capture, metadata read,
    /// OCR, BMP write, journal write, or host transport turn occurs between
    /// digits.
    async fn dispatch_complete_menu_number(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        evidence_menu_number: &str,
        dispatched_number: &str,
        initial_gate: &CapturedScreen,
        purpose: &str,
    ) -> AuditResult<()> {
        let route = direct_access_route(dispatched_number)?;
        let keys = direct_access_keys(dispatched_number)?;
        let (snapshot, capture_started_unix_ms, capture_round_trip) =
            capture_numeric_route_snapshot(session, initial_gate).await?;
        let dispatch_started = Instant::now();
        let outcome = session.guarded_decimal_route(&snapshot, route).await?;
        let evidence = NumericRouteEvidence {
            snapshot,
            route,
            requested_keys: keys,
            capture_started_unix_ms,
            capture_round_trip,
            dispatch_elapsed: dispatch_started.elapsed(),
            outcome,
        };
        persist_numeric_dispatch_evidence(
            output_dir,
            journal,
            evidence_menu_number,
            dispatched_number,
            purpose,
            &evidence,
        )?;
        match &evidence.outcome {
            GuardedDecimalRouteOutcome::Dispatched(receipt)
                if receipt.route == route
                    && receipt.guard_count == 1
                    && receipt.completed_taps == 3
                    && receipt.event_mask == 0x3F =>
            {
                Ok(())
            }
            GuardedDecimalRouteOutcome::Dispatched(_) => Err(io::Error::other(format!(
                "V1.03.AZM returned a malformed completed receipt for numeric route {dispatched_number}"
            ))
            .into()),
            GuardedDecimalRouteOutcome::ContextChanged(receipt) => Err(io::Error::other(format!(
                "V1.03.AZM refused numeric route {dispatched_number} before input with its authenticated zero-prefix receipt (completed taps: {})",
                receipt.completed_taps
            ))
            .into()),
            _ => Err(io::Error::other(format!(
                "V1.03.AZM returned an unsupported guarded-route outcome for numeric route {dispatched_number}"
            ))
            .into()),
        }
    }

    async fn audit_direct_value(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        menu_gate: &CapturedScreen,
        summary: &mut Summary,
    ) -> AuditResult<(bool, &'static str)> {
        dispatch_complete_menu_number(
            session,
            output_dir,
            journal,
            &entry.number,
            &entry.number,
            menu_gate,
            "direct-menu-number",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let landing = capture_quiescent(
            session,
            output_dir,
            journal,
            &format!("{}-current-value", entry.number),
            &entry.number,
        )
        .await?;
        let expected_title = anchor_page_title(entry);
        let (value, entry_path) =
            enter_direct_value_page(session, output_dir, journal, entry, expected_title, landing)
                .await?;
        let title_match = screen_matches_label(&value, expected_title);
        let (validated_payload, payload_source, requires_specialized_validator) =
            direct_value_page_payload(session, output_dir, journal, entry, &value, title_match)
                .await?;
        let current_text = current_value_text(&value);
        let (validated_payload, payload_source, numbered_row_proved) =
            complete_numbered_row_payload(
                session,
                output_dir,
                journal,
                entry,
                title_match,
                validated_payload,
                payload_source,
            )
            .await?;
        let payload_matches = validated_payload.is_some();
        let observed = title_match && payload_matches;
        if title_match {
            summary.located_rows = summary.located_rows.saturating_add(1);
        } else {
            summary.inconclusive = summary.inconclusive.saturating_add(1);
        }
        let value_result = if observed {
            summary.value_or_information_validated =
                summary.value_or_information_validated.saturating_add(1);
            "observed-validated"
        } else if requires_specialized_validator {
            summary.inconclusive = summary.inconclusive.saturating_add(1);
            "inconclusive-specialized-validator"
        } else {
            summary.inconclusive = summary.inconclusive.saturating_add(1);
            "inconclusive-no-unique-documented-legal-value"
        };
        journal.append(json!({
            "type": "current-value-observation",
            "menu_number": entry.number,
            "class": entry.class.as_str(),
            "expected_title": expected_title,
            "entry_path": entry_path,
            "title_result": if title_match { "pass" } else { "inconclusive" },
            "selected_text": value.selected,
            "current_text": current_text,
            "validated_payload": validated_payload,
            "payload_source": payload_source,
            "requires_specialized_validator": requires_specialized_validator,
            "ordinary_value_policy": "one-authenticated-value-locus-with-one-complete-reviewed-typed-value-and-no-conflicting-text; source is an exact selection band, a reviewed centered-scalar page body, or an exact 40-pixel numbered-row subordinate lane",
            "result": value_result,
            "next_key_invariant": if numbered_row_proved {
                "MODE-to-exact-numbered-row-proof-completed-before-row-payload"
            } else {
                "MODE-to-exact-numbered-row-proof"
            },
        }))?;
        if !numbered_row_proved {
            let _numbered_row = prove_numbered_row_after_value(
                session,
                output_dir,
                journal,
                entry,
                &format!("{}-value-backout-numbered-row", entry.number),
            )
            .await?;
        }
        Ok((title_match, value_result))
    }

    async fn direct_value_page_payload(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        value: &CapturedScreen,
        title_match: bool,
    ) -> AuditResult<(Option<Vec<String>>, &'static str, bool)> {
        let requires_specialized_validator = SPECIALIZED_PAYLOAD_NUMBERS
            .split_ascii_whitespace()
            .any(|number| number == entry.number);
        let centered_scalar = CENTERED_SCALAR_NUMBERS
            .split_ascii_whitespace()
            .any(|number| number == entry.number);
        let payload_source = if centered_scalar {
            "centered-scalar-page-body"
        } else {
            "value-page-selection-band"
        };
        let payload = if !title_match {
            None
        } else if matches!(entry.number.as_str(), "551" | "631") {
            audit_scrollable_checkbox_payload(session, output_dir, journal, entry, value).await?
        } else if requires_specialized_validator {
            specialized_payload(&entry.number, value)
        } else if entry.number == "991" {
            firmware_version_payload(value)
        } else if centered_scalar {
            centered_scalar_documented_payload(entry, value)
        } else {
            ordinary_documented_payload(entry, value)
        };
        Ok((payload, payload_source, requires_specialized_validator))
    }

    async fn complete_numbered_row_payload(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        title_match: bool,
        payload: Option<Vec<String>>,
        payload_source: &'static str,
    ) -> AuditResult<(Option<Vec<String>>, &'static str, bool)> {
        if !title_match || payload.is_some() {
            return Ok((payload, payload_source, false));
        }
        // Some stock V1.03 editors render their current value as a centered
        // scalar with no selection band; others render a selected list as
        // thin outlines that are not a complete row band. The same current
        // value is also rendered in the subordinate lane of the exact 40-pixel
        // numbered row. Fuse the two authenticated screens only after the
        // first proves the exact page title and the second proves the exact
        // locator and label. The lower lane must still contain one complete
        // reviewed typed value identity with no conflicting physical locus.
        let row = prove_numbered_row_after_value(
            session,
            output_dir,
            journal,
            entry,
            &format!("{}-value-backout-numbered-row", entry.number),
        )
        .await?;
        Ok((
            numbered_row_documented_payload(entry, &row),
            "exact-numbered-row-subordinate-value-locus",
            true,
        ))
    }

    async fn enter_direct_value_page(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        expected_title: &str,
        landing: CapturedScreen,
    ) -> AuditResult<(CapturedScreen, &'static str)> {
        if screen_matches_label(&landing, expected_title) {
            return Ok((landing, "direct-page"));
        }
        if !numbered_row_matches(&landing, &entry.number, &entry.label) {
            return Err(io::Error::other(format!(
                "menu {} direct value route landed on neither its exact numbered row nor exact reviewed page title {expected_title:?}",
                entry.number
            ))
            .into());
        }
        journal.append(json!({
            "type": "direct-value-numbered-row-proof",
            "phase": "before-read-only-entry",
            "menu_number": entry.number,
            "expected_label": entry.label,
            "expected_locator": entry.number,
            "selected_text": journal_selected_text(&landing.selected, false)?,
            "result": "pass",
        }))?;
        let _enter_receipt = tap(
            session,
            journal,
            FrontPanelKey::Ab,
            "activate-reviewed-value-page-OK",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let page = capture_quiescent(
            session,
            output_dir,
            journal,
            &format!("{}-current-value-after-row-OK", entry.number),
            &entry.number,
        )
        .await?;
        if !screen_matches_label(&page, expected_title) {
            return Err(io::Error::other(format!(
                "menu {} exact numbered row did not open its reviewed value page {expected_title:?} after one A/B soft-key OK",
                entry.number
            ))
            .into());
        }
        Ok((page, "exact-numbered-row-then-one-A/B-OK"))
    }

    async fn audit_row_only(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        menu_gate: &CapturedScreen,
    ) -> AuditResult<(bool, &'static str)> {
        let manifest = parse_menu_manifest(REVIEWED_MANUAL)?;
        let anchor_number = row_only_anchor(&entry.number)?;
        let anchor = manifest_entry(&manifest, anchor_number)?;
        if !matches!(anchor.class, AuditClass::Value | AuditClass::Information) {
            return Err(invalid_input(format!(
                "row-only anchor {} for menu {} is not harmless",
                anchor.number, entry.number
            )));
        }
        journal.append(json!({
            "type": "row-only-route",
            "menu_number": entry.number,
            "anchor_number": anchor.number,
            "anchor_label": anchor.label,
            "policy": "complete-anchor-and-exact-transition-captures",
        }))?;

        let anchor_row = open_complete_anchor_row(
            session,
            output_dir,
            journal,
            anchor,
            &entry.number,
            menu_gate,
        )
        .await?;
        let row = navigate_from_anchor_row(
            session, output_dir, journal, &manifest, anchor, entry, anchor_row,
        )
        .await?;
        let exact_numbered_row = numbered_row_matches(&row, &entry.number, &entry.label);
        let exact_singleton_submenu = entry.number == "710"
            && menu_710_singleton_memory_submenu_matches(&row)
            && menu_710_is_exact_reviewed_singleton(&manifest, entry);
        if !exact_numbered_row && !exact_singleton_submenu {
            return Err(io::Error::other(format!(
                "menu {} row-only route produced neither its exact numbered row nor its reviewed singleton-submenu locator",
                entry.number
            ))
            .into());
        }
        journal.append(json!({
            "type": "row-only-observation",
            "menu_number": entry.number,
            "expected_label": entry.label,
            "expected_locator": if exact_singleton_submenu { "FM Broadcasting / 71- / Memory (sole reviewed leaf 710)" } else { entry.number.as_str() },
            "selected_text": row.selected,
            "exact_numbered_row": exact_numbered_row,
            "exact_singleton_submenu": exact_singleton_submenu,
            "locator_kind": if exact_singleton_submenu { "exact-stock-v1.03-singleton-submenu" } else { "exact-numbered-row" },
            "entered": false,
            "result": "pass",
        }))?;
        Ok((true, "not-entered"))
    }

    async fn audit_safe_inspection(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        menu_gate: &CapturedScreen,
        before: &ConfigurationSnapshot,
    ) -> AuditResult<(bool, &'static str)> {
        let oracle = safe_inspection_oracle(&entry.number)?;
        journal.append(json!({
            "type": "safe-inspection-route",
            "menu_number": entry.number,
            "policy": "one-guarded-automation-complete-route-from-exact-top-menu; accept only the exact numbered row or exact page title; exact row receives one documented A/B soft-key OK action to enter its reviewed read-only page; no key while observing; one MODE to exact numbered row",
            "oracle": safe_inspection_oracle_name(oracle),
            "before_mcp_snapshot": before.artifact,
        }))?;
        dispatch_complete_menu_number(
            session,
            output_dir,
            journal,
            &entry.number,
            &entry.number,
            menu_gate,
            "direct-safe-inspection",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;

        let expected_title = safe_inspection_title(entry);
        let landing = capture_screen(
            session,
            output_dir,
            journal,
            &format!("{}-safe-inspection-landing", entry.number),
            Some(&entry.number),
        )
        .await?;
        let (screen, entry_path) = enter_safe_inspection_page(
            session,
            output_dir,
            journal,
            entry,
            expected_title,
            landing,
        )
        .await?;

        if !has_one_rendered_bottom_left_control(&screen, "back") {
            return Err(io::Error::other(format!(
                "menu {} reviewed inspection page did not prove MODE is the bottom-left Back soft key; refusing to dispatch it",
                entry.number
            ))
            .into());
        }

        let row = tap_capture_selected_exact(
            session,
            output_dir,
            journal,
            FrontPanelKey::Mode,
            "safe-inspection-one-MODE-to-numbered-row",
            &entry.label,
            &format!("{}-safe-inspection-numbered-row", entry.number),
            &entry.number,
        )
        .await?;
        if !numbered_row_matches(&row, &entry.number, &entry.label) {
            return Err(io::Error::other(format!(
                "menu {} safe inspection did not return with one MODE to its exact numbered row",
                entry.number
            ))
            .into());
        }
        journal.append(json!({
            "type": "safe-inspection-numbered-row-proof",
            "menu_number": entry.number,
            "expected_label": entry.label,
            "expected_locator": entry.number,
            "selected_text": journal_selected_text(&row.selected, false)?,
            "MODE_count_since_observation": 1,
            "result": "pass",
        }))?;

        // Validate the retained screen only after MODE has returned to the
        // exact numbered row. A title/OCR/MCP-oracle failure is diagnostic,
        // not a reason to strand the radio on an information page. Several
        // reviewed pages contain a blinking input cursor or live clock, so
        // one stable double-copy V1.03.AZM framebuffer remains the evidence unit.
        if !screen_matches_label(&screen, expected_title) {
            return Err(io::Error::other(format!(
                "menu {} safe inspection did not show the exact reviewed title {expected_title:?}",
                entry.number
            ))
            .into());
        }
        let payload = safe_inspection_payload(&entry.number, oracle, &screen, before)?;
        journal.append(json!({
            "type": "safe-inspection-observation",
            "menu_number": entry.number,
            "expected_title": expected_title,
            "title_result": "pass",
            "entry_path": entry_path,
            "oracle": safe_inspection_oracle_name(oracle),
            "oracle_evidence": payload,
            "keys_while_observing": [],
            "value_toggle_confirmation_or_navigation_keys_dispatched": false,
            "viewport_scope": "complete-page-specific-oracle",
            "cleanup_before_semantic_validation": "one-MODE-to-exact-numbered-row",
            "result": "pass",
        }))?;
        Ok((true, "safe-inspection-validated"))
    }

    async fn enter_safe_inspection_page(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        expected_title: &str,
        landing: CapturedScreen,
    ) -> AuditResult<(CapturedScreen, &'static str)> {
        if screen_matches_label(&landing, expected_title) {
            return Ok((landing, "direct-page"));
        }
        if !numbered_row_matches(&landing, &entry.number, &entry.label) {
            return Err(io::Error::other(format!(
                "menu {} safe inspection landed on neither its exact numbered row nor exact reviewed page title {expected_title:?}",
                entry.number
            ))
            .into());
        }
        journal.append(json!({
            "type": "safe-inspection-numbered-row-proof",
            "phase": "before-read-only-entry",
            "menu_number": entry.number,
            "expected_label": entry.label,
            "expected_locator": entry.number,
            "selected_text": journal_selected_text(&landing.selected, false)?,
            "result": "pass",
        }))?;
        let _enter_receipt = tap(
            session,
            journal,
            FrontPanelKey::Ab,
            "activate-reviewed-read-only-safe-inspection-OK",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let page = capture_screen(
            session,
            output_dir,
            journal,
            &format!("{}-safe-inspection", entry.number),
            Some(&entry.number),
        )
        .await?;
        Ok((page, "exact-numbered-row-then-one-A/B-OK"))
    }

    async fn audit_91a_value(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        menu_gate: &CapturedScreen,
        summary: &mut Summary,
    ) -> AuditResult<(bool, &'static str)> {
        let manifest = parse_menu_manifest(REVIEWED_MANUAL)?;
        let anchor = manifest_entry(&manifest, "919")?;
        let anchor_row = open_complete_anchor_row(
            session,
            output_dir,
            journal,
            anchor,
            &entry.number,
            menu_gate,
        )
        .await?;
        let row = navigate_from_anchor_row(
            session, output_dir, journal, &manifest, anchor, entry, anchor_row,
        )
        .await?;
        if !numbered_row_matches(&row, &entry.number, &entry.label) {
            return Err(navigation_mismatch(
                &entry.number,
                &entry.label,
                &row.selected,
            ));
        }
        summary.located_rows = summary.located_rows.saturating_add(1);
        let _enter_receipt = tap(
            session,
            journal,
            FrontPanelKey::Ab,
            "activate-91A-read-only-OK",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let value = capture_quiescent(
            session,
            output_dir,
            journal,
            "91A-current-value",
            &entry.number,
        )
        .await?;
        if !screen_matches_label(&value, &entry.label) {
            return Err(io::Error::other(format!(
                "menu 91A did not show exact reviewed title {:?} after its exact row was entered",
                entry.label
            ))
            .into());
        }
        let current_text = current_value_text(&value);
        let validated_payload = centered_scalar_documented_payload(entry, &value);
        let result = if validated_payload.is_some() {
            summary.value_or_information_validated =
                summary.value_or_information_validated.saturating_add(1);
            "observed-validated"
        } else {
            summary.inconclusive = summary.inconclusive.saturating_add(1);
            "inconclusive-no-unique-documented-legal-value"
        };
        journal.append(json!({
            "type": "current-value-observation",
            "menu_number": entry.number,
            "selected_text": value.selected,
            "current_text": current_text,
            "validated_payload": validated_payload,
            "ordinary_value_policy": "one-centered-complete-reviewed-typed-value-with-no-selection-band-and-no-conflicting-body-text",
            "result": result,
            "next_key_invariant": "MODE-to-exact-numbered-row-proof",
        }))?;
        let _numbered_row = prove_numbered_row_after_value(
            session,
            output_dir,
            journal,
            entry,
            "91A-value-backout-numbered-row",
        )
        .await?;
        Ok((true, result))
    }

    async fn prove_numbered_row_after_value(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        suffix: &str,
    ) -> AuditResult<CapturedScreen> {
        let row = tap_capture_selected_exact(
            session,
            output_dir,
            journal,
            FrontPanelKey::Mode,
            "value-page-back-to-numbered-row",
            &entry.label,
            suffix,
            &entry.number,
        )
        .await?;
        let locator_matches = screen_has_exact_menu_locator(&row, &entry.number);
        journal.append(json!({
            "type": "value-page-numbered-row-proof",
            "menu_number": entry.number,
            "expected_label": entry.label,
            "expected_locator": entry.number,
            "selected_text": row.selected,
            "label_result": if selected_matches_label(&row, &entry.label) { "pass" } else { "fail" },
            "locator_result": if locator_matches { "pass" } else { "fail" },
            "result": if locator_matches { "pass" } else { "fail" },
        }))?;
        if !locator_matches {
            return Err(io::Error::other(format!(
                "menu {} value page backed out to label {:?} without its one exact numbered-row locator",
                entry.number, entry.label
            ))
            .into());
        }
        Ok(row)
    }

    async fn open_complete_anchor_row(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        anchor: &MenuEntry,
        target_number: &str,
        menu_gate: &CapturedScreen,
    ) -> AuditResult<CapturedScreen> {
        dispatch_complete_menu_number(
            session,
            output_dir,
            journal,
            target_number,
            &anchor.number,
            menu_gate,
            "direct-harmless-anchor",
        )
        .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let page = capture_quiescent(
            session,
            output_dir,
            journal,
            &format!("{target_number}-anchor-{}-page", anchor.number),
            target_number,
        )
        .await?;
        let expected_title = anchor_page_title(anchor);
        if !screen_matches_label(&page, expected_title) {
            return Err(io::Error::other(format!(
                "menu {target_number} harmless anchor {} did not show exact title {expected_title:?}",
                anchor.number
            ))
            .into());
        }

        let row = tap_capture_selected_exact(
            session,
            output_dir,
            journal,
            FrontPanelKey::Mode,
            "anchor-page-back-to-row",
            &anchor.label,
            &format!("{target_number}-anchor-{}-row", anchor.number),
            target_number,
        )
        .await?;
        if !numbered_row_matches(&row, &anchor.number, &anchor.label) {
            return Err(io::Error::other(format!(
                "menu {target_number} harmless anchor {} did not back out to its exact numbered row",
                anchor.number
            ))
            .into());
        }
        Ok(row)
    }

    async fn navigate_from_anchor_row(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        manifest: &[MenuEntry],
        anchor: &MenuEntry,
        target: &MenuEntry,
        anchor_row: CapturedScreen,
    ) -> AuditResult<CapturedScreen> {
        let (anchor_category, anchor_submenu) = category_parts(&anchor.category_path)?;
        let (target_category, target_submenu) = category_parts(&target.category_path)?;
        if anchor_category != target_category {
            return Err(invalid_input(format!(
                "menu {} anchor {} crosses top-level categories {:?} and {:?}",
                target.number, anchor.number, anchor_category, target_category
            )));
        }

        let (current_row, row_start_number) = if anchor.category_path == target.category_path {
            (anchor_row, anchor.number.as_str())
        } else {
            let anchor_submenu_screen = tap_capture_selected_exact(
                session,
                output_dir,
                journal,
                FrontPanelKey::Mode,
                "anchor-row-back-to-submenu",
                anchor_submenu,
                &format!("{}-anchor-submenu", target.number),
                &target.number,
            )
            .await?;
            let submenu_paths = reviewed_submenu_paths(manifest, anchor_category)?;
            let target_submenu_screen = navigate_submenus(
                session,
                output_dir,
                journal,
                &submenu_paths,
                &anchor.category_path,
                &target.category_path,
                anchor_submenu_screen,
                &target.number,
            )
            .await?;
            if !selected_matches_label(&target_submenu_screen, target_submenu) {
                return Err(navigation_mismatch(
                    &target.number,
                    target_submenu,
                    &target_submenu_screen.selected,
                ));
            }

            let target_rows = reviewed_rows(manifest, &target.category_path);
            if target.number == "710" {
                record_menu_710_singleton_submenu_proof(
                    manifest,
                    target,
                    &target_submenu_screen,
                    journal,
                )?;
                return Ok(target_submenu_screen);
            }
            let first = target_rows.first().ok_or_else(|| {
                invalid_input(format!(
                    "menu {} target submenu {:?} has no reviewed rows",
                    target.number, target.category_path
                ))
            })?;
            let first_row = tap_capture_selected_exact(
                session,
                output_dir,
                journal,
                FrontPanelKey::Ab,
                "activate-exact-reviewed-submenu-OK",
                &first.label,
                &format!("{}-target-submenu-first-row", target.number),
                &target.number,
            )
            .await?;
            (first_row, first.number.as_str())
        };

        let target_rows = reviewed_rows(manifest, &target.category_path);
        navigate_rows(
            session,
            output_dir,
            journal,
            &target_rows,
            row_start_number,
            &target.number,
            current_row,
        )
        .await
    }

    fn record_menu_710_singleton_submenu_proof(
        manifest: &[MenuEntry],
        target: &MenuEntry,
        target_submenu_screen: &CapturedScreen,
        journal: &mut Journal,
    ) -> AuditResult<()> {
        if !menu_710_is_exact_reviewed_singleton(manifest, target)
            || !menu_710_singleton_memory_submenu_matches(target_submenu_screen)
        {
            return Err(io::Error::other(
                "menu 710 did not prove the exact stock-V1.03 FM Broadcasting / 71- / Memory singleton-submenu locator",
            )
            .into());
        }
        journal.append(json!({
            "type": "menu-710-singleton-submenu-proof",
            "menu_number": target.number,
            "expected_leaf_label": target.label,
            "category_title": "FM Broadcasting",
            "submenu_locator": "71-",
            "selected_submenu": "Memory",
            "reviewed_leaf_count": 1,
            "selection_band": { "top": 44, "height": 24 },
            "activation_keys_dispatched": false,
            "editor_entered": false,
            "policy": "the selected stock-V1.03 singleton submenu is the last non-entry locator; activating it can enter the FM-radio multi-record list and is forbidden",
            "result": "pass",
        }))?;
        Ok(())
    }

    async fn navigate_submenus(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        ordered: &[(&str, &str)],
        start_path: &str,
        target_path: &str,
        current: CapturedScreen,
        target_number: &str,
    ) -> AuditResult<CapturedScreen> {
        let start = ordered
            .iter()
            .position(|(path, _)| *path == start_path)
            .ok_or_else(|| invalid_input(format!("unknown anchor submenu {start_path:?}")))?;
        let target = ordered
            .iter()
            .position(|(path, _)| *path == target_path)
            .ok_or_else(|| invalid_input(format!("unknown target submenu {target_path:?}")))?;
        let (key, indices): (FrontPanelKey, Vec<usize>) = if start < target {
            (FrontPanelKey::Down, ((start + 1)..=target).collect())
        } else {
            (FrontPanelKey::Up, (target..start).rev().collect())
        };
        navigate_selected_labels(
            session,
            output_dir,
            journal,
            ordered,
            &indices,
            key,
            "select-reviewed-submenu",
            "submenu",
            target_number,
            current,
        )
        .await
    }

    async fn navigate_rows(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        ordered: &[&MenuEntry],
        start_number: &str,
        target_number: &str,
        mut current: CapturedScreen,
    ) -> AuditResult<CapturedScreen> {
        let start = ordered
            .iter()
            .position(|entry| entry.number == start_number)
            .ok_or_else(|| invalid_input(format!("unknown route row {start_number}")))?;
        let target = ordered
            .iter()
            .position(|entry| entry.number == target_number)
            .ok_or_else(|| invalid_input(format!("unknown target row {target_number}")))?;
        let (key, indices): (FrontPanelKey, Vec<usize>) = if start < target {
            (FrontPanelKey::Down, ((start + 1)..=target).collect())
        } else {
            (FrontPanelKey::Up, (target..start).rev().collect())
        };
        for (step, index) in indices.into_iter().enumerate() {
            let expected = ordered
                .get(index)
                .ok_or_else(|| invalid_input(format!("unknown reviewed row index {index}")))?;
            current = tap_capture_selected_exact(
                session,
                output_dir,
                journal,
                key,
                "select-reviewed-row",
                &expected.label,
                &format!("{target_number}-row-step-{}", step + 1),
                target_number,
            )
            .await?;
            if !screen_has_exact_menu_locator(&current, &expected.number) {
                return Err(io::Error::other(format!(
                    "menu {target_number} row navigation reached label {:?} without exact locator {}",
                    expected.label, expected.number
                ))
                .into());
            }
        }
        let target_entry = ordered
            .get(target)
            .ok_or_else(|| invalid_input(format!("unknown reviewed row index {target}")))?;
        if !numbered_row_matches(&current, &target_entry.number, &target_entry.label) {
            return Err(navigation_mismatch(
                target_number,
                &target_entry.label,
                &current.selected,
            ));
        }
        Ok(current)
    }

    async fn navigate_selected_labels(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        ordered: &[(&str, &str)],
        indices: &[usize],
        key: FrontPanelKey,
        purpose: &str,
        stem: &str,
        target_number: &str,
        mut current: CapturedScreen,
    ) -> AuditResult<CapturedScreen> {
        for (step, index) in indices.iter().copied().enumerate() {
            let expected = ordered
                .get(index)
                .ok_or_else(|| invalid_input(format!("unknown reviewed submenu index {index}")))?
                .1;
            current = tap_capture_selected_exact(
                session,
                output_dir,
                journal,
                key,
                purpose,
                expected,
                &format!("{target_number}-{stem}-step-{}", step + 1),
                target_number,
            )
            .await?;
        }
        Ok(current)
    }

    async fn tap_capture_selected_exact(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        key: FrontPanelKey,
        purpose: &str,
        expected: &str,
        suffix: &str,
        menu_number: &str,
    ) -> AuditResult<CapturedScreen> {
        let _receipt = tap(session, journal, key, purpose).await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let screen = capture_quiescent(session, output_dir, journal, suffix, menu_number).await?;
        if !selected_matches_label_for_menu(&screen, Some(menu_number), expected) {
            return Err(navigation_mismatch(menu_number, expected, &screen.selected));
        }
        Ok(screen)
    }

    async fn normalize_to_home(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
    ) -> AuditResult<CapturedScreen> {
        let mut single_band_baseline = None;
        normalize_to_home_tracking_single_band(
            session,
            output_dir,
            journal,
            &mut single_band_baseline,
        )
        .await
    }

    async fn normalize_to_home_tracking_single_band(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        single_band_baseline: &mut Option<CapturedScreen>,
    ) -> AuditResult<CapturedScreen> {
        let mut screen =
            capture_screen(session, output_dir, journal, "initial-state", None).await?;
        for attempt in 0..=5 {
            if is_operating_screen(&screen, None) {
                let baseline =
                    capture_home_quiescent(session, output_dir, journal, "qualified-baseline-home")
                        .await?;
                if !is_operating_screen(&baseline, None) {
                    return Err(io::Error::other(
                        "quiescent baseline no longer proves the operating screen",
                    )
                    .into());
                }
                journal.append(json!({
                    "type": "home-normalization",
                    "back_steps": attempt,
                    "baseline_crc32": format!("{:08X}", baseline.crc32),
                    "baseline_policy": "three-captures-with-identical-masked-pixels-and-ordered-frequency-mode-anchor-text-establish-home-candidate; volatile-row-and-live-RF-S-meter-bytes-may-differ; Vision bounds are evidence only",
                    "result": "pass",
                }))?;
                return Ok(baseline);
            }
            if is_reviewed_single_band_home(&screen) {
                if single_band_baseline.is_none() {
                    *single_band_baseline = Some(screen.clone());
                }
                journal.append(json!({
                    "type": "home-normalization-intent",
                    "source_profile": "reviewed-v1.03-single-band-analog-home",
                    "transition": ["F", "A/B"],
                    "documented_semantics": "toggle-single-dual-band-display",
                    "persistent_mcp_configuration_changed": false,
                }))?;
                tap_function_ab_toggle(
                    session,
                    journal,
                    "normalize-reviewed-single-band-home-to-dual-band",
                )
                .await?;
                tokio::time::sleep(SETTLE_DELAY).await;
                let dual_band = capture_screen(
                    session,
                    output_dir,
                    journal,
                    "normalize-single-to-dual",
                    None,
                )
                .await?;
                if !is_operating_screen(&dual_band, None) {
                    return Err(io::Error::other(
                        "F then A/B did not convert the reviewed single-band analog home screen to a dual-band operating screen",
                    )
                    .into());
                }
                screen = dual_band;
                continue;
            }
            let (key, purpose) = menu_exit_key(&screen).ok_or_else(|| {
                io::Error::other(
                    "initial screen is neither an operating screen nor a recognized menu context",
                )
            })?;
            let _back_receipt = tap(session, journal, key, purpose).await?;
            tokio::time::sleep(SETTLE_DELAY).await;
            screen = capture_screen(
                session,
                output_dir,
                journal,
                &format!("normalize-back-{}", attempt + 1),
                None,
            )
            .await?;
        }
        Err(io::Error::other("menu navigation did not restore the operating screen").into())
    }

    async fn capture_home_quiescent(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        stem: &str,
    ) -> AuditResult<CapturedScreen> {
        let first =
            capture_screen(session, output_dir, journal, &format!("{stem}-q1"), None).await?;
        tokio::time::sleep(QUIESCENCE_DELAY).await;
        let second =
            capture_screen(session, output_dir, journal, &format!("{stem}-q2"), None).await?;
        tokio::time::sleep(QUIESCENCE_DELAY).await;
        let third =
            capture_screen(session, output_dir, journal, &format!("{stem}-q3"), None).await?;
        let second_comparison = compare_dual_band_home(&second, &first)?;
        let third_comparison = compare_dual_band_home(&third, &first)?;
        journal_home_comparison(
            journal,
            "home-quiescence-second",
            None,
            &second,
            &first,
            &second_comparison,
        )?;
        journal_home_comparison(
            journal,
            "home-quiescence-third",
            None,
            &third,
            &first,
            &third_comparison,
        )?;
        let stable = second_comparison.restored() && third_comparison.restored();
        journal.append(json!({
            "type": "screen-quiescence",
            "scope": "reviewed-v1.03-dual-band-home",
            "stem": stem,
            "samples": 3,
            "comparison": "masked-stable-pixels-plus-ordered-frequency-mode-anchor-text",
            "mask_id": HOME_MASK_ID,
            "result": if stable { "pass" } else { "fail" },
        }))?;
        if stable {
            Ok(third)
        } else {
            Err(io::Error::other(format!(
                "home screen {stem:?} was not stable under the reviewed V1.03 dual-band mask and anchor oracle"
            ))
            .into())
        }
    }

    async fn restore_home(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        menu_number: &str,
        baseline: &CapturedScreen,
    ) -> AuditResult<()> {
        if menu_number == "102" {
            return restore_menu_102_runtime(
                session,
                output_dir,
                journal,
                baseline,
                "per-entry-restoration",
            )
            .await;
        }
        let mut key = FrontPanelKey::Mode;
        let mut purpose = "restore-menu-back";
        for step in 1..=6 {
            // Value pages have already backed out to an exact numbered row;
            // row-only cases are also on an exact numbered row except Menu
            // 710, which stops at its exact singleton-submenu locator. Every
            // restore step is therefore a reviewed menu-back operation.
            let _back_receipt = tap(session, journal, key, purpose).await?;
            tokio::time::sleep(SETTLE_DELAY).await;
            let screen = capture_screen(
                session,
                output_dir,
                journal,
                &format!("{menu_number}-restore-{step}"),
                Some(menu_number),
            )
            .await?;
            let comparison = compare_dual_band_home(&screen, baseline)?;
            if comparison.restored() {
                journal_home_comparison(
                    journal,
                    "per-entry-restoration",
                    Some(menu_number),
                    &screen,
                    baseline,
                    &comparison,
                )?;
                journal.append(json!({
                    "type": "restore-assertion",
                    "menu_number": menu_number,
                    "back_steps": step,
                    "comparison": "reviewed-v1.03-dual-band-masked-frame-and-ordered-text-anchors",
                    "mask_id": HOME_MASK_ID,
                    "full_frame_equal": screen.frame == baseline.frame,
                    "result": "pass",
                }))?;
                return Ok(());
            }
            if is_operating_screen(&screen, None) {
                journal_home_comparison(
                    journal,
                    "per-entry-restoration-failure",
                    Some(menu_number),
                    &screen,
                    baseline,
                    &comparison,
                )?;
                return Err(io::Error::other(format!(
                    "menu {menu_number} returned to an operating screen that failed the reviewed V1.03 dual-band home oracle"
                ))
                .into());
            }
            (key, purpose) = menu_exit_key(&screen).ok_or_else(|| {
                io::Error::other(format!(
                    "menu {menu_number} restore reached an unrecognized non-home screen"
                ))
            })?;
        }
        Err(io::Error::other(format!(
            "menu {menu_number} did not restore the reviewed V1.03 dual-band home profile within six safe exit steps"
        ))
        .into())
    }

    async fn restore_menu_102_runtime(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        phase: &str,
    ) -> AuditResult<()> {
        let baseline_operation_band = observed_operation_band(baseline).ok_or_else(|| {
            io::Error::other(
                "Menu 102 restoration requires one unambiguous baseline PTT/operation-band marker",
            )
        })?;
        for step in 0..=6 {
            let screen = capture_screen(
                session,
                output_dir,
                journal,
                &format!("102-runtime-restore-{step}"),
                Some("102"),
            )
            .await?;
            let comparison = compare_dual_band_home(&screen, baseline)?;
            if comparison.restored() {
                journal_home_comparison(
                    journal,
                    phase,
                    Some("102"),
                    &screen,
                    baseline,
                    &comparison,
                )?;
                journal.append(json!({
                    "type": "restore-assertion",
                    "menu_number": "102",
                    "back_steps": step,
                    "runtime_prerequisite_restored": true,
                    "operation_band": format!("{baseline_operation_band:?}"),
                    "dual_band": true,
                    "persistent_mcp_configuration_changed": false,
                    "comparison": "reviewed-v1.03-dual-band-masked-frame-and-ordered-text-anchors",
                    "mask_id": HOME_MASK_ID,
                    "full_frame_equal": screen.frame == baseline.frame,
                    "result": "pass",
                }))?;
                return Ok(());
            }

            if is_reviewed_single_band_b_home(&screen, baseline) {
                restore_menu_102_dual_band(session, output_dir, journal, baseline).await?;
                return finish_menu_102_runtime_restore(
                    session,
                    output_dir,
                    journal,
                    baseline,
                    baseline_operation_band,
                    phase,
                    step,
                )
                .await;
            }

            if comparison.semantic_profile_valid
                && observed_operation_band(&screen) == Some(Band::B)
            {
                return finish_menu_102_runtime_restore(
                    session,
                    output_dir,
                    journal,
                    baseline,
                    baseline_operation_band,
                    phase,
                    step,
                )
                .await;
            }

            let (key, purpose) = menu_exit_key(&screen).ok_or_else(|| {
                io::Error::other(
                    "Menu 102 cleanup reached neither a reviewed menu context nor dual/single-band home",
                )
            })?;
            let _receipt = tap(session, journal, key, purpose).await?;
            tokio::time::sleep(SETTLE_DELAY).await;
        }
        Err(io::Error::other(
            "Menu 102 did not restore its transient single-band context within six safe exit steps",
        )
        .into())
    }

    async fn restore_menu_102_dual_band(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
    ) -> AuditResult<()> {
        tap_function_ab_toggle(session, journal, "menu-102-restore-dual-band").await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let dual_band_b = capture_screen(
            session,
            output_dir,
            journal,
            "102-runtime-restored-dual-band-b",
            Some("102"),
        )
        .await?;
        let comparison = compare_dual_band_home(&dual_band_b, baseline)?;
        let visible_band = observed_operation_band(&dual_band_b);
        let passed = comparison.semantic_profile_valid && visible_band == Some(Band::B);
        journal.append(json!({
            "type": "menu-102-runtime-restoration-screen",
            "phase": "dual-band-operation-band-b",
            "operation_band": visible_band.map(|band| format!("{band:?}")),
            "expected_operation_band": format!("{:?}", Band::B),
            "ordered_frequency_anchors_match": comparison.semantic_profile_valid,
            "result": if passed { "pass" } else { "fail" },
        }))?;
        if passed {
            Ok(())
        } else {
            Err(io::Error::other(
                "Menu 102 cleanup did not restore the reviewed dual-band Band B home context",
            )
            .into())
        }
    }

    async fn finish_menu_102_runtime_restore(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        baseline_operation_band: Band,
        phase: &str,
        back_steps: usize,
    ) -> AuditResult<()> {
        if baseline_operation_band == Band::A {
            let _receipt = tap(
                session,
                journal,
                FrontPanelKey::Ab,
                "menu-102-restore-baseline-operation-band",
            )
            .await?;
            tokio::time::sleep(SETTLE_DELAY).await;
        }
        let restored = capture_home_quiescent(
            session,
            output_dir,
            journal,
            "102-runtime-restored-baseline-home",
        )
        .await?;
        let comparison = compare_dual_band_home(&restored, baseline)?;
        let operation_band = observed_operation_band(&restored);
        journal_home_comparison(
            journal,
            phase,
            Some("102"),
            &restored,
            baseline,
            &comparison,
        )?;
        journal.append(json!({
            "type": "restore-assertion",
            "menu_number": "102",
            "back_steps": back_steps,
            "runtime_prerequisite_restored": true,
            "operation_band": operation_band.map(|band| format!("{band:?}")),
            "expected_operation_band": format!("{baseline_operation_band:?}"),
            "dual_band": true,
            "persistent_mcp_configuration_changed": false,
            "comparison": "reviewed-v1.03-dual-band-masked-frame-and-ordered-text-anchors",
            "mask_id": HOME_MASK_ID,
            "full_frame_equal": restored.frame == baseline.frame,
            "result": if comparison.restored() && operation_band == Some(baseline_operation_band) { "pass" } else { "fail" },
        }))?;
        if comparison.restored() && operation_band == Some(baseline_operation_band) {
            Ok(())
        } else {
            Err(io::Error::other(
                "Menu 102 cleanup did not restore the exact qualified dual-band home baseline and operation band",
            )
            .into())
        }
    }

    async fn reconnect_and_normalize_home(
        radio: &mut Radio<EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        phase: &str,
    ) -> AuditResult<CapturedScreen> {
        journal.set_active_menu(None);
        journal.append(json!({
            "type": "automation-reconnect-recovery-intent",
            "phase": phase,
            "reason": "prior-qualified-session-invalid-after-failed-operation",
            "failed_operation_replayed": false,
            "transport_identity": "same-process-isolated-bluetooth-device",
        }))?;
        let reconnect_started = Instant::now();
        radio.reconnect().await?;
        journal.append(json!({
            "type": "automation-reconnect-recovery-receipt",
            "phase": phase,
            "elapsed_ms": millis(reconnect_started.elapsed()),
            "exact_model_identity_revalidated": true,
            "bluetooth_device_selector_reused": true,
            "failed_operation_replayed": false,
        }))?;

        let qualification_started = Instant::now();
        let mut session = radio.qualify_automation().await?;
        let abi = session.abi();
        journal.append(json!({
            "type": "qualification",
            "phase": phase,
            "first_cat_or_mcp_operation": false,
            "elapsed_ms": millis(qualification_started.elapsed()),
            "abi": {
                "version": abi.version,
                "features": abi.features,
                "max_key": abi.max_key,
                "max_phase": abi.max_phase,
            },
        }))?;
        normalize_to_home(&mut session, output_dir, journal).await
    }

    async fn reconnect_and_restore_initial_home_profile(
        radio: &mut Radio<EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        dual_band_baseline: Option<&CapturedScreen>,
        single_band_baseline: Option<&CapturedScreen>,
        phase: &str,
    ) -> AuditResult<()> {
        let recovered = reconnect_and_normalize_home(radio, output_dir, journal, phase).await?;
        let dual_band_recovery = if let Some(baseline) = dual_band_baseline {
            let comparison = compare_dual_band_home(&recovered, baseline)?;
            journal_home_comparison(journal, phase, None, &recovered, baseline, &comparison)?;
            if comparison.restored() {
                Ok(())
            } else {
                Err(io::Error::other(
                    "initial-session reconnect did not restore the qualified dual-band home baseline",
                )
                .into())
            }
        } else {
            Ok(())
        };
        let display_mode_recovery = if let Some(baseline) = single_band_baseline {
            let qualification_started = Instant::now();
            let mut session = radio.qualify_automation().await?;
            journal.append(json!({
                "type": "qualification",
                "phase": format!("{phase}-startup-display-mode-restoration"),
                "first_cat_or_mcp_operation": false,
                "elapsed_ms": millis(qualification_started.elapsed()),
                "abi": {
                    "version": session.abi().version,
                    "features": session.abi().features,
                    "max_key": session.abi().max_key,
                    "max_phase": session.abi().max_phase,
                },
            }))?;
            restore_startup_single_band_profile(&mut session, output_dir, journal, baseline, phase)
                .await
        } else {
            Ok(())
        };
        combine_primary_and_cleanup_errors(
            Ok(()),
            [
                ("dual-band-home-recovery", dual_band_recovery),
                ("startup-display-mode-restoration", display_mode_recovery),
            ],
        )
    }

    async fn reconnect_and_recover_home(
        radio: &mut Radio<EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        phase: &str,
    ) -> AuditResult<()> {
        let target_band = observed_operation_band(baseline).ok_or_else(|| {
            io::Error::other(
                "reconnect recovery baseline omitted an unambiguous operation-band marker",
            )
        })?;
        journal.set_active_menu(None);
        journal.append(json!({
            "type": "automation-reconnect-recovery-intent",
            "phase": phase,
            "reason": "prior-qualified-session-invalid-after-failed-operation",
            "failed_operation_replayed": false,
            "transport_identity": "same-process-isolated-bluetooth-device",
            "recovery_only_runtime_normalization": {
                "dual_band": true,
                "operation_band": format!("{target_band:?}"),
            },
        }))?;
        let reconnect_started = Instant::now();
        radio.reconnect().await?;
        journal.append(json!({
            "type": "automation-reconnect-recovery-receipt",
            "phase": phase,
            "elapsed_ms": millis(reconnect_started.elapsed()),
            "exact_model_identity_revalidated": true,
            "bluetooth_device_selector_reused": true,
            "failed_operation_replayed": false,
        }))?;

        let observed_band_mode = radio.get_band_mode().await?;
        if observed_band_mode != BandMode::Dual {
            radio.set_band_mode(BandMode::Dual).await?;
        }
        let observed_band = radio.get_band().await?;
        if observed_band != target_band {
            radio.set_band(target_band).await?;
        }
        let verified_band_mode = radio.get_band_mode().await?;
        let verified_band = radio.get_band().await?;
        let runtime_restored = verified_band_mode == BandMode::Dual && verified_band == target_band;
        journal.append(json!({
            "type": "automation-reconnect-runtime-restoration",
            "phase": phase,
            "before": {
                "band_mode": format!("{observed_band_mode:?}"),
                "operation_band": format!("{observed_band:?}"),
            },
            "after": {
                "band_mode": format!("{verified_band_mode:?}"),
                "operation_band": format!("{verified_band:?}"),
            },
            "target": {
                "band_mode": "Dual",
                "operation_band": format!("{target_band:?}"),
            },
            "persistent_mcp_configuration_changed": false,
            "result": if runtime_restored { "pass" } else { "fail" },
        }))?;
        if !runtime_restored {
            return Err(io::Error::other(
                "reconnect recovery failed to restore the qualified dual-band operation-band runtime state",
            )
            .into());
        }

        let qualification_started = Instant::now();
        let mut session = radio.qualify_automation().await?;
        journal.append(json!({
            "type": "qualification",
            "phase": phase,
            "first_cat_or_mcp_operation": false,
            "elapsed_ms": millis(qualification_started.elapsed()),
            "abi": {
                "version": session.abi().version,
                "features": session.abi().features,
                "max_key": session.abi().max_key,
                "max_phase": session.abi().max_phase,
            },
        }))?;
        let recovered = normalize_to_home(&mut session, output_dir, journal).await?;
        let comparison = compare_dual_band_home(&recovered, baseline)?;
        journal_home_comparison(journal, phase, None, &recovered, baseline, &comparison)?;
        if comparison.restored() {
            Ok(())
        } else {
            Err(io::Error::other(
                "reconnected UI recovery reached an operating screen that failed the reviewed V1.03 dual-band home oracle",
            )
            .into())
        }
    }

    async fn best_effort_home_recovery(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        phase: &str,
    ) -> AuditResult<()> {
        journal.set_active_menu(None);
        let recovered = normalize_to_home(session, output_dir, journal).await?;
        let comparison = compare_dual_band_home(&recovered, baseline)?;
        journal_home_comparison(journal, phase, None, &recovered, baseline, &comparison)?;
        if comparison.restored() {
            Ok(())
        } else {
            Err(io::Error::other(
                "best-effort UI recovery reached an operating screen that failed the reviewed V1.03 dual-band home oracle",
            )
            .into())
        }
    }

    fn combine_primary_and_cleanup_errors<const N: usize>(
        primary: AuditResult<()>,
        cleanup: [(&str, AuditResult<()>); N],
    ) -> AuditResult<()> {
        let primary_error = primary.err().map(|error| error.to_string());
        let cleanup_errors = cleanup
            .into_iter()
            .filter_map(|(phase, result)| result.err().map(|error| format!("{phase}: {error}")))
            .collect::<Vec<_>>();
        match (primary_error, cleanup_errors.is_empty()) {
            (None, true) => Ok(()),
            (Some(primary_error), true) => Err(io::Error::other(primary_error).into()),
            (None, false) => Err(io::Error::other(format!(
                "audit cleanup failed: {}",
                cleanup_errors.join("; ")
            ))
            .into()),
            (Some(primary_error), false) => Err(io::Error::other(format!(
                "primary audit failure: {primary_error}; cleanup failures: {}",
                cleanup_errors.join("; ")
            ))
            .into()),
        }
    }

    async fn tap(
        session: &mut AutomationSession<'_, EitherTransport>,
        journal: &mut Journal,
        key: FrontPanelKey,
        purpose: &str,
    ) -> AuditResult<AutomationMetadata> {
        if is_operationally_overloaded_numeric_key(key) {
            return Err(io::Error::other(format!(
                "refusing operationally overloaded numeric key {key:?} through the ordinary tap path; dispatch a complete route through one consumed V1.03.AZM guarded snapshot"
            ))
            .into());
        }
        journal.append(json!({
            "type": "key-intent",
            "purpose": purpose,
            "key": format!("{key:?}"),
            "raw_key": key.as_raw(),
        }))?;
        let started = Instant::now();
        let metadata = session.tap_key(key).await?;
        journal.append(json!({
            "type": "key-receipt",
            "purpose": purpose,
            "key": format!("{key:?}"),
            "raw_key": key.as_raw(),
            "elapsed_ms": millis(started.elapsed()),
            "metadata": metadata_json(&metadata),
        }))?;
        Ok(metadata)
    }

    async fn tap_function_ab_toggle(
        session: &mut AutomationSession<'_, EitherTransport>,
        journal: &mut Journal,
        purpose: &str,
    ) -> AuditResult<()> {
        journal.append(json!({
            "type": "key-pair-intent",
            "purpose": purpose,
            "keys": [format!("{:?}", FrontPanelKey::Function), format!("{:?}", FrontPanelKey::Ab)],
            "raw_keys": [FrontPanelKey::Function.as_raw(), FrontPanelKey::Ab.as_raw()],
            "documented_semantics": "toggle-dual-single-band-display",
            "capture_sleep_or_filesystem_io_between_taps": false,
        }))?;
        let started = Instant::now();
        let function_metadata = session.tap_key(FrontPanelKey::Function).await?;
        let ab_metadata = session.tap_key(FrontPanelKey::Ab).await?;
        journal.append(json!({
            "type": "key-pair-receipt",
            "purpose": purpose,
            "keys": [format!("{:?}", FrontPanelKey::Function), format!("{:?}", FrontPanelKey::Ab)],
            "elapsed_ms": millis(started.elapsed()),
            "function_metadata": metadata_json(&function_metadata),
            "ab_metadata": metadata_json(&ab_metadata),
            "complete_press_release_authenticated": true,
        }))?;
        Ok(())
    }

    async fn restore_startup_single_band_profile(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        baseline: &CapturedScreen,
        phase: &str,
    ) -> AuditResult<()> {
        journal.append(json!({
            "type": "startup-display-mode-restoration-intent",
            "phase": phase,
            "startup_display_mode": "single-band",
            "transition": ["F", "A/B"],
            "persistent_mcp_configuration_changed": false,
        }))?;
        tap_function_ab_toggle(session, journal, "restore-startup-single-band-display-mode")
            .await?;
        tokio::time::sleep(SETTLE_DELAY).await;
        let restored = capture_screen(
            session,
            output_dir,
            journal,
            &format!("{phase}-single-band-restored"),
            None,
        )
        .await?;
        let profile_matches = reviewed_single_band_home_matches(&restored, baseline);
        journal.append(json!({
            "type": "startup-display-mode-restoration",
            "phase": phase,
            "startup_display_mode": "single-band",
            "baseline_crc32": format!("{:08X}", baseline.crc32),
            "restored_crc32": format!("{:08X}", restored.crc32),
            "frequency_and_operation_band_match": profile_matches,
            "persistent_mcp_configuration_changed": false,
            "result": if profile_matches { "pass" } else { "fail" },
        }))?;
        if profile_matches {
            Ok(())
        } else {
            Err(io::Error::other(
                "F then A/B did not restore the reviewed startup single-band home profile",
            )
            .into())
        }
    }

    /// Capture and validate the one raw V1.03.AZM snapshot consumed by a complete
    /// numeric route, without performing OCR, file output, or journal I/O.
    async fn capture_numeric_route_snapshot(
        session: &mut AutomationSession<'_, EitherTransport>,
        qualified_menu: &CapturedScreen,
    ) -> AuditResult<(AutomationSnapshot, u128, Duration)> {
        let capture_started_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let capture_started = Instant::now();
        let snapshot = session.capture_screen().await?;
        let capture_round_trip = capture_started.elapsed();
        if snapshot.frame != qualified_menu.frame || snapshot.metadata.crc32 != qualified_menu.crc32
        {
            return Err(io::Error::other(
                "fresh V1.03.AZM numeric-route snapshot does not byte-match the qualified top-level Menu frame; refusing dispatch",
            )
            .into());
        }
        Ok((snapshot, capture_started_unix_ms, capture_round_trip))
    }

    /// Persist numeric proof material only after the guarded transaction
    /// returns, keeping OCR and filesystem latency out of the prefix path.
    #[expect(
        clippy::too_many_lines,
        reason = "one guarded route produces a deliberately verbose, ordered hardware evidence bundle covering its gate, aggregate receipt, and six possible input events"
    )]
    fn persist_numeric_dispatch_evidence(
        output_dir: &Path,
        journal: &mut Journal,
        evidence_menu_number: &str,
        dispatched_number: &str,
        purpose: &str,
        evidence: &NumericRouteEvidence,
    ) -> AuditResult<()> {
        let observations = evidence.snapshot.frame.recognize_text()?;
        let bands = v103_selection_bands(&evidence.snapshot.frame);
        let selected = selected_text(&observations, &bands)
            .into_iter()
            .map(|observation| observation.text().to_owned())
            .collect::<Vec<_>>();
        let (journal_selected, journal_observations, redact_private_value) =
            journal_screen_text(Some(evidence_menu_number), &selected, &observations)?;
        let suffix =
            format!("{evidence_menu_number}-number-{dispatched_number}-guarded-route-gate");
        let file_name = journal.next_capture_name(&suffix);
        fs::write(
            output_dir.join(&file_name),
            evidence.snapshot.frame.to_stock_bmp(),
        )?;
        journal.append(json!({
            "type": "screen-capture",
            "menu_number": evidence_menu_number,
            "suffix": suffix,
            "bmp": file_name,
            "capture_started_unix_ms": evidence.capture_started_unix_ms,
            "capture_elapsed_ms": millis(evidence.capture_round_trip),
            "ocr_timing": "deferred-until-after-guarded-route-outcome",
            "metadata": metadata_json(&evidence.snapshot.metadata),
            "selection_bands": bands.iter().map(|band| json!({
                "top": band.top(),
                "bottom_exclusive": band.bottom_exclusive(),
                "height": band.height(),
            })).collect::<Vec<_>>(),
            "selected_text": journal_selected,
            "observations": journal_observations,
            "private_value_redaction": redact_private_value.then_some("all-body-and-selected-observations-sha256-only"),
            "captured_before-route-and-persisted-after-guarded-outcome": true,
        }))?;

        let (outcome, result) = match &evidence.outcome {
            GuardedDecimalRouteOutcome::Dispatched(_) => ("dispatched", "pass"),
            GuardedDecimalRouteOutcome::ContextChanged(_) => {
                ("context-changed", "authenticated-zero-prefix-refusal")
            }
            _ => ("unsupported", "fail-closed"),
        };
        let receipt = evidence.outcome.receipt();
        journal.append(json!({
            "type": "numeric-route-context-gate",
            "menu_number": evidence_menu_number,
            "dispatched_number": dispatched_number,
            "requested_keys": evidence.requested_keys.iter().map(|key| format!("{key:?}")).collect::<Vec<_>>(),
            "crc32": format!("{:08X}", evidence.snapshot.metadata.crc32),
            "generation": evidence.snapshot.metadata.generation,
            "initial_command_count": evidence.snapshot.metadata.command_count,
            "initial_seqlock": evidence.snapshot.metadata.seqlock,
            "final_metadata": metadata_json(evidence.outcome.metadata()),
            "capture_started_unix_ms": evidence.capture_started_unix_ms,
            "capture_round_trip_ms": millis(evidence.capture_round_trip),
            "guarded_call_elapsed_ms": millis(evidence.dispatch_elapsed),
            "maximum_host_lease_age_after_validated_capture_ms": millis(GUARDED_SNAPSHOT_MAX_AGE),
            "maximum_route_command_reply_duration_ms": millis(GUARDED_ROUTE_MAX_DURATION),
            "context": "byte-identical-to-qualified-top-level-Menu-frame",
            "direct_prefix_parser_state": "firmware-atomic-complete-route-not-host-visible",
            "lease_consumption": "one-snapshot-one-guarded-transaction",
            "wire_command": format!("GM R{},{:02X}", evidence.route, receipt.sequence),
            "wire_reply_status": if matches!(&evidence.outcome, GuardedDecimalRouteOutcome::Dispatched(_)) { "00" } else { "02" },
            "wire_command_count_delta": 1,
            "wire_seqlock_delta": 2,
            "between_digit_host_work": "none-single-firmware-command",
            "guard_invariant": "one-firmware-observed-full-frame-match-before-all-three-synchronous-route-taps",
            "host_ocr_io_to_key_race_removed": true,
            "residual_concurrent_framebuffer_writer_toctou": true,
            "context_changed_policy": "exact-zero-prefix-receipt-before-any-route-input-no-retry",
            "session_requires_recovery": evidence.outcome.requires_recovery(),
            "outcome": outcome,
            "route_sequence": receipt.sequence,
            "route_guard_count": receipt.guard_count,
            "route_completed_taps": receipt.completed_taps,
            "route_event_mask": format!("{:02X}", receipt.event_mask),
            "result": result,
        }))?;

        for (digit_index, key) in evidence.requested_keys.iter().copied().enumerate() {
            journal.append(json!({
                "type": "key-intent",
                "purpose": purpose,
                "digit_index": digit_index,
                "digit_ordinal": digit_index + 1,
                "key": format!("{key:?}"),
                "raw_key": key.as_raw(),
                "submitted_in_atomic_firmware_command": true,
                "dispatch_authorized_by_start_guard": matches!(&evidence.outcome, GuardedDecimalRouteOutcome::Dispatched(_)),
                "complete_press_release_authenticated": digit_index < usize::from(receipt.completed_taps),
                "press_event_bit": digit_index * 2,
                "release_event_bit": digit_index * 2 + 1,
                "recorded_after-guarded-transaction-to-protect-prefix-timing": true,
            }))?;
        }
        journal.append(json!({
            "type": "guarded-decimal-route-receipt",
            "purpose": purpose,
            "route": evidence.route.to_string(),
            "sequence": receipt.sequence,
            "guard_count": receipt.guard_count,
            "completed_taps": receipt.completed_taps,
            "event_mask": format!("{:02X}", receipt.event_mask),
            "command_count": receipt.metadata.command_count,
            "seqlock": receipt.metadata.seqlock,
            "bound_snapshot_generation": evidence.snapshot.metadata.generation,
            "bound_snapshot_crc32": format!("{:08X}", evidence.snapshot.metadata.crc32),
            "authenticated_by_final_metadata": true,
            "recorded_after_guarded_transaction": true,
        }))?;
        Ok(())
    }

    const fn is_operationally_overloaded_numeric_key(key: FrontPanelKey) -> bool {
        matches!(
            key,
            FrontPanelKey::Mark0
                | FrontPanelKey::Vfo1
                | FrontPanelKey::Mr2
                | FrontPanelKey::Call3
                | FrontPanelKey::Msg4
                | FrontPanelKey::List5
                | FrontPanelKey::Beacon6
                | FrontPanelKey::Reverse7
                | FrontPanelKey::Tone8
                | FrontPanelKey::Pf1_9
        )
    }

    async fn capture_quiescent(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        stem: &str,
        menu_number: &str,
    ) -> AuditResult<CapturedScreen> {
        let screen = capture_screen(session, output_dir, journal, stem, Some(menu_number)).await?;
        journal.append(json!({
            "type": "settled-stable-screen-assertion",
            "menu_number": menu_number,
            "stem": stem,
            "host_samples": 1,
            "post_key_settle_ms": millis(SETTLE_DELAY),
            "firmware_snapshot_contract": "one firmware-published stable frame; metadata bracketing the pixel transfer is byte-identical and host CRC matches",
            "crc32": format!("{:08X}", screen.crc32),
            "result": "pass",
        }))?;
        Ok(screen)
    }

    async fn capture_screen(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        suffix: &str,
        menu_number: Option<&str>,
    ) -> AuditResult<CapturedScreen> {
        let started = Instant::now();
        let snapshot = session.capture_screen().await?;
        let capture_elapsed = started.elapsed();
        let ocr_started = Instant::now();
        let observations = snapshot.frame.recognize_text()?;
        let ocr_elapsed = ocr_started.elapsed();
        let bands = v103_selection_bands(&snapshot.frame);
        let selected = selected_text(&observations, &bands)
            .into_iter()
            .map(|observation| observation.text().to_owned())
            .collect::<Vec<_>>();
        let (journal_selected, journal_observations, redact_private_value) =
            journal_screen_text(menu_number, &selected, &observations)?;
        let file_name = journal.next_capture_name(suffix);
        let path = output_dir.join(&file_name);
        fs::write(&path, snapshot.frame.to_stock_bmp())?;
        let crc32 = snapshot.metadata.crc32;
        journal.append(json!({
            "type": "screen-capture",
            "menu_number": menu_number,
            "suffix": suffix,
            "bmp": file_name,
            "capture_elapsed_ms": millis(capture_elapsed),
            "ocr_elapsed_ms": millis(ocr_elapsed),
            "metadata": metadata_json(&snapshot.metadata),
            "selection_bands": bands.iter().map(|band| json!({
                "top": band.top(),
                "bottom_exclusive": band.bottom_exclusive(),
                "height": band.height(),
            })).collect::<Vec<_>>(),
            "selected_text": journal_selected,
            "observations": journal_observations,
            "private_value_redaction": redact_private_value.then_some("all-body-and-selected-observations-sha256-only"),
        }))?;
        Ok(CapturedScreen {
            frame: snapshot.frame,
            observations,
            selected,
            crc32,
        })
    }

    fn metadata_json(metadata: &AutomationMetadata) -> Value {
        json!({
            "seqlock": metadata.seqlock,
            "generation": metadata.generation,
            "capture_result": metadata.capture_result,
            "crc32": format!("{:08X}", metadata.crc32),
            "capture_attempts": metadata.capture_attempts,
            "command_count": metadata.command_count,
            "last_command": metadata.last_command,
            "last_host_sequence": metadata.last_host_sequence,
            "last_key": metadata.last_key,
            "last_phase": metadata.last_phase,
            "last_key_result": metadata.last_key_result,
            "rle_encoded_length": metadata.rle_encoded_length,
            "route_ascii": format!("{:06X}", metadata.route_ascii),
            "route_guard_count": metadata.route_guard_count,
            "route_completed_taps": metadata.route_completed_taps,
            "route_event_mask": format!("{:02X}", metadata.route_event_mask),
        })
    }

    fn observation_json(observation: &TextObservation) -> Value {
        let bounds = observation.bounds();
        json!({
            "text": observation.text(),
            "confidence": observation.confidence(),
            "bounds": {
                "x": bounds.x(),
                "y": bounds.y(),
                "width": bounds.width(),
                "height": bounds.height(),
            },
        })
    }

    fn journal_screen_text(
        menu_number: Option<&str>,
        selected: &[String],
        observations: &[TextObservation],
    ) -> AuditResult<(Value, Vec<Value>, bool)> {
        // The caller already knows which reviewed menu entry owns this
        // capture. Redaction must therefore follow that authenticated audit
        // context, not an OCR title heuristic: duplicate, missing, or damaged
        // title recognition must never make a private capture fall back to
        // plaintext JSONL. This may conservatively redact harmless gate/row
        // captures made while auditing the same entry; the internal OCR and
        // framebuffer evidence remain unchanged for validation.
        let redact_private_value = matches!(menu_number, Some("516" | "651" | "935" | "946"));
        Ok((
            journal_selected_text(selected, redact_private_value)?,
            journal_observations(observations, redact_private_value)?,
            redact_private_value,
        ))
    }

    fn journal_selected_text(
        selected: &[String],
        redact_private_value: bool,
    ) -> AuditResult<Value> {
        if !redact_private_value {
            return Ok(json!(selected));
        }
        let values = selected
            .iter()
            .map(|text| private_text_json(text))
            .collect::<AuditResult<Vec<_>>>()?;
        Ok(json!(values))
    }

    fn journal_observations(
        observations: &[TextObservation],
        redact_private_value: bool,
    ) -> AuditResult<Vec<Value>> {
        observations
            .iter()
            .map(|observation| {
                let center = observation.bounds().y() + observation.bounds().height() / 2.0;
                let text = observation.text().trim();
                if redact_private_value && (0.15..0.85).contains(&center) {
                    let bounds = observation.bounds();
                    Ok(json!({
                        "text_sha256": sha256_hex(text.as_bytes())?,
                        "text_redacted": true,
                        "confidence": observation.confidence(),
                        "bounds": {
                            "x": bounds.x(),
                            "y": bounds.y(),
                            "width": bounds.width(),
                            "height": bounds.height(),
                        },
                    }))
                } else {
                    Ok(observation_json(observation))
                }
            })
            .collect()
    }

    fn private_text_json(text: &str) -> AuditResult<Value> {
        let trimmed = text.trim();
        Ok(json!({
            "text_sha256": sha256_hex(trimmed.as_bytes())?,
            "text_redacted": true,
        }))
    }

    fn parse_args() -> AuditResult<Config> {
        parse_args_from(std::env::args().skip(1))
    }

    fn parse_args_from(args: impl IntoIterator<Item = String>) -> AuditResult<Config> {
        let mut device_name = None;
        let mut port = None;
        let mut output_dir = None;
        let mut only_menu = None;
        let mut start_menu = None;
        let mut limit = None;
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| invalid_input(format!("{argument} requires a value")))?;
            if value.is_empty() || value.starts_with("--") {
                return Err(invalid_input(format!("{argument} requires a value")));
            }
            match argument.as_str() {
                "--device" if device_name.is_none() => device_name = Some(value),
                "--port" if port.is_none() => port = Some(value),
                "--output-dir" if output_dir.is_none() => output_dir = Some(PathBuf::from(value)),
                "--menu" if only_menu.is_none() => only_menu = Some(value),
                "--start" if start_menu.is_none() => start_menu = Some(value),
                "--limit" if limit.is_none() => limit = Some(value.parse()?),
                "--device" | "--port" | "--output-dir" | "--menu" | "--start" | "--limit" => {
                    return Err(invalid_input(format!("duplicate argument {argument}")));
                }
                _ => return Err(invalid_input(format!("unknown argument {argument}"))),
            }
        }
        let endpoint = match (device_name, port) {
            (Some(_), Some(_)) => {
                return Err(invalid_input("--port and --device are mutually exclusive"));
            }
            (Some(device_name), None) => Endpoint::Bluetooth(device_name),
            (None, Some(port)) => {
                if !Path::new(&port).is_absolute() {
                    return Err(invalid_input("--port must be an absolute path"));
                }
                if SerialTransport::is_bluetooth_port(&port) {
                    return Err(invalid_input(
                        "--port requires a USB CDC path; use --device for native Bluetooth",
                    ));
                }
                Endpoint::Usb(port)
            }
            (None, None) => Endpoint::Bluetooth("TH-D75".to_owned()),
        };
        if only_menu.is_some() && (start_menu.is_some() || limit.is_some()) {
            return Err(invalid_input(
                "--menu cannot be combined with --start or --limit",
            ));
        }
        if limit == Some(0) {
            return Err(invalid_input("--limit must be greater than zero"));
        }
        Ok(Config {
            endpoint,
            output_dir: output_dir.ok_or_else(|| invalid_input("--output-dir is required"))?,
            only_menu,
            start_menu,
            limit,
        })
    }

    fn open_transport(endpoint: &Endpoint) -> AuditResult<EitherTransport> {
        match endpoint {
            Endpoint::Bluetooth(device_name) => Ok(EitherTransport::Bluetooth(
                BluetoothTransport::open(Some(device_name))?,
            )),
            Endpoint::Usb(port) => Ok(EitherTransport::Serial(SerialTransport::open(port)?)),
        }
    }

    async fn apply_pre_mcp_transport_policy<T: Transport>(
        radio: &mut Radio<T>,
        policy: PreMcpTransportPolicy,
    ) -> Result<(), kenwood_thd75::Error> {
        match policy {
            PreMcpTransportPolicy::ReuseQualifiedLink => Ok(()),
            PreMcpTransportPolicy::ReopenUsbCdcAndIdentify => radio.reconnect().await,
        }
    }

    async fn prepare_transport_for_mcp<T: Transport>(
        radio: &mut Radio<T>,
        policy: PreMcpTransportPolicy,
        journal: &mut Journal,
        phase: &str,
    ) -> AuditResult<()> {
        let started = Instant::now();
        apply_pre_mcp_transport_policy(radio, policy).await?;
        journal.append(json!({
            "type": "pre-mcp-transport-boundary",
            "phase": phase,
            "action": policy.action(),
            "usb_cdc_reopen_reason": match policy {
                PreMcpTransportPolicy::ReuseQualifiedLink => None,
                PreMcpTransportPolicy::ReopenUsbCdcAndIdentify => Some(
                    "discard-the-long-lived-automation-CDC-session-before-changing-line-coding-and-entering-MCP"
                ),
            },
            "sleep_or_blind_retry_used": false,
            "elapsed_ms": millis(started.elapsed()),
            "result": "pass",
        }))
    }

    fn prepare_output_dir(path: &Path) -> AuditResult<()> {
        if !path.is_absolute() {
            return Err(invalid_input("--output-dir must be absolute"));
        }
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = builder.mode(0o700);
        builder.create(path)?;
        Ok(())
    }

    fn parse_menu_manifest(manual: &str) -> AuditResult<Vec<MenuEntry>> {
        let (_, section) = manual
            .split_once("## MENU CONFIGURATION")
            .ok_or_else(|| invalid_input("manual has no MENU CONFIGURATION section"))?;
        let section = section
            .split_once("\n---")
            .map_or(section, |(body, _)| body);
        let mut category_path = String::new();
        let mut entries = Vec::new();
        for line in section.lines().filter(|line| line.starts_with('|')) {
            let columns = line.split('|').skip(1).map(str::trim).collect::<Vec<_>>();
            let Some(first) = columns.first().copied() else {
                continue;
            };
            if let Some(heading) = strip_bold(first)
                && columns
                    .get(1..4)
                    .is_some_and(|rest| rest.iter().all(|column| column.is_empty()))
            {
                heading.clone_into(&mut category_path);
                continue;
            }
            if !is_menu_number(first) {
                continue;
            }
            let label = columns
                .get(1)
                .and_then(|value| strip_bold(value))
                .ok_or_else(|| invalid_input(format!("menu {first} has no bold display label")))?;
            let description = columns.get(2).copied().unwrap_or_default();
            let setting_values = columns.get(3).copied().unwrap_or_default();
            entries.push(MenuEntry {
                number: first.to_owned(),
                label: label.to_owned(),
                category_path: category_path.clone(),
                description: description.to_owned(),
                setting_values: setting_values.to_owned(),
                class: class_for(first)?,
            });
        }
        Ok(entries)
    }

    fn validate_manifest(entries: &[MenuEntry]) -> AuditResult<()> {
        if entries.len() != EXPECTED_MENU_COUNT {
            return Err(invalid_input(format!(
                "reviewed menu count changed: found {}, expected {EXPECTED_MENU_COUNT}",
                entries.len()
            )));
        }
        let numbers = entries
            .iter()
            .map(|entry| entry.number.as_str())
            .collect::<BTreeSet<_>>();
        if numbers.len() != entries.len() {
            return Err(invalid_input("menu manifest contains duplicate numbers"));
        }
        let categories = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .category_path
                    .split_once(" - ")
                    .map(|(category, _)| category)
            })
            .collect::<BTreeSet<_>>();
        if categories.len() != EXPECTED_CATEGORY_COUNT {
            return Err(invalid_input(format!(
                "reviewed category count changed: found {}, expected {EXPECTED_CATEGORY_COUNT}",
                categories.len()
            )));
        }
        let counts = [
            AuditClass::Value,
            AuditClass::Guarded,
            AuditClass::Information,
            AuditClass::RowOnly,
        ]
        .map(|class| entries.iter().filter(|entry| entry.class == class).count());
        let [
            value_count,
            guarded_count,
            information_count,
            row_only_count,
        ] = counts;
        if counts != [99, 60, 3, EXPECTED_ROW_ONLY_COUNT]
            || value_count + guarded_count + information_count
                != EXPECTED_VALUE_OR_INFORMATION_COUNT
            || row_only_count != EXPECTED_ROW_ONLY_COUNT
        {
            return Err(invalid_input(format!(
                "reviewed V/G/I/R partition changed: {counts:?}"
            )));
        }
        let safe_inspection_count = entries
            .iter()
            .filter(|entry| {
                entry.class == AuditClass::RowOnly
                    && matches!(
                        row_only_policy(&entry.number),
                        Ok(RowOnlyPolicy::SafeInspection)
                    )
            })
            .count();
        let located_not_entered_count = entries
            .iter()
            .filter(|entry| {
                entry.class == AuditClass::RowOnly
                    && row_only_policy(&entry.number)
                        .is_ok_and(RowOnlyPolicy::is_located_not_entered)
            })
            .count();
        if safe_inspection_count != EXPECTED_SAFE_INSPECTION_COUNT
            || located_not_entered_count != EXPECTED_LOCATED_NOT_ENTERED_COUNT
            || safe_inspection_count + located_not_entered_count != EXPECTED_ROW_ONLY_COUNT
        {
            return Err(invalid_input(format!(
                "reviewed row-only handling partition changed: safe-inspection={safe_inspection_count}, located-not-entered={located_not_entered_count}"
            )));
        }
        for entry in entries
            .iter()
            .filter(|entry| entry.class == AuditClass::RowOnly)
        {
            let policy = row_only_policy(&entry.number)?;
            if policy == RowOnlyPolicy::SafeInspection {
                let _oracle = safe_inspection_oracle(&entry.number)?;
            }
        }
        for entry in entries
            .iter()
            .filter(|entry| entry.class != AuditClass::RowOnly)
        {
            if !entry_has_typed_value_oracle(entry) {
                return Err(invalid_input(format!(
                    "menu {} has no reviewed typed current-value oracle",
                    entry.number
                )));
            }
        }
        Ok(())
    }

    fn select_entries<'entry>(
        entries: &'entry [MenuEntry],
        config: &Config,
    ) -> AuditResult<Vec<&'entry MenuEntry>> {
        if let Some(only) = config.only_menu.as_deref() {
            let entry = entries
                .iter()
                .find(|entry| entry.number == only)
                .ok_or_else(|| invalid_input(format!("unknown menu number {only}")))?;
            return Ok(vec![entry]);
        }
        let start_index = config.start_menu.as_deref().map_or(Ok(0), |start| {
            entries
                .iter()
                .position(|entry| entry.number == start)
                .ok_or_else(|| invalid_input(format!("unknown start menu number {start}")))
        })?;
        let limit = config.limit.unwrap_or(entries.len());
        let selected = entries
            .iter()
            .skip(start_index)
            .take(limit)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(invalid_input(
                "audit selection must contain at least one menu",
            ));
        }
        Ok(selected)
    }

    fn coverage_scope(all_entries: &[MenuEntry], selected: &[&MenuEntry]) -> CoverageScope {
        if selected.len() == all_entries.len()
            && selected
                .iter()
                .zip(all_entries)
                .all(|(selected, expected)| selected.number == expected.number)
        {
            CoverageScope::FullManifest
        } else if selected.len() == 1 {
            CoverageScope::SingleMenu
        } else {
            CoverageScope::PartialManifest
        }
    }

    fn field_len(field: &MenuField) -> AuditResult<usize> {
        let len = match field.descriptor.codec {
            FieldCodec::Byte { .. }
            | FieldCodec::Bool
            | FieldCodec::BitBool { .. }
            | FieldCodec::BitField { .. } => 1,
            FieldCodec::FixedString { len, .. } | FieldCodec::Bytes { len } => len,
            FieldCodec::Unsigned { width, .. } | FieldCodec::Signed { width, .. } => {
                usize::from(width)
            }
        };
        if len == 0 {
            return Err(invalid_input(format!(
                "MCP field {} has a zero-byte codec",
                field.descriptor.name
            )));
        }
        Ok(len)
    }

    fn configuration_snapshot_pages() -> AuditResult<Vec<u16>> {
        if MCP_D75_MENU_FIELDS.len() != EXPECTED_CONFIGURATION_SNAPSHOT_FIELD_COUNT {
            return Err(invalid_input(format!(
                "MCP menu-field registry changed: found {}, expected {EXPECTED_CONFIGURATION_SNAPSHOT_FIELD_COUNT}; snapshot scope requires re-review",
                MCP_D75_MENU_FIELDS.len()
            )));
        }
        if usize::from(programming::TOTAL_PAGES) != EXPECTED_MCP_TOTAL_PAGE_COUNT {
            return Err(invalid_input(format!(
                "MCP image page count changed: found {}, expected {EXPECTED_MCP_TOTAL_PAGE_COUNT}; snapshot scope requires re-review",
                programming::TOTAL_PAGES
            )));
        }
        let mut pages = BTreeSet::new();
        for field in MCP_D75_MENU_FIELDS {
            let len = field_len(field)?;
            let end = field
                .descriptor
                .offset
                .checked_add(len - 1)
                .ok_or_else(|| {
                    invalid_input(format!(
                        "MCP field {} extends beyond the address space",
                        field.descriptor.name
                    ))
                })?;
            let start_page = u16::try_from(field.descriptor.offset / programming::PAGE_SIZE)
                .map_err(|_| {
                    invalid_input(format!(
                        "MCP field {} starts beyond the page address space",
                        field.descriptor.name
                    ))
                })?;
            let end_page = u16::try_from(end / programming::PAGE_SIZE).map_err(|_| {
                invalid_input(format!(
                    "MCP field {} ends beyond the page address space",
                    field.descriptor.name
                ))
            })?;
            if end_page >= programming::TOTAL_PAGES {
                return Err(invalid_input(format!(
                    "MCP field {} reaches page 0x{end_page:04X} beyond the image",
                    field.descriptor.name
                )));
            }
            pages.extend(start_page..=end_page);
        }
        if pages.len() != EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT {
            return Err(invalid_input(format!(
                "MCP menu-field snapshot scope changed: found {} pages, expected {EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT}; scope requires re-review",
                pages.len()
            )));
        }
        Ok(pages.into_iter().collect())
    }

    async fn read_configuration_snapshot(
        radio: &mut Radio<EitherTransport>,
        output_dir: &Path,
        expected_pages: &[u16],
        journal: &mut Journal,
        phase: &str,
    ) -> AuditResult<ConfigurationSnapshot> {
        let started = Instant::now();
        let typed_pages: Vec<McpPage> = expected_pages
            .iter()
            .copied()
            .map(McpPage::new)
            .collect::<Result<_, _>>()?;
        let pages = radio
            .read_sparse_memory_pages(&typed_pages)
            .await?
            .into_iter()
            .map(|(page, data)| (page.as_raw(), data))
            .collect::<Vec<_>>();
        let actual_page_numbers = pages.iter().map(|(page, _)| *page).collect::<Vec<_>>();
        if actual_page_numbers != expected_pages {
            return Err(io::Error::other(format!(
                "{phase} configuration snapshot returned unexpected pages"
            ))
            .into());
        }
        let raw = serialize_configuration_snapshot(&pages);
        let sha256 = sha256_bytes(&raw)?;
        let artifact = format!("mcp-{phase}-pages.bin");
        let artifact_path = output_dir.join(&artifact);
        let mut options = OpenOptions::new();
        let configured = options.write(true).create_new(true);
        #[cfg(unix)]
        let configured = configured.mode(0o600);
        let mut artifact_file = configured.open(&artifact_path)?;
        artifact_file.write_all(&raw)?;
        artifact_file.sync_all()?;
        let snapshot = ConfigurationSnapshot {
            pages,
            sha256,
            artifact,
        };
        let page_hashes = snapshot
            .pages
            .iter()
            .map(|(page, bytes)| {
                Ok(json!({
                    "page": format!("0x{page:04X}"),
                    "sha256": sha256_hex(bytes)?,
                }))
            })
            .collect::<AuditResult<Vec<_>>>()?;
        let snapshot_sha256 = hex_sha256(&snapshot.sha256)?;
        journal.append(json!({
            "type": "persistent-configuration-snapshot",
            "phase": phase,
            "read_operation": "MCP-read-only",
            "scope": CONFIGURATION_SNAPSHOT_SCOPE,
            "scope_limits": {
                "captured_full_pages": EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT,
                "total_mcp_pages": EXPECTED_MCP_TOTAL_PAGE_COUNT,
                "excluded_mcp_pages": EXPECTED_MCP_TOTAL_PAGE_COUNT - EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT,
                "captured_registry_fields": EXPECTED_CONFIGURATION_SNAPSHOT_FIELD_COUNT,
                "comparison_semantics": "final-before-versus-after-byte-equality",
                "proves_no_intermediate_write": false,
                "covers_non_mcp_transient_or_volatile_state": false,
            },
            "schema": {
                "model": MCP_D75_SCHEMA_MODEL,
                "firmware": MCP_D75_SCHEMA_FIRMWARE,
                "version": MCP_D75_SCHEMA_VERSION,
                "source_sha256": MCP_D75_SOURCE_SHA256,
                "field_count": MCP_D75_MENU_FIELDS.len(),
            },
            "page_count": snapshot.pages.len(),
            "byte_count": snapshot.pages.len() * programming::PAGE_SIZE,
            "raw_artifact": snapshot.artifact,
            "raw_artifact_format": "ordered-records-of-u16le-page-number-followed-by-256-raw-page-bytes-no-header",
            "raw_artifact_byte_count": raw.len(),
            "page_numbers": snapshot.pages.iter().map(|(page, _)| format!("0x{page:04X}")).collect::<Vec<_>>(),
            "page_hashes": page_hashes,
            "snapshot_sha256": snapshot_sha256,
            "elapsed_ms": millis(started.elapsed()),
            "result": "pass",
        }))?;
        Ok(snapshot)
    }

    fn require_configuration_unchanged(
        before: &ConfigurationSnapshot,
        after: &ConfigurationSnapshot,
        journal: &mut Journal,
    ) -> AuditResult<()> {
        let differing_pages = before
            .pages
            .iter()
            .zip(&after.pages)
            .filter_map(|((before_page, before_bytes), (after_page, after_bytes))| {
                if before_page != after_page {
                    return Some(json!({
                        "before_page": format!("0x{before_page:04X}"),
                        "after_page": format!("0x{after_page:04X}"),
                        "reason": "page-order-or-identity-mismatch",
                    }));
                }
                let byte_offsets = before_bytes
                    .iter()
                    .zip(after_bytes)
                    .enumerate()
                    .filter_map(|(offset, (before, after))| (before != after).then_some(offset))
                    .collect::<Vec<_>>();
                (!byte_offsets.is_empty()).then(|| {
                    json!({
                        "page": format!("0x{before_page:04X}"),
                        "differing_byte_offsets": byte_offsets,
                    })
                })
            })
            .collect::<Vec<_>>();
        let exact_match =
            configuration_snapshots_match(before, after) && differing_pages.is_empty();
        let before_sha256 = hex_sha256(&before.sha256)?;
        let after_sha256 = hex_sha256(&after.sha256)?;
        journal.append(json!({
            "type": "persistent-configuration-nonmutation-assertion",
            "scope": CONFIGURATION_SNAPSHOT_SCOPE,
            "comparison": "exact-final-page-identity-and-byte-equality-within-declared-350-page-scope",
            "proves_no_intermediate_write": false,
            "covers_non_mcp_transient_or_volatile_state": false,
            "before_sha256": before_sha256,
            "after_sha256": after_sha256,
            "before_raw_artifact": before.artifact,
            "after_raw_artifact": after.artifact,
            "before_page_count": before.pages.len(),
            "after_page_count": after.pages.len(),
            "differing_pages": differing_pages,
            "result": if exact_match { "pass" } else { "fail" },
        }))?;
        if exact_match {
            Ok(())
        } else {
            Err(io::Error::other(
                "one or more bytes in the declared 350-page MCP snapshot scope differ at audit end; refusing a pass verdict",
            )
            .into())
        }
    }

    fn serialize_configuration_snapshot(pages: &[(u16, [u8; programming::PAGE_SIZE])]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(pages.len() * (2 + programming::PAGE_SIZE));
        for (page, bytes) in pages {
            raw.extend_from_slice(&page.to_le_bytes());
            raw.extend_from_slice(bytes);
        }
        raw
    }

    fn sha256_bytes(bytes: &[u8]) -> AuditResult<[u8; 32]> {
        let byte_length = u64::try_from(bytes.len())
            .map_err(|_| invalid_input("evidence is too large for SHA-256 length encoding"))?;
        let bit_length = byte_length
            .checked_mul(8)
            .ok_or_else(|| invalid_input("evidence bit length overflowed SHA-256 encoding"))?;
        let mut padded = Vec::with_capacity(bytes.len().saturating_add(128));
        padded.extend_from_slice(bytes);
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_length.to_be_bytes());

        let mut state = [
            0x6a09_e667,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
            0x510e_527f,
            0x9b05_688c,
            0x1f83_d9ab,
            0x5be0_cd19,
        ];
        for block in padded.chunks_exact(64) {
            sha256_compress(&mut state, block)?;
        }
        let mut digest = [0_u8; 32];
        for (chunk, word) in digest.chunks_exact_mut(4).zip(state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        Ok(digest)
    }

    fn sha256_compress(state: &mut [u32; 8], block: &[u8]) -> AuditResult<()> {
        if block.len() != 64 {
            return Err(invalid_input(
                "internal SHA-256 compression block has the wrong length",
            ));
        }
        let mut schedule = [0_u32; 64];
        let words = block.chunks_exact(4);
        if !words.remainder().is_empty() {
            return Err(invalid_input(
                "internal SHA-256 word split left trailing bytes",
            ));
        }
        for (slot, word) in schedule.iter_mut().take(16).zip(words) {
            let [a, b, c, d] = word else {
                return Err(invalid_input("internal SHA-256 word has the wrong length"));
            };
            *slot = u32::from_be_bytes([*a, *b, *c, *d]);
        }
        for index in 16..64 {
            let word_15 = sha256_schedule_word(&schedule, index - 15)?;
            let word_2 = sha256_schedule_word(&schedule, index - 2)?;
            let small_sigma_0 = word_15.rotate_right(7) ^ word_15.rotate_right(18) ^ (word_15 >> 3);
            let small_sigma_1 = word_2.rotate_right(17) ^ word_2.rotate_right(19) ^ (word_2 >> 10);
            let extended = sha256_schedule_word(&schedule, index - 16)?
                .wrapping_add(small_sigma_0)
                .wrapping_add(sha256_schedule_word(&schedule, index - 7)?)
                .wrapping_add(small_sigma_1);
            let slot = schedule
                .get_mut(index)
                .ok_or_else(|| invalid_input("internal SHA-256 schedule index is invalid"))?;
            *slot = extended;
        }
        let [
            mut working_a,
            mut working_b,
            mut working_c,
            mut working_d,
            mut working_e,
            mut working_f,
            mut working_g,
            mut working_h,
        ] = *state;
        for (round_constant, schedule_word) in SHA256_ROUND_CONSTANTS.iter().zip(schedule) {
            let big_sigma_1 =
                working_e.rotate_right(6) ^ working_e.rotate_right(11) ^ working_e.rotate_right(25);
            let choice = (working_e & working_f) ^ ((!working_e) & working_g);
            let temporary_1 = working_h
                .wrapping_add(big_sigma_1)
                .wrapping_add(choice)
                .wrapping_add(*round_constant)
                .wrapping_add(schedule_word);
            let big_sigma_0 =
                working_a.rotate_right(2) ^ working_a.rotate_right(13) ^ working_a.rotate_right(22);
            let majority =
                (working_a & working_b) ^ (working_a & working_c) ^ (working_b & working_c);
            let temporary_2 = big_sigma_0.wrapping_add(majority);

            working_h = working_g;
            working_g = working_f;
            working_f = working_e;
            working_e = working_d.wrapping_add(temporary_1);
            working_d = working_c;
            working_c = working_b;
            working_b = working_a;
            working_a = temporary_1.wrapping_add(temporary_2);
        }
        let [
            state_a,
            state_b,
            state_c,
            state_d,
            state_e,
            state_f,
            state_g,
            state_h,
        ] = *state;
        *state = [
            state_a.wrapping_add(working_a),
            state_b.wrapping_add(working_b),
            state_c.wrapping_add(working_c),
            state_d.wrapping_add(working_d),
            state_e.wrapping_add(working_e),
            state_f.wrapping_add(working_f),
            state_g.wrapping_add(working_g),
            state_h.wrapping_add(working_h),
        ];
        Ok(())
    }

    fn sha256_schedule_word(schedule: &[u32; 64], index: usize) -> AuditResult<u32> {
        schedule
            .get(index)
            .copied()
            .ok_or_else(|| invalid_input("internal SHA-256 schedule read is out of range"))
    }

    fn sha256_hex(bytes: &[u8]) -> AuditResult<String> {
        hex_sha256(&sha256_bytes(bytes)?)
    }

    fn hex_sha256(digest: &[u8; 32]) -> AuditResult<String> {
        let mut hex = String::with_capacity(digest.len() * 2);
        for word in digest.chunks_exact(4) {
            let [a, b, c, d] = word else {
                return Err(invalid_input("internal SHA-256 digest word is incomplete"));
            };
            std::fmt::write(
                &mut hex,
                format_args!("{:08x}", u32::from_be_bytes([*a, *b, *c, *d])),
            )?;
        }
        Ok(hex)
    }

    fn configuration_snapshots_match(
        before: &ConfigurationSnapshot,
        after: &ConfigurationSnapshot,
    ) -> bool {
        before.pages == after.pages
    }

    fn class_for(number: &str) -> AuditResult<AuditClass> {
        let mut matched = None;
        for (numbers, class) in [
            (VALUE_NUMBERS, AuditClass::Value),
            (GUARDED_NUMBERS, AuditClass::Guarded),
            (INFORMATION_NUMBERS, AuditClass::Information),
            (ROW_ONLY_NUMBERS, AuditClass::RowOnly),
        ] {
            if numbers
                .split_ascii_whitespace()
                .any(|candidate| candidate == number)
            {
                if matched.is_some() {
                    return Err(invalid_input(format!(
                        "menu {number} appears in more than one safety class"
                    )));
                }
                matched = Some(class);
            }
        }
        matched.ok_or_else(|| invalid_input(format!("menu {number} has no safety class")))
    }

    fn menu_value_kind(number: &str) -> &'static str {
        if ROW_ONLY_NUMBERS
            .split_ascii_whitespace()
            .any(|candidate| candidate == number)
        {
            return if matches!(row_only_policy(number), Ok(RowOnlyPolicy::SafeInspection)) {
                "read-only-safe-inspection-page-current-state"
            } else {
                "row-label-only-never-entered"
            };
        }
        match number {
            "181" | "406" | "509" | "551" | "631" => {
                "current-checkbox-state-from-framebuffer-and-exact-labels"
            }
            "530" => "current-typed-low-high-speed-and-unit",
            "591" => "current-network-selection",
            "840" => "live-microsd-capacity-telemetry",
            "912" | "913" => "current-equalizer-levels",
            "922" => "live-battery-level-graphic-telemetry",
            "980" => "current-custom-automation-usb-storage-apply-setting",
            "991" => "running-firmware-version",
            "91A" => "current-documented-level-value-via-reviewed-row-route",
            _ => "current-documented-legal-setting-value-not-default",
        }
    }

    fn entry_has_typed_value_oracle(entry: &MenuEntry) -> bool {
        entry.class != AuditClass::RowOnly && value_domain(entry).is_some()
    }

    fn direct_access_keys(number: &str) -> AuditResult<Vec<FrontPanelKey>> {
        let route = direct_access_route(number)?;
        route
            .digits()
            .into_iter()
            .map(|digit| digit_key(digit + b'0'))
            .collect::<AuditResult<Vec<_>>>()
    }

    fn direct_access_route(number: &str) -> AuditResult<GuardedDecimalRoute> {
        let bytes = number.as_bytes();
        let &[first, second, third] = bytes else {
            return Err(invalid_input(format!(
                "menu {number} has no complete decimal direct-access sequence"
            )));
        };
        if !bytes.iter().all(u8::is_ascii_digit) {
            return Err(invalid_input(format!(
                "menu {number} has no complete decimal direct-access sequence"
            )));
        }
        Ok(GuardedDecimalRoute::new([
            first - b'0',
            second - b'0',
            third - b'0',
        ])?)
    }

    fn row_only_anchor(number: &str) -> AuditResult<&'static str> {
        let anchor = match number {
            "100" => "101",
            "163" | "164" => "161",
            "200" | "201" | "203" | "204" | "210" | "220" | "230" => "202",
            "300" => "302",
            "310" | "312" => "311",
            "401" => "402",
            "411" => "412",
            "500" => "501",
            "503" | "504" | "516" => "505",
            "560" | "562" | "564" => "563",
            "572" | "583" | "585" | "588" => "570",
            "594" | "595" => "593",
            "600" | "610" => "611",
            "651" | "652" | "653" | "654" => "645",
            "710" => "701",
            "800" | "801" | "802" | "803" | "810" | "811" | "812" | "813" | "820" | "830" => "840",
            "903" => "902",
            "911" => "910",
            "931" | "932" | "933" | "934" | "935" => "922",
            "946" => "945",
            "950" => "970",
            "999" => "991",
            _ => {
                return Err(invalid_input(format!(
                    "menu {number} has no reviewed harmless-anchor route"
                )));
            }
        };
        Ok(anchor)
    }

    fn row_only_policy(number: &str) -> AuditResult<RowOnlyPolicy> {
        let mut policy = None;
        for (numbers, candidate) in [
            (SAFE_INSPECTION_NUMBERS, RowOnlyPolicy::SafeInspection),
            (DESTRUCTIVE_ACTION_NUMBERS, RowOnlyPolicy::DestructiveAction),
            (
                MULTI_RECORD_EDITOR_NUMBERS,
                RowOnlyPolicy::MultiRecordEditor,
            ),
        ] {
            if numbers
                .split_ascii_whitespace()
                .any(|candidate_number| candidate_number == number)
            {
                if policy.is_some() {
                    return Err(invalid_input(format!(
                        "row-only menu {number} appears in more than one handling policy"
                    )));
                }
                policy = Some(candidate);
            }
        }
        policy
            .ok_or_else(|| invalid_input(format!("row-only menu {number} has no handling policy")))
    }

    fn safe_inspection_oracle(number: &str) -> AuditResult<SafeInspectionOracle> {
        let oracle = match number {
            "100" => SafeInspectionOracle::ProgrammableVfo,
            "401" => SafeInspectionOracle::ActiveChoice {
                field: "gps.MyPositionSelect",
                labels: &MY_POSITION_ROWS,
            },
            "500" => SafeInspectionOracle::ShortText {
                field: "aprs.MyCallsign",
                blank_display: Some("NOCALL"),
            },
            "503" => SafeInspectionOracle::ActiveChoice {
                field: "aprs.StatusTextSelect",
                labels: &STATUS_TEXT_ROWS,
            },
            "504" => SafeInspectionOracle::ActiveChoice {
                field: "aprs.PacketPathType",
                labels: &PACKET_PATH_ROWS,
            },
            "516" => SafeInspectionOracle::ActiveChoice {
                field: "aprs.ObjectUsedNo",
                labels: &OBJECT_ROWS,
            },
            "562" => SafeInspectionOracle::ShortText {
                field: "aprs.AutoReplyTargetCall",
                blank_display: None,
            },
            "572" => SafeInspectionOracle::ShortText {
                field: "aprs.SpecialCall",
                blank_display: None,
            },
            "585" => SafeInspectionOracle::ShortText {
                field: "aprs.UIfloodAliases",
                blank_display: None,
            },
            "588" => SafeInspectionOracle::ShortText {
                field: "aprs.UItraceAliases",
                blank_display: None,
            },
            "651" => SafeInspectionOracle::DvGatewayCallsign,
            "911" => SafeInspectionOracle::EqualizerCheckboxes,
            "935" => SafeInspectionOracle::BluetoothInformation,
            "950" => SafeInspectionOracle::DynamicDateTime,
            _ => {
                return Err(invalid_input(format!(
                    "menu {number} has no reviewed safe-inspection oracle"
                )));
            }
        };
        Ok(oracle)
    }

    const fn safe_inspection_oracle_name(oracle: SafeInspectionOracle) -> &'static str {
        match oracle {
            SafeInspectionOracle::ProgrammableVfo => {
                "screen-two-ordered-legal-band-a-frequency-limits"
            }
            SafeInspectionOracle::ActiveChoice { .. } => {
                "before-mcp-selected-index-plus-one-aligned-USE-marker"
            }
            SafeInspectionOracle::ShortText { .. } => {
                "before-mcp-exact-decoded-short-text-in-value-region"
            }
            SafeInspectionOracle::DvGatewayCallsign => {
                "before-mcp-selected-dv-gateway-callsign-plus-exact-blue-selected-numbered-row"
            }
            SafeInspectionOracle::EqualizerCheckboxes => {
                "before-mcp-three-bits-plus-exact-labels-and-framebuffer-checkboxes"
            }
            SafeInspectionOracle::BluetoothInformation => {
                "before-mcp-device-name-plus-runtime-address-and-class-formats"
            }
            SafeInspectionOracle::DynamicDateTime => {
                "before-mcp-time-zone-domain-plus-live-date-time-zone-formats"
            }
        }
    }

    fn safe_inspection_title(entry: &MenuEntry) -> &str {
        match entry.number.as_str() {
            // The numbered row is labeled "Object", while its reviewed
            // read-only stock V1.03 page has the expanded title below.
            "516" => "APRS Object",
            "935" => "Bluetooth Device Information",
            "950" => "Date & Time",
            _ => &entry.label,
        }
    }

    fn safe_inspection_payload(
        menu_number: &str,
        oracle: SafeInspectionOracle,
        screen: &CapturedScreen,
        before: &ConfigurationSnapshot,
    ) -> AuditResult<Value> {
        match oracle {
            SafeInspectionOracle::ProgrammableVfo => programmable_vfo_payload(screen),
            SafeInspectionOracle::ActiveChoice { field, labels } => {
                let raw = snapshot_unsigned_field(before, field)?;
                let index = usize::try_from(raw).map_err(|_| {
                    invalid_input(format!("menu {menu_number} MCP choice index is too large"))
                })?;
                let expected = labels.get(index).ok_or_else(|| {
                    invalid_input(format!(
                        "menu {menu_number} MCP choice {raw} has no reviewed screen label"
                    ))
                })?;
                let marker = aligned_use_marker(screen, expected).ok_or_else(|| {
                    io::Error::other(format!(
                        "menu {menu_number} did not show exactly one USE marker aligned with its MCP-selected row"
                    ))
                })?;
                Ok(json!({
                    "mcp_field": field,
                    "mcp_raw": raw,
                    "expected_active_label": expected,
                    "USE_marker": marker,
                    "comparison": "exact-canonical-label-and-vertical-marker-alignment",
                }))
            }
            SafeInspectionOracle::ShortText {
                field,
                blank_display,
            } => {
                let expected = snapshot_text_field(before, field)?;
                let displayed = if expected.is_empty() {
                    blank_display.unwrap_or("")
                } else {
                    &expected
                };
                require_exact_short_text(screen, displayed).map_err(|error| {
                    io::Error::other(format!(
                        "menu {menu_number} did not exactly render its decoded MCP short text: {error}"
                    ))
                })?;
                Ok(json!({
                    "mcp_field": field,
                    "decoded_length": expected.chars().count(),
                    "decoded_sha256": sha256_hex(expected.as_bytes())?,
                    "displayed_blank_policy": blank_display,
                    "comparison": "one-exact-decoded-value-region-observation-and-no-conflicting-value-text",
                }))
            }
            SafeInspectionOracle::DvGatewayCallsign => dv_gateway_callsign_payload(screen, before),
            SafeInspectionOracle::EqualizerCheckboxes => equalizer_checkbox_payload(screen, before),
            SafeInspectionOracle::BluetoothInformation => {
                bluetooth_information_payload(screen, before)
            }
            SafeInspectionOracle::DynamicDateTime => dynamic_date_time_payload(screen, before),
        }
    }

    fn snapshot_field_value(
        snapshot: &ConfigurationSnapshot,
        field_name: &str,
    ) -> AuditResult<DecodedFieldValue> {
        let field = menu_field(field_name)
            .ok_or_else(|| invalid_input(format!("MCP schema has no field {field_name:?}")))?;
        let len = field_len(field)?;
        let last_offset = field
            .descriptor
            .offset
            .checked_add(len.saturating_sub(1))
            .ok_or_else(|| invalid_input(format!("MCP field {field_name} range overflowed")))?;
        let first_page = field.descriptor.offset / programming::PAGE_SIZE;
        let last_page = last_offset / programming::PAGE_SIZE;
        for page in first_page..=last_page {
            let page = u16::try_from(page)
                .map_err(|_| invalid_input(format!("MCP field {field_name} page overflowed")))?;
            if snapshot
                .pages
                .binary_search_by_key(&page, |(candidate, _)| *candidate)
                .is_err()
            {
                return Err(invalid_input(format!(
                    "before-audit snapshot omitted page 0x{page:04X} required by {field_name}"
                )));
            }
        }
        let image_len = usize::from(programming::TOTAL_PAGES)
            .checked_mul(programming::PAGE_SIZE)
            .ok_or_else(|| invalid_input("MCP image length overflowed"))?;
        let mut image = vec![0_u8; image_len];
        for (page, bytes) in &snapshot.pages {
            let start = usize::from(*page)
                .checked_mul(programming::PAGE_SIZE)
                .ok_or_else(|| invalid_input("MCP snapshot page offset overflowed"))?;
            let end = start
                .checked_add(programming::PAGE_SIZE)
                .ok_or_else(|| invalid_input("MCP snapshot page end overflowed"))?;
            let destination = image.get_mut(start..end).ok_or_else(|| {
                invalid_input(format!("MCP snapshot page 0x{page:04X} is out of range"))
            })?;
            destination.copy_from_slice(bytes);
        }
        Ok(field.read(&image)?)
    }

    fn snapshot_unsigned_field(
        snapshot: &ConfigurationSnapshot,
        field_name: &str,
    ) -> AuditResult<u64> {
        match snapshot_field_value(snapshot, field_name)? {
            DecodedFieldValue::Unsigned(value) => Ok(value),
            value => Err(invalid_input(format!(
                "MCP field {field_name} decoded as {value:?}, not unsigned"
            ))),
        }
    }

    fn snapshot_text_field(
        snapshot: &ConfigurationSnapshot,
        field_name: &str,
    ) -> AuditResult<String> {
        match snapshot_field_value(snapshot, field_name)? {
            DecodedFieldValue::Text(value) => Ok(value),
            value => Err(invalid_input(format!(
                "MCP field {field_name} decoded as {value:?}, not text"
            ))),
        }
    }

    fn snapshot_bool_field(
        snapshot: &ConfigurationSnapshot,
        field_name: &str,
    ) -> AuditResult<bool> {
        match snapshot_field_value(snapshot, field_name)? {
            DecodedFieldValue::Bool(value) => Ok(value),
            value => Err(invalid_input(format!(
                "MCP field {field_name} decoded as {value:?}, not boolean"
            ))),
        }
    }

    fn aligned_use_marker(screen: &CapturedScreen, expected_label: &str) -> Option<Value> {
        let expected = canonical_text(expected_label);
        let markers = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                bounds.x() > 0.70 && (0.10..0.85).contains(&center_y)
            })
            .filter(|observation| canonical_text(observation.text()) == "use")
            .collect::<Vec<_>>();
        let marker = unique_physical_text_locus(&markers)?;
        let marker_center = marker.bounds().y() + marker.bounds().height() / 2.0;
        let aligned_labels = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| canonical_selected_label(observation.text()) == expected)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                (0.10..0.85).contains(&center_y)
                    && (center_y - marker_center).abs() <= 10.0 / SCREEN_HEIGHT_F32
            })
            .collect::<Vec<_>>();
        let label = unique_physical_text_locus(&aligned_labels)?;
        let label_center = label.bounds().y() + label.bounds().height() / 2.0;
        Some(json!({
            "text": "USE",
            "label_center_y": label_center,
            "marker_center_y": marker_center,
            "maximum_alignment_error_pixels": 10,
        }))
    }

    fn unique_physical_text_locus<'observation>(
        observations: &[&'observation TextObservation],
    ) -> Option<&'observation TextObservation> {
        let first = *observations.first()?;
        observations
            .iter()
            .all(|observation| bounds_substantially_overlap(&observation.bounds(), &first.bounds()))
            .then_some(first)
    }

    fn body_observations(screen: &CapturedScreen) -> Vec<&TextObservation> {
        screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center = bounds.y() + bounds.height() / 2.0;
                (0.15..0.85).contains(&center)
            })
            .collect()
    }

    fn require_exact_short_text(screen: &CapturedScreen, expected: &str) -> AuditResult<()> {
        let expected = canonical_value_text(expected);
        let semantic = body_observations(screen)
            .into_iter()
            .filter(|observation| !canonical_value_text(observation.text()).is_empty())
            .collect::<Vec<_>>();
        if expected.is_empty() {
            if semantic.is_empty() {
                return Ok(());
            }
            return Err(io::Error::other("expected an empty input region").into());
        }
        if unique_physical_text_locus(&semantic).is_some()
            && semantic
                .iter()
                .any(|observation| canonical_value_text(observation.text()) == expected)
        {
            Ok(())
        } else {
            Err(io::Error::other(
                "the value region did not contain exactly one physical text locus with the expected value",
            )
            .into())
        }
    }

    fn dv_gateway_callsign_payload(
        screen: &CapturedScreen,
        before: &ConfigurationSnapshot,
    ) -> AuditResult<Value> {
        let selected = snapshot_unsigned_field(before, "dv.MyCallsignSelectDvGateway")?;
        if selected > 5 {
            return Err(invalid_input(format!(
                "DV Gateway callsign selection {selected} is outside 0..=5"
            )));
        }
        let field = format!("dv.MyCallsignDvGatewayList[{selected}].MyCallsignDvGateway");
        let callsign = snapshot_text_field(before, &field)?;
        let memo_field = format!("dv.MyCallsignDvGatewayList[{selected}].MemoDvGateway");
        let memo = snapshot_text_field(before, &memo_field)?;
        if callsign.is_empty() && !memo.is_empty() {
            return Err(invalid_input(
                "Menu 651 selected DV Gateway row has a memo but no callsign",
            ));
        }
        let ordinal = selected
            .checked_add(1)
            .ok_or_else(|| invalid_input("Menu 651 selected row ordinal overflowed"))?;
        let displayed = if callsign.is_empty() {
            format!("{ordinal}:")
        } else if memo.is_empty() {
            format!("{ordinal}:{callsign}")
        } else {
            format!("{ordinal}:{callsign} /{memo}")
        };
        let bands = v103_selection_bands(&screen.frame);
        let [band] = bands.as_slice() else {
            return Err(io::Error::other(
                "menu 651 did not show exactly one selected callsign row",
            )
            .into());
        };
        let selected_index = usize::try_from(selected)
            .map_err(|_| invalid_input("Menu 651 selection index does not fit usize"))?;
        let expected_top = 20_usize
            .checked_add(
                selected_index
                    .checked_mul(24)
                    .ok_or_else(|| invalid_input("Menu 651 selected-row offset overflowed"))?,
            )
            .ok_or_else(|| invalid_input("Menu 651 selected-row position overflowed"))?;
        if band.top() != expected_top
            || band.bottom_exclusive() != expected_top.saturating_add(24)
            || !selected_matches_label(screen, &displayed)
        {
            return Err(io::Error::other(
                "menu 651 blue-selected row did not exactly match its MCP-selected numbered DV Gateway callsign and memo",
            )
            .into());
        }
        Ok(json!({
            "selection_field": "dv.MyCallsignSelectDvGateway",
            "selected_index": selected,
            "selected_callsign_field": field,
            "selected_callsign_sha256": sha256_hex(callsign.as_bytes())?,
            "selected_callsign_length": callsign.chars().count(),
            "selected_memo_field": memo_field,
            "selected_memo_sha256": sha256_hex(memo.as_bytes())?,
            "selected_memo_length": memo.chars().count(),
            "selected_row_ordinal": ordinal,
            "selection_band": {
                "top": band.top(),
                "bottom_exclusive": band.bottom_exclusive(),
            },
            "comparison": "exact-MCP-indexed-numbered-callsign-and-memo-row-plus-blue-selection-band",
        }))
    }

    fn equalizer_checkbox_payload(
        screen: &CapturedScreen,
        before: &ConfigurationSnapshot,
    ) -> AuditResult<Value> {
        let fields = [
            ("RX EQ", "radio.RxEqualizer"),
            ("TX EQ(FM, NFM)", "radio.TxEqualizerFmNfm"),
            ("TX EQ(DV)", "radio.TxEqualizerDv"),
        ];
        let expected = fields
            .iter()
            .map(|(label, field)| {
                Ok(format!(
                    "{label}={}",
                    if snapshot_bool_field(before, field)? {
                        "checked"
                    } else {
                        "unchecked"
                    }
                ))
            })
            .collect::<AuditResult<Vec<_>>>()?;
        let actual = checkbox_payload(screen, &["RX EQ", "TX EQ(FM, NFM)", "TX EQ(DV)"])
            .ok_or_else(|| {
                io::Error::other(
                    "menu 911 did not show all three exact EQ labels and framebuffer checkbox states",
                )
            })?;
        if actual != expected {
            return Err(io::Error::other(
                "menu 911 framebuffer checkbox states differ from the before-audit MCP bits",
            )
            .into());
        }
        Ok(json!({
            "fields": fields.map(|(_, field)| field),
            "rows": actual,
            "comparison": "exact-labels-plus-framebuffer-checkbox-pixels-equal-before-MCP-bits",
        }))
    }

    fn bluetooth_information_payload(
        screen: &CapturedScreen,
        before: &ConfigurationSnapshot,
    ) -> AuditResult<Value> {
        let device_name = snapshot_text_field(before, "radio.BluetoothDeviceName")?;
        if device_name.is_empty()
            || body_observations(screen)
                .iter()
                .filter(|observation| {
                    canonical_value_text(observation.text()) == canonical_value_text(&device_name)
                })
                .count()
                != 1
        {
            return Err(io::Error::other(
                "menu 935 did not show exactly one decoded MCP Bluetooth device name",
            )
            .into());
        }
        let address = menu_935_bluetooth_address_identity(screen)?;
        let (class, class_format) = menu_935_bluetooth_class_identity(screen)?;
        Ok(json!({
            "mcp_field": "radio.BluetoothDeviceName",
            "device_name_sha256": sha256_hex(device_name.as_bytes())?,
            "device_name_length": device_name.chars().count(),
            "runtime_device_address_sha256": sha256_hex(address.as_bytes())?,
            "runtime_device_address_format": "six-colon-separated-hex-octets-with-at-most-one-uppercase-O-observed-as-zero-at-one-physical-locus",
            "runtime_device_class_sha256": sha256_hex(class.as_bytes())?,
            "runtime_device_class_format": class_format,
        }))
    }

    fn dynamic_date_time_payload(
        screen: &CapturedScreen,
        before: &ConfigurationSnapshot,
    ) -> AuditResult<Value> {
        let raw = snapshot_unsigned_field(before, "radio.TimeZone")?;
        let field = menu_field("radio.TimeZone")
            .ok_or_else(|| invalid_input("MCP schema omitted radio.TimeZone"))?;
        if !field.allowed_values.contains(&raw) {
            return Err(invalid_input(format!(
                "menu 950 MCP time-zone raw value {raw} is outside the reviewed finite domain"
            )));
        }
        let date = menu_950_live_value(screen, 0.25, 0.40, looks_like_date);
        let time = menu_950_live_value(screen, 0.50, 0.66, looks_like_time);
        let timezone = menu_950_live_value(screen, 0.75, 0.90, looks_like_utc_offset);
        let (Some(date), Some(time), Some(timezone)) = (date, time, timezone) else {
            return Err(io::Error::other(
                "menu 950 did not expose valid live date, time, and UTC-offset formats in one capture",
            )
            .into());
        };
        Ok(json!({
            "mcp_field": "radio.TimeZone",
            "mcp_raw": raw,
            "mcp_raw_in_reviewed_finite_domain": true,
            "displayed_date_sha256": sha256_hex(date.as_bytes())?,
            "displayed_time_sha256": sha256_hex(time.as_bytes())?,
            "displayed_timezone_sha256": sha256_hex(timezone.as_bytes())?,
            "live_clock_quiescence_claimed": false,
            "comparison": "MCP-time-zone-domain-and-unique-right-column-live-date-time-UTC-offset-syntax-in-three-exact-value-rows",
        }))
    }

    fn menu_950_live_value(
        screen: &CapturedScreen,
        minimum_center_y: f32,
        maximum_center_y: f32,
        matches_format: fn(&str) -> bool,
    ) -> Option<&str> {
        let candidates = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                bounds.x() >= 0.40
                    && (minimum_center_y..maximum_center_y).contains(&center_y)
                    && matches_format(observation.text().trim())
            })
            .collect::<Vec<_>>();
        unique_physical_text_locus(&candidates).map(|observation| observation.text().trim())
    }

    fn programmable_vfo_payload(screen: &CapturedScreen) -> AuditResult<Value> {
        let mut frequencies = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter_map(|observation| {
                parse_frequency_khz(observation.text()).map(|khz| (observation, khz))
            })
            .collect::<Vec<_>>();
        frequencies.sort_by(|(left, _), (right, _)| {
            let left_center = left.bounds().y() + left.bounds().height() / 2.0;
            let right_center = right.bounds().y() + right.bounds().height() / 2.0;
            left_center.total_cmp(&right_center)
        });
        let [(lower_observation, lower), (upper_observation, upper)] = frequencies.as_slice()
        else {
            return Err(io::Error::other(
                "menu 100 did not show exactly two confident frequency limits",
            )
            .into());
        };
        let legal_bands = [
            (136_000_u32, 174_000_u32),
            (216_000, 260_000),
            (410_000, 470_000),
        ];
        let same_legal_band = legal_bands.iter().any(|(minimum, maximum)| {
            (*minimum..=*maximum).contains(lower) && (*minimum..=*maximum).contains(upper)
        });
        if lower > upper || !same_legal_band {
            return Err(io::Error::other(
                "menu 100 limits are not ordered within one documented Band-A receive range",
            )
            .into());
        }
        Ok(json!({
            "lower_display": lower_observation.text(),
            "upper_display": upper_observation.text(),
            "lower_khz": lower,
            "upper_khz": upper,
            "documented_band_a_ranges_khz": legal_bands,
            "ordered": true,
            "same_legal_band": true,
        }))
    }

    fn parse_frequency_khz(text: &str) -> Option<u32> {
        let trimmed = text
            .trim()
            .strip_suffix("MHz")
            .unwrap_or_else(|| text.trim())
            .trim();
        let (whole, fraction) = match trimmed.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (trimmed, None),
        };
        if whole.len() != 3 || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let whole = whole.parse::<u32>().ok()?;
        let first_three = match fraction {
            Some(fraction)
                if (3..=5).contains(&fraction.len())
                    && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                fraction.get(..3)?.parse::<u32>().ok()?
            }
            Some(_) => return None,
            None => 0,
        };
        whole.checked_mul(1_000)?.checked_add(first_three)
    }

    fn normalized_menu_935_bluetooth_address(text: &str) -> Option<String> {
        if looks_like_bluetooth_address(text.trim()) {
            return Some(text.trim().to_ascii_uppercase());
        }
        let parts = text.trim().split(':').collect::<Vec<_>>();
        if parts.len() != 6 || parts.iter().any(|part| part.len() != 2) {
            return None;
        }
        let mut normalized = String::with_capacity(17);
        let mut observed_o_count = 0_usize;
        for (index, part) in parts.iter().enumerate() {
            if index != 0 {
                normalized.push(':');
            }
            for byte in part.bytes() {
                normalized.push(match byte {
                    b'0'..=b'9' | b'A'..=b'F' => char::from(byte),
                    b'a'..=b'f' => char::from(byte.to_ascii_uppercase()),
                    b'O' if observed_o_count == 0 => {
                        observed_o_count += 1;
                        '0'
                    }
                    _ => return None,
                });
            }
        }
        Some(normalized)
    }

    fn looks_like_bluetooth_address(text: &str) -> bool {
        let parts = text.split(':').collect::<Vec<_>>();
        parts.len() == 6
            && parts
                .iter()
                .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
    }

    fn menu_935_bluetooth_address_identity(screen: &CapturedScreen) -> AuditResult<String> {
        let candidates = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                bounds.x() < 0.25 && (0.50..0.68).contains(&center_y)
            })
            .filter_map(|observation| {
                normalized_menu_935_bluetooth_address(observation.text())
                    .map(|identity| (observation, identity))
            })
            .collect::<Vec<_>>();
        let loci = candidates
            .iter()
            .map(|(observation, _)| *observation)
            .collect::<Vec<_>>();
        if unique_physical_text_locus(&loci).is_none() {
            return Err(io::Error::other(
                "menu 935 did not expose one physical Bluetooth address locus",
            )
            .into());
        }
        let identities = candidates
            .into_iter()
            .map(|(_, identity)| identity)
            .collect::<BTreeSet<_>>();
        if identities.len() != 1 {
            return Err(io::Error::other(
                "menu 935 Bluetooth address observations did not resolve to one identity",
            )
            .into());
        }
        identities
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("menu 935 Bluetooth address identity was empty").into())
    }

    fn menu_935_bluetooth_class_identity(
        screen: &CapturedScreen,
    ) -> AuditResult<(String, &'static str)> {
        let candidates = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                bounds.x() > 0.55 && (0.75..0.90).contains(&center_y)
            })
            .filter(|observation| looks_like_bluetooth_device_class(observation.text()))
            .filter_map(|observation| {
                bluetooth_device_class_format(observation.text())
                    .map(|format| (observation, canonical_text(observation.text()), format))
            })
            .collect::<Vec<_>>();
        let loci = candidates
            .iter()
            .map(|(observation, _, _)| *observation)
            .collect::<Vec<_>>();
        if unique_physical_text_locus(&loci).is_none() {
            return Err(io::Error::other(
                "menu 935 did not expose one physical Bluetooth device-class locus",
            )
            .into());
        }
        let identities = candidates
            .iter()
            .map(|(_, identity, _)| identity.as_str())
            .collect::<BTreeSet<_>>();
        let formats = candidates
            .iter()
            .map(|(_, _, format)| *format)
            .collect::<BTreeSet<_>>();
        if identities.len() != 1 || formats.len() != 1 {
            return Err(io::Error::other(
                "menu 935 Bluetooth device-class observations did not resolve to one identity and format",
            )
            .into());
        }
        let identity = identities
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("menu 935 Bluetooth class identity was empty"))?
            .to_owned();
        let format = formats
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("menu 935 Bluetooth class format was empty"))?;
        Ok((identity, format))
    }

    fn looks_like_bluetooth_device_class(text: &str) -> bool {
        bluetooth_device_class_format(text).is_some()
    }

    fn bluetooth_device_class_format(text: &str) -> Option<&'static str> {
        if canonical_text(text) == "phone" {
            // Exact stock V1.03 Menu 935 rendering on retained TH-D75A
            // hardware. The page exposes the major class label, not the
            // underlying 24-bit Bluetooth Class-of-Device integer.
            return Some("stock-v1.03-major-class-label-phone");
        }
        let trimmed = text
            .trim()
            .strip_prefix("0x")
            .or_else(|| text.trim().strip_prefix("0X"))
            .unwrap_or_else(|| text.trim());
        (trimmed.len() == 6 && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some("six-hex-digits-optionally-0x-prefixed")
    }

    fn looks_like_date(text: &str) -> bool {
        for separator in ['/', '-'] {
            let parts = text.trim().split(separator).collect::<Vec<_>>();
            let [first, second, third] = parts.as_slice() else {
                continue;
            };
            if !parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            {
                continue;
            }
            let Ok(first_value) = first.parse::<u16>() else {
                continue;
            };
            let Ok(second_value) = second.parse::<u16>() else {
                continue;
            };
            let Ok(third_value) = third.parse::<u16>() else {
                continue;
            };
            let valid = if first.len() == 4 {
                (2024..=9999).contains(&first_value)
                    && (1..=12).contains(&second_value)
                    && (1..=31).contains(&third_value)
            } else if third.len() == 4 {
                (1..=12).contains(&first_value)
                    && (1..=31).contains(&second_value)
                    && (2024..=9999).contains(&third_value)
            } else {
                false
            };
            if valid {
                return true;
            }
        }
        false
    }

    fn looks_like_time(text: &str) -> bool {
        let parts = text.trim().split(':').collect::<Vec<_>>();
        if !(2..=3).contains(&parts.len())
            || !parts
                .iter()
                .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return false;
        }
        let Some(hour) = parts.first() else {
            return false;
        };
        let Some(minute) = parts.get(1) else {
            return false;
        };
        let Ok(hour) = hour.parse::<u8>() else {
            return false;
        };
        let Ok(minute) = minute.parse::<u8>() else {
            return false;
        };
        let second = parts
            .get(2)
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(0);
        hour <= 23 && minute <= 59 && second <= 59
    }

    fn looks_like_utc_offset(text: &str) -> bool {
        let upper = text.trim().to_ascii_uppercase();
        let Some((_, after_utc)) = upper.split_once("UTC") else {
            return false;
        };
        let offset = after_utc.trim();
        let Some(sign) = offset.as_bytes().first() else {
            return false;
        };
        if !matches!(*sign, b'+' | b'-') {
            return false;
        }
        let parts = offset[1..].split(':').collect::<Vec<_>>();
        let [hour, minute] = parts.as_slice() else {
            return false;
        };
        hour.len() == 2
            && minute.len() == 2
            && hour.bytes().all(|byte| byte.is_ascii_digit())
            && minute.bytes().all(|byte| byte.is_ascii_digit())
            && hour.parse::<u8>().is_ok_and(|value| value <= 15)
            && minute
                .parse::<u8>()
                .is_ok_and(|value| matches!(value, 0 | 15 | 30 | 45))
    }

    fn manifest_entry<'entry>(
        manifest: &'entry [MenuEntry],
        number: &str,
    ) -> AuditResult<&'entry MenuEntry> {
        manifest
            .iter()
            .find(|entry| entry.number == number)
            .ok_or_else(|| invalid_input(format!("reviewed manifest has no menu {number}")))
    }

    fn anchor_page_title(anchor: &MenuEntry) -> &str {
        if anchor.number == "840" {
            "microSD Card"
        } else {
            &anchor.label
        }
    }

    fn category_parts(path: &str) -> AuditResult<(&str, &str)> {
        path.split_once(" - ")
            .ok_or_else(|| invalid_input(format!("invalid reviewed category path {path:?}")))
    }

    fn reviewed_submenu_paths<'entry>(
        manifest: &'entry [MenuEntry],
        category: &str,
    ) -> AuditResult<Vec<(&'entry str, &'entry str)>> {
        let mut paths = Vec::new();
        for entry in manifest {
            let (entry_category, submenu) = category_parts(&entry.category_path)?;
            if entry_category != category {
                continue;
            }
            if paths
                .last()
                .is_none_or(|(path, _)| *path != entry.category_path)
            {
                if paths.iter().any(|(path, _)| *path == entry.category_path) {
                    return Err(invalid_input(format!(
                        "reviewed submenu {:?} is not contiguous",
                        entry.category_path
                    )));
                }
                paths.push((entry.category_path.as_str(), submenu));
            }
        }
        if paths.is_empty() {
            return Err(invalid_input(format!(
                "reviewed category {category:?} has no submenus"
            )));
        }
        Ok(paths)
    }

    fn reviewed_rows<'entry>(
        manifest: &'entry [MenuEntry],
        category_path: &str,
    ) -> Vec<&'entry MenuEntry> {
        manifest
            .iter()
            .filter(|entry| entry.category_path == category_path)
            .collect()
    }

    fn menu_710_is_exact_reviewed_singleton(manifest: &[MenuEntry], target: &MenuEntry) -> bool {
        let rows = reviewed_rows(manifest, "FM Broadcasting - Memory");
        target.number == "710"
            && target.label == "FM Radio List"
            && target.category_path == "FM Broadcasting - Memory"
            && target.class == AuditClass::RowOnly
            && matches!(
                row_only_policy(&target.number),
                Ok(RowOnlyPolicy::MultiRecordEditor)
            )
            && rows.len() == 1
            && rows.first().is_some_and(|only| {
                only.number == target.number
                    && only.label == target.label
                    && only.category_path == target.category_path
                    && only.class == target.class
            })
    }

    fn navigation_mismatch(menu_number: &str, expected: &str, selected: &[String]) -> AuditError {
        Box::new(io::Error::other(format!(
            "menu {menu_number} navigation expected exact selected label {expected:?}, got {selected:?}; refusing to compensate or continue"
        )))
    }

    fn digit_key(digit: u8) -> AuditResult<FrontPanelKey> {
        match digit {
            b'0' => Ok(FrontPanelKey::Mark0),
            b'1' => Ok(FrontPanelKey::Vfo1),
            b'2' => Ok(FrontPanelKey::Mr2),
            b'3' => Ok(FrontPanelKey::Call3),
            b'4' => Ok(FrontPanelKey::Msg4),
            b'5' => Ok(FrontPanelKey::List5),
            b'6' => Ok(FrontPanelKey::Beacon6),
            b'7' => Ok(FrontPanelKey::Reverse7),
            b'8' => Ok(FrontPanelKey::Tone8),
            b'9' => Ok(FrontPanelKey::Pf1_9),
            _ => Err(invalid_input(
                "direct menu access accepts decimal digits only",
            )),
        }
    }

    fn strip_bold(text: &str) -> Option<&str> {
        text.strip_prefix("**")?.strip_suffix("**")
    }

    fn is_menu_number(text: &str) -> bool {
        matches!(text.as_bytes(), [a, b, c] if a.is_ascii_digit() && b.is_ascii_digit() && is_menu_number_suffix(*c))
    }

    const fn is_menu_number_suffix(byte: u8) -> bool {
        byte.is_ascii_digit() || matches!(byte, b'A'..=b'F')
    }

    fn selected_matches_label(screen: &CapturedScreen, expected: &str) -> bool {
        selected_matches_label_for_menu(screen, None, expected)
    }

    fn selected_matches_label_for_menu(
        screen: &CapturedScreen,
        menu_number: Option<&str>,
        expected: &str,
    ) -> bool {
        let expected = canonical_text(expected);
        if expected.is_empty() {
            return false;
        }
        let bands = v103_selection_bands(&screen.frame);
        let [band] = bands.as_slice() else {
            return false;
        };
        let selected = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                band.contains_normalized_y(bounds.y() + bounds.height() / 2.0)
            })
            .collect::<Vec<_>>();
        let exact = selected
            .iter()
            .copied()
            .filter(|observation| {
                selected_label_matches_for_menu(observation, menu_number, &expected)
            })
            .collect::<Vec<_>>();
        let exact_locus = if exact.is_empty() {
            None
        } else {
            let Some(exact) = unique_physical_text_locus(&exact) else {
                return false;
            };
            Some(exact)
        };
        let Ok(lower_value_locus_row) = u16::try_from(band.top() + band.height() / 2) else {
            return false;
        };
        let lower_value_locus_px = f32::from(lower_value_locus_row);
        let upper_lane = selected
            .iter()
            .copied()
            .filter(|observation| {
                (observation.bounds().y() + observation.bounds().height() / 2.0) * SCREEN_HEIGHT_F32
                    < lower_value_locus_px
            })
            .collect::<Vec<_>>();
        let fragment_loci = (band.height() == 40)
            .then(|| exact_ordered_two_fragment_selected_label_loci(&upper_lane, &expected))
            .flatten();
        if exact_locus.is_none() && fragment_loci.is_none() {
            return false;
        }
        let label_center_px = exact_locus
            .into_iter()
            .chain(
                fragment_loci
                    .into_iter()
                    .flat_map(<[&TextObservation; 2]>::from),
            )
            .map(|observation| {
                (observation.bounds().y() + observation.bounds().height() / 2.0) * SCREEN_HEIGHT_F32
            })
            .fold(0.0_f32, f32::max);
        selected
            .iter()
            .filter(|observation| is_observed_menu_locator(observation.text()))
            .count()
            <= 1
            && selected.iter().all(|observation| {
                let canonical = canonical_selected_label(observation.text());
                let center_px = (observation.bounds().y() + observation.bounds().height() / 2.0)
                    * SCREEN_HEIGHT_F32;
                canonical.is_empty()
                    || selected_label_matches_for_menu(observation, menu_number, &expected)
                    || is_observed_menu_locator(observation.text())
                    || fragment_loci.is_some_and(|(left, right)| {
                        let left_canonical = canonical_selected_label(left.text());
                        let right_canonical = canonical_selected_label(right.text());
                        (selected_label_fragment_locus_matches(
                            &canonical,
                            &left_canonical,
                            &expected,
                        )
                            && bounds_substantially_overlap(
                                &observation.bounds(),
                                &left.bounds(),
                            ))
                            || (selected_label_fragment_locus_matches(
                                &canonical,
                                &right_canonical,
                                &expected,
                            )
                                && bounds_substantially_overlap(
                                    &observation.bounds(),
                                    &right.bounds(),
                                ))
                    })
                    // Stock V1.03 uses a 40-pixel highlighted row for entries that
                    // render their current value on a subordinate second line.
                    // That payload is evidence about the selected row, not a
                    // second competing label.
                    || (center_px >= lower_value_locus_px && center_px >= label_center_px + 4.0)
            })
    }

    fn exact_ordered_two_fragment_selected_label_loci<'observation>(
        observations: &[&'observation TextObservation],
        expected: &str,
    ) -> Option<(&'observation TextObservation, &'observation TextObservation)> {
        let mut matched = None;
        for (split_index, _) in expected.match_indices(' ') {
            let left_expected = &expected[..split_index];
            let right_expected = &expected[split_index + 1..];
            let left = observations
                .iter()
                .copied()
                .filter(|observation| {
                    selected_label_fragment_matches(
                        &canonical_selected_label(observation.text()),
                        left_expected,
                        expected,
                    )
                })
                .collect::<Vec<_>>();
            let right = observations
                .iter()
                .copied()
                .filter(|observation| {
                    selected_label_fragment_matches(
                        &canonical_selected_label(observation.text()),
                        right_expected,
                        expected,
                    )
                })
                .collect::<Vec<_>>();
            let (Some(left), Some(right)) = (
                unique_physical_text_locus(&left),
                unique_physical_text_locus(&right),
            ) else {
                continue;
            };
            let left_bounds = left.bounds();
            let right_bounds = right.bounds();
            let left_center_y = left_bounds.y() + left_bounds.height() / 2.0;
            let right_center_y = right_bounds.y() + right_bounds.height() / 2.0;
            let horizontal_gap = right_bounds.x() - (left_bounds.x() + left_bounds.width());
            let maximum_horizontal_gap_px = if expected == "usb audio out. lvl." {
                14.0
            } else {
                12.0
            };
            if right_bounds.x() <= left_bounds.x()
                || (left_center_y - right_center_y).abs() > 4.0 / SCREEN_HEIGHT_F32
                || !(-2.0 / SCREEN_WIDTH_F32..=maximum_horizontal_gap_px / SCREEN_WIDTH_F32)
                    .contains(&horizontal_gap)
            {
                continue;
            }
            if matched.replace((left, right)).is_some() {
                // More than one textual decomposition is ambiguous even if
                // every candidate happens to occupy the same highlighted row.
                return None;
            }
        }
        matched
    }

    fn selected_label_fragment_matches(
        observed: &str,
        expected_fragment: &str,
        complete_expected: &str,
    ) -> bool {
        observed == expected_fragment
            || matches!(
                (complete_expected, expected_fragment, observed),
                ("usb audio out. lvl.", "lvl.", "lvi.")
            )
    }

    fn selected_label_fragment_locus_matches(
        observed: &str,
        accepted: &str,
        complete_expected: &str,
    ) -> bool {
        observed == accepted
            || (complete_expected == "usb audio out. lvl."
                && matches!((observed, accepted), ("lvl.", "lvi.") | ("lvi.", "lvl.")))
            // In the retained stock-V1.03 Menu 701 row Vision merged the
            // subordinate value `3` into the left title fragment. This exact
            // form is accepted only as a co-located alternative after the
            // independent `Auto Mute RET.` + `Time` pair reconstructs the
            // complete reviewed label.
            || (complete_expected == "auto mute ret. time"
                && accepted == "auto mute ret."
                && observed == "auto mute ret-3")
    }

    fn selected_label_matches(observation: &TextObservation, expected: &str) -> bool {
        selected_label_matches_for_menu(observation, None, expected)
    }

    fn selected_label_matches_for_menu(
        observation: &TextObservation,
        menu_number: Option<&str>,
        expected: &str,
    ) -> bool {
        let observed = canonical_selected_label(observation.text());
        if observed == expected {
            return true;
        }
        if menu_number == Some("999") && expected == "reset" && observed == "řeset" {
            // Exact overlapping stock V1.03 Vision output retained from the
            // destructive Reset row. Keep the diacritic correction scoped to
            // Menu 999; the selected-row oracle still requires every accepted
            // recognition to occupy one physical locus.
            return true;
        }
        if menu_number == Some("631") && expected == "$gpvtg" && observed == "sgpytg" {
            // Exact overlapping Vision output retained from the selected
            // stock V1.03 Menu 631 `$GPVTG` row. The independent exact label,
            // selected-row geometry, and framebuffer checkbox pixels remain
            // required; `unique_physical_text_locus` rejects a second locus.
            return true;
        }
        if retained_ui_label_alias(&observed, expected) {
            return true;
        }
        if expected == "rx" && observed == "ry" {
            // Exact live V1.03 Vision output for the two-pixel-wide final X in
            // Menu 181. Row geometry and checkbox pixels remain independent.
            return true;
        }
        if matches!(
            (expected, observed.as_str()),
            ("wx alert", "*x alert")
                | ("cw width", "c# width" | "ch hidth")
                | ("delay", "de lay")
                | ("reverse", "řeverse")
                | ("icon", "rcon" | "tcon")
                | ("turn time", "lurn time")
                | ("mobile", "amobile")
                | ("object/item", "2object/item")
                | ("$gpgsa", "i $gpgsa")
                | ("digipeat(mycall)", "digipeat (mycali)")
                | ("uicheck", "ulcheck" | "u check")
        ) {
            // Exact retained V1.03 Vision forms. Keep these label-specific;
            // `unique_physical_text_locus` still requires every accepted
            // alternative to occupy one locus, while unrelated overlapping
            // text continues to fail closed.
            return true;
        }
        let mut characters = observed.chars();
        let Some(merged_checkbox_glyph) = characters.next() else {
            return false;
        };
        observation.bounds().x() < 0.15
            && expected.chars().count() >= 3
            && matches!(merged_checkbox_glyph, 'c' | 'j' | 'l' | 'm' | 'o')
            && characters.as_str() == expected
    }

    fn retained_ui_label_alias(observed: &str, expected: &str) -> bool {
        matches!(
            (expected, observed),
            ("uicheck", "uicheck" | "ulcheck" | "u check")
                | ("uidigipeat", "uidigipeat" | "uldigipeat")
                | ("uiflood", "uiflood" | "ulflood" | "vlflood")
                | ("uiflood alias", "uiflood alias" | "ulflood alias")
                | (
                    "uifloodsubstitution",
                    "uifloodsubstitution" | "ulfloodsubstitution" | "ulf loodsubstitution"
                )
                | ("uitrace", "uitrace" | "ultrace")
                | ("uitrace alias", "uitrace alias" | "ultrace alias")
                | ("uidigi aliases", "uidigi aliases" | "uldigi aliases")
                | ("$gpgll", "$gpgll" | "sgpigll")
                | ("tx/rx eq", "tx/rx eq" | "txyrx eq")
                | ("dv/dr", "dv/dr" | "dvydr" | "dv/dk")
        )
    }

    fn screen_matches_label(screen: &CapturedScreen, expected: &str) -> bool {
        let expected = canonical_text(expected);
        if expected.is_empty() {
            return false;
        }
        let title_observations = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                bounds.x() < 0.70 && bounds.y() + bounds.height() / 2.0 < 0.15
            })
            .collect::<Vec<_>>();
        if retained_ui_label_alias(&expected, &expected) {
            let retained_aliases = title_observations
                .iter()
                .copied()
                .filter(|observation| {
                    retained_ui_label_alias(&canonical_text(observation.text()), &expected)
                })
                .collect::<Vec<_>>();
            return retained_aliases.len() == title_observations.len()
                && unique_physical_text_locus(&retained_aliases).is_some();
        }
        let Some(exact) = title_observations
            .iter()
            .find(|observation| canonical_text(observation.text()) == expected)
        else {
            return exact_two_fragment_title_locus(&title_observations, &expected);
        };
        title_observations
            .iter()
            .all(|observation| bounds_substantially_overlap(&observation.bounds(), &exact.bounds()))
            || retained_full_title_and_fragment_pair(&title_observations, &expected, exact)
    }

    fn retained_full_title_and_fragment_pair(
        observations: &[&TextObservation],
        expected: &str,
        exact: &TextObservation,
    ) -> bool {
        // The retained stock V1.03 Menu 513 frame contains Vision's complete
        // `Prop. Pathing` observation plus its two source fragments. The
        // unusually tall left-fragment bounds overlap only 63% of the complete
        // locus, so the normal 70% duplicate-locus gate rejects it. Admit only
        // this exact title, exact ordered fragments, and no other title text.
        if expected != "prop. pathing" {
            return false;
        }
        let left = observations
            .iter()
            .copied()
            .filter(|observation| canonical_text(observation.text()) == "prop.")
            .collect::<Vec<_>>();
        let right = observations
            .iter()
            .copied()
            .filter(|observation| canonical_text(observation.text()) == "pathing")
            .collect::<Vec<_>>();
        let Some(left) = unique_physical_text_locus(&left) else {
            return false;
        };
        let Some(right) = unique_physical_text_locus(&right) else {
            return false;
        };
        let left_bounds = left.bounds();
        let right_bounds = right.bounds();
        let left_center_y = left_bounds.y() + left_bounds.height() / 2.0;
        let right_center_y = right_bounds.y() + right_bounds.height() / 2.0;
        let horizontal_gap = right_bounds.x() - (left_bounds.x() + left_bounds.width());
        canonical_text(&format!("{} {}", left.text(), right.text())) == expected
            && right_bounds.x() > left_bounds.x()
            && (left_center_y - right_center_y).abs() <= 4.0 / SCREEN_HEIGHT_F32
            && (-2.0 / SCREEN_WIDTH_F32..=12.0 / SCREEN_WIDTH_F32).contains(&horizontal_gap)
            && observations.iter().all(|observation| {
                let canonical = canonical_text(observation.text());
                match canonical.as_str() {
                    "prop." | "pathing" => true,
                    value if value == expected => {
                        bounds_substantially_overlap(&observation.bounds(), &exact.bounds())
                    }
                    _ => false,
                }
            })
    }

    fn exact_two_fragment_title_locus(observations: &[&TextObservation], expected: &str) -> bool {
        observations.iter().enumerate().any(|(left_index, left)| {
            observations
                .iter()
                .enumerate()
                .filter(|(right_index, _)| *right_index != left_index)
                .any(|(_, right)| {
                    let left_bounds = left.bounds();
                    let right_bounds = right.bounds();
                    let left_center_y = left_bounds.y() + left_bounds.height() / 2.0;
                    let right_center_y = right_bounds.y() + right_bounds.height() / 2.0;
                    let horizontal_gap = right_bounds.x() - (left_bounds.x() + left_bounds.width());
                    format!(
                        "{} {}",
                        canonical_title_fragment(left.text(), expected),
                        canonical_title_fragment(right.text(), expected)
                    ) == expected
                        && right_bounds.x() > left_bounds.x()
                        && (left_center_y - right_center_y).abs() <= 4.0 / SCREEN_HEIGHT_F32
                        && (-2.0 / SCREEN_WIDTH_F32..=12.0 / SCREEN_WIDTH_F32)
                            .contains(&horizontal_gap)
                        && observations.iter().all(|observation| {
                            bounds_substantially_overlap(&observation.bounds(), &left_bounds)
                                || bounds_substantially_overlap(
                                    &observation.bounds(),
                                    &right_bounds,
                                )
                        })
                })
        })
    }

    fn canonical_title_fragment(text: &str, expected: &str) -> String {
        let canonical = canonical_text(text);
        if expected == "apo: auto power off" && canonical == "apo=" {
            // Exact retained stock V1.03 Menu 921 title fragment. Vision reads
            // the tiny colon after `APO` as `=` while independently reading
            // the remaining `Auto Power Off` fragment exactly. Keep this
            // correction scoped to that one complete expected title.
            "apo:".to_owned()
        } else {
            canonical
        }
    }

    fn is_observed_menu_locator(text: &str) -> bool {
        is_menu_locator(&text.trim().to_ascii_uppercase())
    }

    fn screen_has_exact_menu_locator(screen: &CapturedScreen, expected: &str) -> bool {
        let locators = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                bounds.x() > 0.70 && bounds.y() + bounds.height() / 2.0 < 0.15
            })
            .filter(|observation| is_observed_menu_locator(observation.text()))
            .collect::<Vec<_>>();
        let Some(exact) = locators
            .iter()
            .find(|locator| locator.text().trim().eq_ignore_ascii_case(expected))
        else {
            return false;
        };
        locators
            .iter()
            .all(|locator| bounds_substantially_overlap(&locator.bounds(), &exact.bounds()))
    }

    fn numbered_row_matches(screen: &CapturedScreen, number: &str, label: &str) -> bool {
        selected_matches_label_for_menu(screen, Some(number), label)
            && screen_has_exact_menu_locator(screen, number)
    }

    fn menu_710_singleton_memory_submenu_matches(screen: &CapturedScreen) -> bool {
        let bands = v103_selection_bands(&screen.frame);
        let [band] = bands.as_slice() else {
            return false;
        };
        band.top() == 44
            && band.height() == 24
            && screen_matches_label(screen, "FM Broadcasting")
            && screen_has_exact_menu_locator(screen, "71-")
            && selected_matches_label_for_menu(screen, Some("710"), "Memory")
            && has_one_rendered_bottom_control(screen, "back")
            && has_one_rendered_bottom_left_control(screen, "back")
            && has_one_rendered_bottom_right_control(screen, "ok")
    }

    fn is_operating_screen(screen: &CapturedScreen, anchor: Option<&str>) -> bool {
        let frequency_count = screen
            .observations
            .iter()
            .filter(|observation| {
                observation.confidence() >= MIN_OCR_CONFIDENCE
                    && looks_like_frequency(observation.text())
            })
            .count();
        frequency_count >= 2
            && ["Menu", "Back", "OK"]
                .iter()
                .all(|text| !has_exact_text(&screen.observations, text))
            && anchor.is_none_or(|text| has_exact_text(&screen.observations, text))
    }

    fn is_reviewed_single_band_home(screen: &CapturedScreen) -> bool {
        let frequencies = home_frequency_anchors(screen);
        let frequency_center_px = frequencies
            .first()
            .map(|anchor| (anchor.bounds.y() + anchor.bounds.height() / 2.0) * SCREEN_HEIGHT_F32);
        frequencies.len() == 1
            && frequency_center_px.is_some_and(|center| (45.0..=90.0).contains(&center))
            && screen.selected.is_empty()
            && screen_has_known_analog_mode(screen)
            && observed_operation_band(screen).is_some()
            && !baseline_has_disallowed_home_layout(screen)
    }

    fn reviewed_single_band_home_matches(
        screen: &CapturedScreen,
        baseline: &CapturedScreen,
    ) -> bool {
        let frequencies = home_frequency_anchors(screen);
        let baseline_frequencies = home_frequency_anchors(baseline);
        is_reviewed_single_band_home(screen)
            && is_reviewed_single_band_home(baseline)
            && frequencies
                .first()
                .zip(baseline_frequencies.first())
                .is_some_and(|(actual, expected)| {
                    frequencies.len() == 1
                        && baseline_frequencies.len() == 1
                        && actual.canonical == expected.canonical
                })
            && observed_operation_band(screen) == observed_operation_band(baseline)
    }

    fn is_reviewed_single_band_b_home(
        screen: &CapturedScreen,
        dual_band_baseline: &CapturedScreen,
    ) -> bool {
        let baseline_frequencies = home_frequency_anchors(dual_band_baseline);
        let frequencies = home_frequency_anchors(screen);
        let band_b_frequency_matches = baseline_frequencies
            .get(1)
            .zip(frequencies.first())
            .is_some_and(|(expected, actual)| {
                baseline_frequencies.len() == 2
                    && frequencies.len() == 1
                    && expected.canonical == actual.canonical
            });
        band_b_frequency_matches
            && screen.selected.is_empty()
            && screen_has_known_analog_mode(screen)
            && !baseline_has_disallowed_home_layout(screen)
    }

    fn screen_has_known_analog_mode(screen: &CapturedScreen) -> bool {
        screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .map(|observation| canonical_value_text(observation.text()))
            .any(|canonical| {
                canonical
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .any(|token| {
                        matches!(token, "fm" | "nfm" | "am" | "wfm" | "usb" | "lsb" | "cw")
                    })
            })
    }

    fn observed_operation_band(screen: &CapturedScreen) -> Option<Band> {
        let bands = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| canonical_text(observation.text()).contains("ptt"))
            .filter_map(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                if (0.05..0.30).contains(&center_y) {
                    Some(Band::A)
                } else if (0.35..0.65).contains(&center_y) {
                    Some(Band::B)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let first = *bands.first()?;
        bands.iter().all(|band| *band == first).then_some(first)
    }

    fn compare_dual_band_home(
        screen: &CapturedScreen,
        baseline: &CapturedScreen,
    ) -> AuditResult<HomeComparison> {
        let baseline_frequency_anchors = home_frequency_anchors(baseline);
        let baseline_stable_anchors = home_stable_text_anchors(baseline);
        if !dual_band_home_semantic_profile_valid(
            baseline,
            &baseline_frequency_anchors,
            &baseline_stable_anchors,
        ) {
            return Err(io::Error::other(
                "qualified baseline is not the reviewed V1.03 dual-band operating-screen profile (exactly two frequencies and a known analog-mode anchor required)",
            )
            .into());
        }

        let frequency_anchors = home_frequency_anchors(screen);
        let stable_anchors = home_stable_text_anchors(screen);
        let semantic_profile_valid = dual_band_home_semantic_profile_valid(
            screen,
            &frequency_anchors,
            &stable_anchors,
        )
            && ordered_home_anchor_texts_match(&baseline_frequency_anchors, &frequency_anchors)
            // Exact masked pixels remain the authoritative stable-mode proof.
            // Vision must independently recognize a reviewed analog mode, but
            // an intermittent duplicate/icon OCR observation is not permitted
            // to override byte-identical stable framebuffer pixels.
            && home_anchors_have_known_analog_mode(&stable_anchors);
        let baseline_bytes = baseline.frame.rgb565_le();
        let candidate_bytes = screen.frame.rgb565_le();
        let full_differing_pixels = baseline_bytes
            .chunks_exact(2)
            .zip(candidate_bytes.chunks_exact(2))
            .filter(|(expected, actual)| expected != actual)
            .count();
        let baseline_masked = masked_home_bytes(&baseline.frame);
        let candidate_masked = masked_home_bytes(&screen.frame);
        let masked_differing_pixels = baseline_masked
            .chunks_exact(2)
            .zip(candidate_masked.chunks_exact(2))
            .filter(|(expected, actual)| expected != actual)
            .count();
        if baseline_masked.len() != HOME_MASK_INCLUDED_PIXELS * 2
            || candidate_masked.len() != HOME_MASK_INCLUDED_PIXELS * 2
        {
            return Err(
                io::Error::other("V1.03 home mask selected an unexpected pixel count").into(),
            );
        }
        Ok(HomeComparison {
            full_differing_pixels,
            masked_differing_pixels,
            baseline_masked_sha256: sha256_hex(&baseline_masked)?,
            candidate_masked_sha256: sha256_hex(&candidate_masked)?,
            frequency_anchors,
            stable_anchors,
            semantic_profile_valid,
        })
    }

    fn dual_band_home_semantic_profile_valid(
        screen: &CapturedScreen,
        frequency_anchors: &[HomeTextAnchor],
        stable_anchors: &[HomeTextAnchor],
    ) -> bool {
        let centers_px = frequency_anchors
            .iter()
            .map(|anchor| (anchor.bounds.y() + anchor.bounds.height() / 2.0) * SCREEN_HEIGHT_F32)
            .collect::<Vec<_>>();
        frequency_anchors.len() == 2
            && !stable_anchors.is_empty()
            && home_anchors_have_known_analog_mode(stable_anchors)
            && centers_px
                .first()
                .zip(centers_px.get(1))
                .is_some_and(|(first, second)| {
                    (45.0..=90.0).contains(first) && (115.0..=170.0).contains(second)
                })
            && !baseline_has_disallowed_home_layout(screen)
    }

    fn masked_home_bytes(frame: &ScreenFrame) -> Vec<u8> {
        let row_bytes = SCREEN_WIDTH * 2;
        let mut bytes = Vec::with_capacity(HOME_MASK_INCLUDED_PIXELS * 2);
        for (y, row) in frame.rgb565_le().chunks_exact(row_bytes).enumerate() {
            for (x, pixel) in row.chunks_exact(2).enumerate() {
                if !home_pixel_is_masked(x, y) {
                    bytes.extend_from_slice(pixel);
                }
            }
        }
        bytes
    }

    fn home_pixel_is_masked(x: usize, y: usize) -> bool {
        let full_width_volatile_row = HOME_MASK_EXCLUDED_ROWS
            .iter()
            .any(|(top, bottom)| (*top..*bottom).contains(&y));
        let (meter_x, meter_y, meter_width, meter_height) = HOME_MASK_SIGNAL_METER_RECT;
        let signal_meter = (meter_x..meter_x.saturating_add(meter_width)).contains(&x)
            && (meter_y..meter_y.saturating_add(meter_height)).contains(&y);
        full_width_volatile_row || signal_meter
    }

    fn home_frequency_anchors(screen: &CapturedScreen) -> Vec<HomeTextAnchor> {
        let mut anchors = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| looks_like_frequency(observation.text()))
            .map(|observation| HomeTextAnchor {
                canonical: canonical_value_text(observation.text()),
                bounds: observation.bounds(),
            })
            .collect::<Vec<_>>();
        sort_home_anchors(&mut anchors);
        anchors
    }

    fn home_stable_text_anchors(screen: &CapturedScreen) -> Vec<HomeTextAnchor> {
        let mut anchors = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let center_y =
                    (observation.bounds().y() + observation.bounds().height() / 2.0) * 180.0;
                (20.0..31.0).contains(&center_y) || (95.0..111.0).contains(&center_y)
            })
            .filter(|observation| !looks_like_frequency(observation.text()))
            .filter_map(|observation| {
                let canonical = canonical_value_text(observation.text());
                (!canonical.is_empty()).then(|| HomeTextAnchor {
                    canonical,
                    bounds: observation.bounds(),
                })
            })
            .collect::<Vec<_>>();
        sort_home_anchors(&mut anchors);
        anchors
    }

    fn sort_home_anchors(anchors: &mut [HomeTextAnchor]) {
        anchors.sort_by(|left, right| {
            let left_y = left.bounds.y() + left.bounds.height() / 2.0;
            let right_y = right.bounds.y() + right.bounds.height() / 2.0;
            left_y
                .total_cmp(&right_y)
                .then_with(|| left.bounds.x().total_cmp(&right.bounds.x()))
        });
    }

    fn baseline_has_disallowed_home_layout(screen: &CapturedScreen) -> bool {
        if !screen.selected.is_empty()
            || ["Menu", "Back", "OK"]
                .iter()
                .any(|text| has_exact_text(&screen.observations, text))
        {
            return true;
        }
        screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .map(|observation| canonical_value_text(observation.text()))
            .any(|canonical| contains_disallowed_home_token(&canonical))
    }

    fn home_anchors_have_known_analog_mode(anchors: &[HomeTextAnchor]) -> bool {
        anchors.iter().any(|anchor| {
            anchor
                .canonical
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|token| matches!(token, "fm" | "nfm" | "am" | "wfm" | "usb" | "lsb" | "cw"))
        })
    }

    fn contains_disallowed_home_token(canonical: &str) -> bool {
        canonical.contains("d-star")
            || canonical.contains("d star")
            || canonical
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|token| matches!(token, "dv" | "dr" | "dstar" | "gps" | "aprs" | "date"))
    }

    fn ordered_home_anchor_texts_match(
        expected: &[HomeTextAnchor],
        actual: &[HomeTextAnchor],
    ) -> bool {
        expected.len() == actual.len()
            && expected
                .iter()
                .zip(actual)
                .all(|(expected, actual)| expected.canonical == actual.canonical)
    }

    fn journal_home_comparison(
        journal: &mut Journal,
        phase: &str,
        menu_number: Option<&str>,
        screen: &CapturedScreen,
        baseline: &CapturedScreen,
        comparison: &HomeComparison,
    ) -> AuditResult<()> {
        journal.append(json!({
            "type": "dual-band-home-restoration-oracle",
            "phase": phase,
            "menu_number": menu_number,
            "mask_id": HOME_MASK_ID,
            "mask_excluded_rectangles": HOME_MASK_EXCLUDED_ROWS.iter().map(|(top, bottom)| json!({
                    "x": 0,
                    "y": top,
                    "width": SCREEN_WIDTH,
                    "height": bottom - top,
                })).chain(std::iter::once(json!({
                    "x": HOME_MASK_SIGNAL_METER_RECT.0,
                    "y": HOME_MASK_SIGNAL_METER_RECT.1,
                    "width": HOME_MASK_SIGNAL_METER_RECT.2,
                    "height": HOME_MASK_SIGNAL_METER_RECT.3,
                    "semantic": "live-RF-S-meter-fill",
                }))).collect::<Vec<_>>(),
            "included_pixels": HOME_MASK_INCLUDED_PIXELS,
            "excluded_pixels": HOME_MASK_EXCLUDED_PIXELS,
            "total_pixels": HOME_MASK_INCLUDED_PIXELS + HOME_MASK_EXCLUDED_PIXELS,
            "full_frame": {
                "baseline_crc32": format!("{:08X}", baseline.crc32),
                "candidate_crc32": format!("{:08X}", screen.crc32),
                "differing_pixels": comparison.full_differing_pixels,
                "used_for_verdict": false,
            },
            "masked_frame": {
                "baseline_sha256": comparison.baseline_masked_sha256,
                "candidate_sha256": comparison.candidate_masked_sha256,
                "differing_pixels": comparison.masked_differing_pixels,
                "used_for_verdict": true,
            },
            "ordered_frequency_anchors": comparison.frequency_anchors.iter().map(home_anchor_json).collect::<Vec<_>>(),
            "ordered_stable_mode_header_anchors": comparison.stable_anchors.iter().map(home_anchor_json).collect::<Vec<_>>(),
            "ordered_frequency_anchor_text_must_match": true,
            "ordered_stable_header_anchor_text_must_match": false,
            "candidate_known_analog_mode_anchor_required": true,
            "vision_anchor_bounds_used_for_verdict": false,
            "semantic_profile_valid": comparison.semantic_profile_valid,
            "result": if comparison.restored() { "pass" } else { "fail" },
        }))
    }

    fn home_anchor_json(anchor: &HomeTextAnchor) -> Value {
        json!({
            "canonical_text": anchor.canonical,
            "bounds": {
                "x": anchor.bounds.x(),
                "y": anchor.bounds.y(),
                "width": anchor.bounds.width(),
                "height": anchor.bounds.height(),
            },
        })
    }

    fn is_top_level_menu(screen: &CapturedScreen) -> bool {
        top_level_menu_matches(screen, 2)
    }

    fn is_restoration_top_level_menu(screen: &CapturedScreen) -> bool {
        top_level_menu_matches(screen, 2) && v103_selection_bands(&screen.frame).is_empty()
    }

    fn top_level_menu_matches(screen: &CapturedScreen, minimum_category_count: usize) -> bool {
        let reviewed_categories = TOP_MENU_CATEGORY_LABELS
            .iter()
            .map(|label| canonical_text(label))
            .collect::<BTreeSet<_>>();
        let observed_categories = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                (0.15..0.90).contains(&center_y)
            })
            .map(|observation| canonical_text(observation.text()))
            .filter(|text| reviewed_categories.contains(text))
            .collect::<BTreeSet<_>>();
        screen.selected.is_empty()
            && v103_selection_bands(&screen.frame).is_empty()
            && has_top_level_menu_title(screen)
            && observed_categories.len() >= minimum_category_count
            && has_one_rendered_bottom_control(screen, "ok")
            && !has_exact_text(&screen.observations, "Back")
            && !screen
                .observations
                .iter()
                .any(|observation| looks_like_frequency(observation.text()))
    }

    fn has_one_rendered_bottom_control(screen: &CapturedScreen, expected: &str) -> bool {
        let matching = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| observation.bounds().y() > 0.85)
            .filter(|observation| canonical_text(observation.text()) == expected)
            .collect::<Vec<_>>();
        let Some(first) = matching.first() else {
            return false;
        };
        matching
            .iter()
            .all(|observation| bounds_substantially_overlap(&observation.bounds(), &first.bounds()))
    }

    fn has_one_rendered_bottom_left_control(screen: &CapturedScreen, expected: &str) -> bool {
        let matching = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                bounds.x() < 0.35 && bounds.y() > 0.85
            })
            .filter(|observation| canonical_text(observation.text()) == expected)
            .collect::<Vec<_>>();
        let Some(first) = matching.first() else {
            return false;
        };
        matching
            .iter()
            .all(|observation| bounds_substantially_overlap(&observation.bounds(), &first.bounds()))
    }

    fn has_one_rendered_bottom_right_control(screen: &CapturedScreen, expected: &str) -> bool {
        let matching = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| observation.bounds().y() > 0.85)
            .filter(|observation| canonical_text(observation.text()) == expected)
            .collect::<Vec<_>>();
        let Some(first) = matching.first() else {
            return false;
        };
        first.bounds().x() > 0.65
            && matching.iter().all(|observation| {
                observation.bounds().x() > 0.65
                    && bounds_substantially_overlap(&observation.bounds(), &first.bounds())
            })
    }

    fn has_top_level_menu_title(screen: &CapturedScreen) -> bool {
        let title_observations = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                bounds.x() < 0.35 && bounds.y() + bounds.height() / 2.0 < 0.15
            })
            .collect::<Vec<_>>();
        let matching = title_observations
            .iter()
            .filter(|observation| canonical_text(observation.text()) == "menu")
            .copied()
            .collect::<Vec<_>>();
        let [menu_title] = matching.as_slice() else {
            return false;
        };
        title_observations.iter().all(|observation| {
            canonical_text(observation.text()) == "menu"
                || bounds_substantially_overlap(&observation.bounds(), &menu_title.bounds())
        })
    }

    fn bounds_substantially_overlap(left: &NormalizedBounds, right: &NormalizedBounds) -> bool {
        let overlap_width = ((left.x() + left.width()).min(right.x() + right.width())
            - left.x().max(right.x()))
        .max(0.0);
        let overlap_height = ((left.y() + left.height()).min(right.y() + right.height())
            - left.y().max(right.y()))
        .max(0.0);
        let smaller_area = (left.width() * left.height()).min(right.width() * right.height());
        smaller_area > 0.0 && overlap_width * overlap_height / smaller_area >= 0.70
    }

    fn require_top_level_menu(screen: &CapturedScreen) -> AuditResult<()> {
        if is_top_level_menu(screen) {
            Ok(())
        } else {
            Err(io::Error::other(
                "numeric-key gate did not prove the top-level Menu title, controls, empty selection-band layout, and at least two reviewed category identities",
            )
            .into())
        }
    }

    fn is_menu_locator(text: &str) -> bool {
        matches!(text.as_bytes(), [a, b, c] if a.is_ascii_digit() && b.is_ascii_digit() && (*c == b'-' || is_menu_number_suffix(*c)))
    }

    fn has_reviewed_safe_title(screen: &CapturedScreen) -> bool {
        parse_menu_manifest(REVIEWED_MANUAL).is_ok_and(|entries| {
            entries.iter().any(|entry| {
                if entry.class != AuditClass::RowOnly {
                    screen_matches_label(screen, anchor_page_title(entry))
                } else if matches!(
                    row_only_policy(&entry.number),
                    Ok(RowOnlyPolicy::SafeInspection)
                ) {
                    screen_matches_label(screen, safe_inspection_title(entry))
                } else {
                    false
                }
            })
        })
    }

    fn current_value_text(screen: &CapturedScreen) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let source = if screen.selected.is_empty() {
            screen
                .observations
                .iter()
                .filter(|observation| (0.15..0.85).contains(&observation.bounds().y()))
                .map(TextObservation::text)
                .collect::<Vec<_>>()
        } else {
            screen.selected.iter().map(String::as_str).collect()
        };
        source
            .into_iter()
            .filter_map(|text| {
                let canonical = canonical_text(text);
                (!canonical.is_empty() && seen.insert(canonical)).then(|| text.to_owned())
            })
            .collect()
    }

    fn firmware_version_payload(screen: &CapturedScreen) -> Option<Vec<String>> {
        let version_count = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| (0.15..0.85).contains(&observation.bounds().y()))
            .filter(|observation| {
                canonical_text(observation.text())
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .eq("v1.03.azm".chars())
            })
            .count();
        (version_count == 1).then(|| vec!["Firmware=V1.03.AZM".to_owned()])
    }

    fn ordinary_documented_payload(
        entry: &MenuEntry,
        screen: &CapturedScreen,
    ) -> Option<Vec<String>> {
        let bands = v103_selection_bands(&screen.frame);
        let [band] = bands.as_slice() else {
            return None;
        };
        let substantive = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                band.contains_normalized_y(center_y)
            })
            .filter(|observation| {
                let canonical = canonical_value_text(observation.text());
                !canonical.is_empty() && !is_documented_unit_only(entry, &canonical)
            })
            .collect::<Vec<_>>();
        documented_payload_from_observations(entry, &substantive)
    }

    fn centered_scalar_documented_payload(
        entry: &MenuEntry,
        screen: &CapturedScreen,
    ) -> Option<Vec<String>> {
        if !CENTERED_SCALAR_NUMBERS
            .split_ascii_whitespace()
            .any(|number| number == entry.number)
            || !v103_selection_bands(&screen.frame).is_empty()
            || !screen.selected.is_empty()
            || !screen_matches_label(screen, anchor_page_title(entry))
            || !has_one_rendered_bottom_control(screen, "back")
        {
            return None;
        }
        let substantive = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                (0.25..0.70).contains(&(bounds.y() + bounds.height() / 2.0))
            })
            .filter(|observation| {
                let canonical = canonical_value_text(observation.text());
                !canonical.is_empty() && !is_documented_unit_only(entry, &canonical)
            })
            .collect::<Vec<_>>();
        documented_payload_from_observations(entry, &substantive)
    }

    fn numbered_row_documented_payload(
        entry: &MenuEntry,
        screen: &CapturedScreen,
    ) -> Option<Vec<String>> {
        if !numbered_row_matches(screen, &entry.number, &entry.label) {
            return None;
        }
        let bands = v103_selection_bands(&screen.frame);
        let [band] = bands.as_slice() else {
            return None;
        };
        if band.height() != 40 {
            return None;
        }
        let lower_lane_start =
            f32::from(u16::try_from(band.top() + band.height() / 2).ok()?) / SCREEN_HEIGHT_F32;
        let substantive = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center_y = bounds.y() + bounds.height() / 2.0;
                band.contains_normalized_y(center_y) && center_y >= lower_lane_start
            })
            .filter(|observation| {
                let canonical = canonical_value_text(observation.text());
                !canonical.is_empty() && !is_documented_unit_only(entry, &canonical)
            })
            .collect::<Vec<_>>();
        documented_payload_from_observations(entry, &substantive)
    }

    fn documented_payload_from_observations(
        entry: &MenuEntry,
        substantive: &[&TextObservation],
    ) -> Option<Vec<String>> {
        let (exact, identity) = substantive.iter().find_map(|observation| {
            entry_value_identity(entry, observation.text()).map(|identity| (*observation, identity))
        })?;
        if !substantive.iter().all(|observation| {
            bounds_substantially_overlap(&observation.bounds(), &exact.bounds())
                && entry_value_identity(entry, observation.text())
                    .is_none_or(|observed| observed == identity)
        }) {
            return None;
        }
        Some(vec![
            format!("DocumentedDomain={identity}"),
            format!("Displayed={}", exact.text()),
        ])
    }

    fn entry_value_identity(entry: &MenuEntry, observed: &str) -> Option<String> {
        if matches!(entry.number.as_str(), "618" | "640") && canonical_value_text(observed) == "aii"
        {
            // Exact stock V1.03 Vision output for the selected `All` value on
            // these two pages. Keep the confusable page-scoped: `AII` remains
            // invalid for every other documented value domain.
            return value_domain(entry)?.identity("All");
        }
        if entry.number == "960"
            && matches!(
                canonical_value_text(observed).as_str(),
                "mkey lock" | "akey lock"
            )
        {
            // Exact overlapping stock V1.03 Vision forms for the selected
            // `Key Lock` value. Keep the merged checkbox-glyph correction
            // scoped to Menu 960's typed value domain.
            return value_domain(entry)?.identity("Key Lock");
        }
        if entry.number == "973"
            && matches!(
                canonical_value_text(observed).as_str(),
                "dd \"mm. mmi" | "dd °mm. mm'"
            )
        {
            // Exact Vision alternatives retained from stock V1.03 Menu 973.
            // Either can identify `dd°mm.mm'`; when both are present, the
            // payload validator still requires one physical locus. Keep the
            // quote/degree and final-i correction page-scoped.
            return value_domain(entry)?.identity("dd°mm.mm'");
        }
        value_domain(entry)?.identity(observed)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the reviewed page-specific typed domains remain auditable in one exact menu-number match"
    )]
    fn value_domain(entry: &MenuEntry) -> Option<ValueDomain> {
        if entry.class == AuditClass::RowOnly {
            return None;
        }
        if SPECIALIZED_PAYLOAD_NUMBERS
            .split_ascii_whitespace()
            .any(|number| number == entry.number)
            || entry.number == "991"
        {
            return Some(ValueDomain::Specialized);
        }
        let domain = match entry.number.as_str() {
            "101" => ValueDomain::ExactChoices(numbered_choices("type", 1, 8)),
            "132" | "133" | "701" => integer_domain(1, 10, None, None, &["sec"]),
            "136" => {
                ValueDomain::ExactChoices(canonical_choices(&["Off", "15 min", "30 min", "60 min"]))
            }
            "140" => ValueDomain::OffsetFrequency,
            "151" => integer_domain(0, 9, None, None, &[]),
            "170" => ValueDomain::DiscreteWithSuffix {
                choices: (400_u16..=1000)
                    .step_by(100)
                    .map(|value| value.to_string())
                    .collect(),
                suffixes: &["hz"],
            },
            "402" => ValueDomain::ExactChoices(
                std::iter::once("off".to_owned())
                    .chain(
                        numbered_choices("", 1, 4)
                            .into_iter()
                            .map(|value| format!("{value}-digit")),
                    )
                    .collect(),
            ),
            // Stock V1.03 renders the selected duration as separate `8` and
            // `min` OCR loci inside one authenticated selection band.
            "404" => ValueDomain::DiscreteWithSuffix {
                choices: canonical_choices(&["Off", "1", "2", "4", "8", "Auto"]),
                suffixes: &["min"],
            },
            "413" => integer_domain(2, 1800, None, None, &["sec"]),
            "414" => ValueDomain::Hundredths {
                minimum: 1,
                maximum: 999,
                suffixes: &["mile", "km", "nm"],
            },
            "501" => ValueDomain::ExactChoices(canonical_choices(&APRS_ICON_NAMES)),
            "502" => ValueDomain::ExactChoices(canonical_choices(&POSITION_COMMENTS)),
            "523" | "550" => ValueDomain::DistanceLimit,
            "531" => integer_domain(1, 100, None, None, &["min"]),
            "532" => integer_domain(10, 180, None, None, &["sec"]),
            "533" => integer_domain(5, 90, None, None, &["deg"]),
            "534" => integer_domain(1, 255, None, None, &["(10deg/speed)", "10deg/speed"]),
            "535" => integer_domain(5, 180, None, None, &["sec"]),
            "581" => integer_domain(1, 250, None, None, &["sec"]),
            "593" => ValueDomain::DiscreteWithSuffix {
                choices: CTCSS_FREQUENCIES
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                suffixes: &["hz"],
            },
            "611" => ValueDomain::IndexedOpaqueChoices {
                minimum: 1,
                maximum: 5,
            },
            "615" => integer_domain(1, 50, None, Some("level"), &[]),
            "621" => integer_domain(0, 99, Some(2), None, &[]),
            "901" => integer_domain(3, 60, None, None, &["sec"]),
            "915" | "917" => ValueDomain::ExactChoices(
                ["volume link", "vol link", "vol"]
                    .into_iter()
                    .map(str::to_owned)
                    .chain(numbered_choices("level", 1, 7))
                    .collect(),
            ),
            "918" => ValueDomain::ExactChoices(numbered_choices("speed", 1, 4)),
            "91A" => ValueDomain::ExactChoices(numbered_choices("level", 1, 7)),
            "940" | "941" => ValueDomain::FrontAssignment,
            "942" | "943" | "944" => ValueDomain::MicrophoneAssignment,
            "970" => ValueDomain::ExactChoices(canonical_choices(&[
                "mi/h, mile",
                "km/h, km",
                "knots, nm",
            ])),
            "980" => {
                ValueDomain::ExactChoices(canonical_choices(&["COM+AF/IF Output", "Mass Storage"]))
            }
            _ => {
                let choices = documented_choice_candidates(&entry.setting_values)
                    .into_iter()
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    return None;
                }
                ValueDomain::DocumentedChoices {
                    choices,
                    units: documented_units(&entry.setting_values)
                        .into_iter()
                        .collect(),
                }
            }
        };
        Some(domain)
    }

    const fn integer_domain(
        minimum: u16,
        maximum: u16,
        width: Option<usize>,
        prefix: Option<&'static str>,
        suffixes: &'static [&'static str],
    ) -> ValueDomain {
        ValueDomain::Integer {
            minimum,
            maximum,
            width,
            prefix,
            suffixes,
        }
    }

    fn numbered_choices(prefix: &str, minimum: u16, maximum: u16) -> Vec<String> {
        (minimum..=maximum)
            .map(|value| {
                if prefix.is_empty() {
                    value.to_string()
                } else {
                    format!("{prefix} {value}")
                }
            })
            .collect()
    }

    fn canonical_choices(choices: &[&str]) -> Vec<String> {
        choices
            .iter()
            .map(|choice| canonical_value_text(choice))
            .collect()
    }

    impl ValueDomain {
        fn identity(&self, observed: &str) -> Option<String> {
            let canonical = canonical_value_text(observed);
            match self {
                Self::ExactChoices(choices) => choices
                    .iter()
                    .find(|choice| choice.as_str() == canonical)
                    .map(|choice| format!("choice:{choice}")),
                Self::DocumentedChoices { choices, units } => {
                    let mut variants = vec![canonical.clone()];
                    variants.extend(units.iter().filter_map(|unit| {
                        canonical
                            .strip_suffix(unit)
                            .map(str::trim_end)
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned)
                    }));
                    variants.into_iter().find_map(|variant| {
                        choices
                            .iter()
                            .find(|choice| choice.as_str() == variant)
                            .map(|choice| format!("choice:{choice}"))
                    })
                }
                Self::DiscreteWithSuffix { choices, suffixes } => {
                    let value = strip_optional_suffix(&canonical, suffixes);
                    choices
                        .iter()
                        .find(|choice| choice.as_str() == value)
                        .map(|choice| format!("discrete:{choice}"))
                }
                Self::Integer {
                    minimum,
                    maximum,
                    width,
                    prefix,
                    suffixes,
                } => integer_value_identity(
                    &canonical, *minimum, *maximum, *width, *prefix, suffixes,
                ),
                Self::IndexedOpaqueChoices { minimum, maximum } => {
                    indexed_opaque_choice_identity(&canonical, *minimum, *maximum)
                }
                Self::Hundredths {
                    minimum,
                    maximum,
                    suffixes,
                } => hundredths_identity(&canonical, *minimum, *maximum, suffixes),
                Self::OffsetFrequency => offset_frequency_identity(&canonical),
                Self::DistanceLimit => distance_limit_identity(&canonical),
                Self::FrontAssignment => assignment_identity(&canonical, FRONT_PF_ASSIGNMENTS),
                Self::MicrophoneAssignment => assignment_identity(&canonical, MIC_PF_ASSIGNMENTS),
                Self::Specialized => None,
            }
        }
    }

    fn indexed_opaque_choice_identity(observed: &str, minimum: u8, maximum: u8) -> Option<String> {
        if observed == "off" {
            return Some("choice:off".to_owned());
        }
        let (index, opaque) = observed
            .split_once(':')
            .map_or((observed, None), |(index, opaque)| (index, Some(opaque)));
        if index.len() != 1
            || !index.bytes().all(|byte| byte.is_ascii_digit())
            || opaque.is_some_and(|value| value.trim().is_empty())
        {
            return None;
        }
        let index = index.parse::<u8>().ok()?;
        (minimum..=maximum)
            .contains(&index)
            .then(|| format!("choice:{index}"))
    }

    fn strip_optional_suffix<'text>(text: &'text str, suffixes: &[&str]) -> &'text str {
        suffixes
            .iter()
            .find_map(|suffix| text.strip_suffix(suffix).map(str::trim_end))
            .unwrap_or(text)
    }

    fn integer_value_identity(
        observed: &str,
        minimum: u16,
        maximum: u16,
        width: Option<usize>,
        prefix: Option<&str>,
        suffixes: &[&str],
    ) -> Option<String> {
        let without_suffix = strip_optional_suffix(observed, suffixes);
        let raw = if let Some(prefix) = prefix {
            without_suffix
                .strip_prefix(prefix)?
                .trim_start_matches([' ', ':'])
        } else {
            without_suffix
        };
        if raw.is_empty()
            || !raw.bytes().all(|byte| byte.is_ascii_digit())
            || width.is_some_and(|required| raw.len() != required)
        {
            return None;
        }
        let value = raw.parse::<u16>().ok()?;
        let rendered = width.map_or_else(|| value.to_string(), |width| format!("{value:0width$}"));
        if raw != rendered {
            return None;
        }
        (minimum..=maximum)
            .contains(&value)
            .then(|| format!("integer:{value}"))
    }

    fn hundredths_identity(
        observed: &str,
        minimum: u16,
        maximum: u16,
        suffixes: &[&str],
    ) -> Option<String> {
        let value = strip_optional_suffix(observed, suffixes);
        let (whole, fraction) = value.split_once('.')?;
        if whole.len() != 1
            || fraction.len() != 2
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let scaled = whole
            .parse::<u16>()
            .ok()?
            .checked_mul(100)?
            .checked_add(fraction.parse::<u16>().ok()?)?;
        (minimum..=maximum)
            .contains(&scaled)
            .then(|| format!("hundredths:{scaled:03}"))
    }

    fn offset_frequency_identity(observed: &str) -> Option<String> {
        let value = strip_optional_suffix(observed, &["mhz"]);
        let scaled_hundredths = parse_decimal_hundredths(value)?;
        (scaled_hundredths <= 2995 && scaled_hundredths % 5 == 0)
            .then(|| format!("offset-frequency:{scaled_hundredths}-hundredths-MHz"))
    }

    fn parse_decimal_hundredths(value: &str) -> Option<u16> {
        let (whole, fraction) = value.split_once('.')?;
        if whole.is_empty()
            || !(1..=3).contains(&fraction.len())
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let whole = whole.parse::<u16>().ok()?;
        let fractional = fraction.parse::<u16>().ok()?;
        let hundredths = match fraction.len() {
            1 => fractional.checked_mul(10)?,
            2 => fractional,
            3 if fractional % 10 == 0 => fractional / 10,
            _ => return None,
        };
        whole.checked_mul(100)?.checked_add(hundredths)
    }

    fn distance_limit_identity(observed: &str) -> Option<String> {
        if observed == "off" {
            return Some("choice:off".to_owned());
        }
        let value = ["mile", "km", "nm"]
            .iter()
            .find_map(|unit| observed.strip_suffix(*unit).map(str::trim_end))
            .unwrap_or(observed);
        let distance = value.parse::<u16>().ok()?;
        ((10..=2500).contains(&distance) && distance % 10 == 0)
            .then(|| format!("distance-limit:{distance}"))
    }

    fn assignment_identity(observed: &str, assignments: &[&str]) -> Option<String> {
        let normalized = match observed {
            "balance (pf1)" => "balance",
            "gps (pf2)" => "gps",
            "a/b (pf1 mic)" => "a/b",
            "vfo (pf2 mic)" => "vfo",
            "mr (pf3 mic)" => "mr",
            other => other,
        };
        assignments
            .iter()
            .map(|assignment| canonical_value_text(assignment))
            .find(|assignment| assignment.as_str() == normalized)
            .map(|assignment| format!("assignment:{assignment}"))
    }

    fn is_documented_unit_only(entry: &MenuEntry, observed: &str) -> bool {
        (entry.number == "140" && observed == "mhz")
            || (entry.number == "404" && observed == "min")
            || documented_units(&entry.setting_values).contains(observed)
    }

    fn documented_choice_candidates(documentation: &str) -> BTreeSet<String> {
        split_documented_alternatives(documentation)
            .into_iter()
            .filter_map(|alternative| {
                let without_unit = alternative
                    .split_once('[')
                    .map_or(alternative.as_str(), |(value, _)| value)
                    .trim();
                if without_unit.is_empty()
                    || without_unit == "-"
                    || without_unit.contains(" - ")
                    || without_unit.contains("...")
                    || without_unit.contains(" ~ ")
                {
                    return None;
                }
                let without_region_note = if let Some((value, note)) =
                    without_unit.rsplit_once(" (")
                    && (note.to_ascii_lowercase().contains("th-d75")
                        || matches!(canonical_value_text(value).as_str(), "off" | "on"))
                {
                    value
                } else {
                    without_unit
                };
                let candidate = canonical_value_text(without_region_note);
                (!candidate.is_empty()).then_some(candidate)
            })
            .collect()
    }

    fn split_documented_alternatives(documentation: &str) -> Vec<String> {
        let cleaned = documentation.replace("**", "");
        if cleaned.contains("A:") && cleaned.contains("B:") {
            return cleaned
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect();
        }
        let mut alternatives = Vec::new();
        let mut start = 0;
        let mut square_depth = 0_u8;
        let mut parenthesis_depth = 0_u8;
        for (index, character) in cleaned.char_indices() {
            match character {
                '[' => square_depth = square_depth.saturating_add(1),
                ']' => square_depth = square_depth.saturating_sub(1),
                '(' => parenthesis_depth = parenthesis_depth.saturating_add(1),
                ')' => parenthesis_depth = parenthesis_depth.saturating_sub(1),
                '/' if square_depth == 0
                    && parenthesis_depth == 0
                    && !slash_is_embedded_compound(&cleaned, index) =>
                {
                    alternatives.push(cleaned[start..index].trim().to_owned());
                    start = index + character.len_utf8();
                }
                _ => {}
            }
        }
        alternatives.push(cleaned[start..].trim().to_owned());
        alternatives
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect()
    }

    fn slash_is_embedded_compound(text: &str, slash_index: usize) -> bool {
        let before = text[..slash_index].to_ascii_lowercase();
        let after = text[slash_index + 1..].to_ascii_lowercase();
        [
            ("af", "if"),
            ("mi", "h"),
            ("km", "h"),
            ("a", "b"),
            ("dv", "dr"),
            ("tx", "rx"),
        ]
        .iter()
        .any(|(left, right)| before.ends_with(*left) && after.starts_with(*right))
    }

    fn documented_units(documentation: &str) -> BTreeSet<String> {
        let mut units = BTreeSet::new();
        let mut remaining = documentation;
        while let Some((_, after_open)) = remaining.split_once('[') {
            let Some((inside, after_close)) = after_open.split_once(']') else {
                break;
            };
            units.extend(
                inside
                    .split(',')
                    .map(canonical_value_text)
                    .filter(|unit| !unit.is_empty()),
            );
            remaining = after_close;
        }
        let canonical = canonical_value_text(&documentation.replace("**", ""));
        for unit in ["hz", "khz", "ms", "sec", "min", "db", "deg"] {
            if canonical.ends_with(&format!(" {unit}")) {
                units.extend([unit.to_owned()]);
            }
        }
        units
    }

    fn canonical_value_text(text: &str) -> String {
        let canonical = canonical_selected_label(text)
            .replace(": ", ":")
            .replace(" :", ":")
            .replace("/ ", "/")
            .replace(" /", "/");
        let characters = canonical.chars().collect::<Vec<_>>();
        characters
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, character)| {
                let prior_is_period = index
                    .checked_sub(1)
                    .and_then(|prior| characters.get(prior))
                    .copied()
                    == Some('.');
                let before_prior_is_digit = index
                    .checked_sub(2)
                    .and_then(|prior| characters.get(prior))
                    .is_some_and(char::is_ascii_digit);
                let split_decimal_space = character == ' '
                    && prior_is_period
                    && before_prior_is_digit
                    && characters.get(index + 1).is_some_and(char::is_ascii_digit);
                (!split_decimal_space).then_some(character)
            })
            .collect()
    }

    async fn audit_scrollable_checkbox_payload(
        session: &mut AutomationSession<'_, EitherTransport>,
        output_dir: &Path,
        journal: &mut Journal,
        entry: &MenuEntry,
        initial: &CapturedScreen,
    ) -> AuditResult<Option<Vec<String>>> {
        let Some(labels) = scrollable_checkbox_labels(&entry.number) else {
            return Ok(None);
        };
        journal.append(json!({
            "type": "scrollable-checkbox-audit-start",
            "menu_number": entry.number,
            "logical_rows": labels,
            "navigation_key": "Down",
            "value_toggle_or_confirmation_keys_dispatched": false,
            "policy": "exact-selected-row-and-framebuffer-checkbox-state",
        }))?;

        let mut captured = None;
        let mut payload = Vec::with_capacity(labels.len());
        for (logical_index, expected_label) in labels.iter().copied().enumerate() {
            if logical_index > 0 {
                let _receipt = tap(
                    session,
                    journal,
                    FrontPanelKey::Down,
                    "select-next-read-only-checkbox-row",
                )
                .await?;
                tokio::time::sleep(SETTLE_DELAY).await;
                captured = Some(
                    capture_quiescent(
                        session,
                        output_dir,
                        journal,
                        &format!("{}-checkbox-row-{}", entry.number, logical_index + 1),
                        &entry.number,
                    )
                    .await?,
                );
            }
            let screen = captured.as_ref().unwrap_or(initial);
            let title_matches = screen_matches_label(screen, &entry.label);
            let selected_matches = selected_matches_label_for_menu(
                screen,
                Some(entry.number.as_str()),
                expected_label,
            );
            let selected_checkbox = v103_selected_checkbox(&screen.frame);
            let actual_slot = selected_checkbox.map(|(slot, _)| slot);
            let checkbox_state = selected_checkbox.map(|(_, state)| state);
            let expected_slot = logical_index.min(5);
            let slot_matches = actual_slot == Some(expected_slot);
            let state_text = checkbox_state.map(|state| match state {
                CheckboxState::Checked => "checked",
                CheckboxState::Unchecked => "unchecked",
            });
            let row_matches =
                title_matches && selected_matches && slot_matches && state_text.is_some();
            journal.append(json!({
                "type": "scrollable-checkbox-row-observation",
                "menu_number": entry.number,
                "logical_index": logical_index,
                "expected_label": expected_label,
                "selected_text": screen.selected,
                "expected_visible_slot": expected_slot,
                "actual_visible_slot": actual_slot,
                "checkbox_state": state_text,
                "crc32": format!("{:08X}", screen.crc32),
                "title_result": if title_matches { "pass" } else { "inconclusive" },
                "selection_result": if selected_matches { "pass" } else { "inconclusive" },
                "slot_result": if slot_matches { "pass" } else { "inconclusive" },
                "value_toggle_or_confirmation_keys_dispatched": false,
                "result": if row_matches { "pass" } else { "inconclusive" },
            }))?;
            let Some(state_text) = state_text.filter(|_| row_matches) else {
                journal.append(json!({
                    "type": "scrollable-checkbox-audit-end",
                    "menu_number": entry.number,
                    "rows_observed": logical_index + 1,
                    "value_toggle_or_confirmation_keys_dispatched": false,
                    "result": "inconclusive",
                }))?;
                return Ok(None);
            };
            payload.push(format!("{expected_label}={state_text}"));
        }
        journal.append(json!({
            "type": "scrollable-checkbox-audit-end",
            "menu_number": entry.number,
            "rows_observed": labels.len(),
            "value_toggle_or_confirmation_keys_dispatched": false,
            "result": "pass",
        }))?;
        Ok(Some(payload))
    }

    fn scrollable_checkbox_labels(menu_number: &str) -> Option<&'static [&'static str]> {
        match menu_number {
            "551" => Some(&FILTER_TYPE_ROWS),
            "631" => Some(&DV_GPS_SENTENCE_ROWS),
            _ => None,
        }
    }

    fn specialized_payload(menu_number: &str, screen: &CapturedScreen) -> Option<Vec<String>> {
        match menu_number {
            "181" => checkbox_payload(screen, &["RX", "FM Radio"]),
            "406" => checkbox_payload(
                screen,
                &["$GPGGA", "$GPGLL", "$GPGSA", "$GPGSV", "$GPRMC", "$GPVTG"],
            ),
            "509" => checkbox_payload(screen, &["Frequency", "PTT", "APRS Key"]),
            "530" => speed_payload(screen),
            "591" => network_payload(screen),
            "840" => memory_size_payload(screen),
            "912" => eq_payload(screen, &["0.4", "0.8", "1.6", "3.2"], -9..=3),
            "913" => eq_payload(screen, &["0.4", "0.8", "1.6", "3.2", "6.4"], -9..=9),
            "922" => battery_level_payload(screen),
            // Menus 551/631 are handled asynchronously above because their
            // seventh rows require one safe viewport scroll. Unknown pages
            // fail closed here.
            _ => None,
        }
    }

    fn checkbox_payload(screen: &CapturedScreen, labels: &[&str]) -> Option<Vec<String>> {
        labels
            .iter()
            .enumerate()
            .map(|(slot, label)| {
                if !checkbox_row_has_unique_label(screen, slot, label) {
                    return None;
                }
                let state = match v103_checkbox_state(&screen.frame, slot)? {
                    CheckboxState::Checked => "checked",
                    CheckboxState::Unchecked => "unchecked",
                };
                Some(format!("{label}={state}"))
            })
            .collect()
    }

    fn checkbox_row_has_unique_label(screen: &CapturedScreen, slot: usize, expected: &str) -> bool {
        let Ok(slot_row) = u16::try_from(slot) else {
            return false;
        };
        let expected_center = 24.0_f32.mul_add(f32::from(slot_row), 32.0) / 180.0;
        let expected = canonical_text(expected);
        let labels = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                let center = bounds.y() + bounds.height() / 2.0;
                (center - expected_center).abs() <= 10.0 / 180.0
            })
            .filter(|observation| checkbox_row_label_matches(observation, slot, &expected))
            .collect::<Vec<_>>();
        unique_physical_text_locus(&labels).is_some()
    }

    fn checkbox_row_label_matches(
        observation: &TextObservation,
        slot: usize,
        expected: &str,
    ) -> bool {
        selected_label_matches(observation, expected)
            || (slot == 1
                && expected == "tx eq(fm, nfm)"
                && canonical_selected_label(observation.text()) == "dtx eq (fm, nfm)")
    }

    fn eq_payload(
        screen: &CapturedScreen,
        frequencies: &[&str],
        allowed_levels: std::ops::RangeInclusive<i8>,
    ) -> Option<Vec<String>> {
        let frequency_rows = screen
            .observations
            .iter()
            .filter(|observation| {
                observation.confidence() >= MIN_OCR_CONFIDENCE && observation.bounds().x() < 0.50
            })
            .filter_map(|observation| {
                let frequency = parse_eq_frequency(observation.text())?;
                let slots = (0..frequencies.len())
                    .filter(|slot| observation_is_in_eq_row(observation, *slot))
                    .collect::<Vec<_>>();
                let [slot] = slots.as_slice() else {
                    return Some((usize::MAX, frequency));
                };
                Some((*slot, frequency))
            })
            .collect::<BTreeSet<_>>();
        let expected_frequency_rows = frequencies
            .iter()
            .copied()
            .enumerate()
            .collect::<BTreeSet<_>>();
        if frequency_rows != expected_frequency_rows {
            return None;
        }
        frequencies
            .iter()
            .copied()
            .enumerate()
            .map(|(slot, expected_frequency)| {
                let levels = screen
                    .observations
                    .iter()
                    .filter(|observation| {
                        observation.confidence() >= MIN_OCR_CONFIDENCE
                            && observation.bounds().x() > 0.75
                            && observation_is_in_eq_row(observation, slot)
                    })
                    .filter_map(|observation| parse_eq_level(observation.text()))
                    .collect::<BTreeSet<_>>();
                let level = levels.iter().copied().next()?;
                if levels.len() != 1 || !allowed_levels.contains(&level) {
                    return None;
                }
                let level_text = match level.cmp(&0) {
                    std::cmp::Ordering::Equal => "±0".to_owned(),
                    std::cmp::Ordering::Greater => format!("+{level}"),
                    std::cmp::Ordering::Less => level.to_string(),
                };
                Some(format!("{expected_frequency} kHz={level_text} dB"))
            })
            .collect()
    }

    fn observation_is_in_eq_row(observation: &TextObservation, slot: usize) -> bool {
        let Ok(slot) = u16::try_from(slot) else {
            return false;
        };
        let expected_center = 24.0_f32.mul_add(f32::from(slot), 32.0) / 180.0;
        let bounds = observation.bounds();
        let observed_center = bounds.y() + bounds.height() / 2.0;
        (observed_center - expected_center).abs() <= 10.0 / 180.0
    }

    fn parse_eq_frequency(text: &str) -> Option<&'static str> {
        let compact = text
            .chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        match compact.as_str() {
            "0.4khz" => Some("0.4"),
            "0.8khz" => Some("0.8"),
            "1.6khz" => Some("1.6"),
            "3.2khz" => Some("3.2"),
            "6.4khz" => Some("6.4"),
            _ => None,
        }
    }

    fn parse_eq_level(text: &str) -> Option<i8> {
        let compact = text
            .chars()
            .filter(|character| !character.is_whitespace())
            .map(|character| match character {
                'O' | 'o' | 'Ø' | 'ø' => '0',
                other => other,
            })
            .collect::<String>();
        let mut characters = compact.chars();
        let sign = characters.next()?;
        let digits = characters.collect::<String>();
        if digits.len() != 1 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let magnitude = digits.parse::<i8>().ok()?;
        match sign {
            '-' => Some(-magnitude),
            '+' => Some(magnitude),
            '±' if magnitude == 0 => Some(0),
            _ => None,
        }
    }

    fn speed_payload(screen: &CapturedScreen) -> Option<Vec<String>> {
        let values = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| (0.15..0.85).contains(&observation.bounds().y()))
            .filter_map(|observation| parse_speed(observation.text()))
            .collect::<BTreeSet<_>>();
        if values.len() != 1 {
            return None;
        }
        let (low, high, unit) = values.into_iter().next()?;
        (low <= high).then(|| {
            vec![
                format!("Low Speed={low}"),
                format!("High Speed={high}"),
                format!("Unit={unit}"),
            ]
        })
    }

    fn parse_speed(text: &str) -> Option<(u8, u8, &'static str)> {
        let fields = text.split_ascii_whitespace().collect::<Vec<_>>();
        let [low, "-", high, unit] = fields.as_slice() else {
            return None;
        };
        let low = low.parse::<u8>().ok()?;
        let high = high.parse::<u8>().ok()?;
        let unit = match *unit {
            "mile/h" => "mile/h",
            "km/h" => "km/h",
            "knots" => "knots",
            _ => return None,
        };
        ((2..=30).contains(&low) && (2..=90).contains(&high)).then_some((low, high, unit))
    }

    fn network_payload(screen: &CapturedScreen) -> Option<Vec<String>> {
        let bands = v103_selection_bands(&screen.frame);
        let [band] = bands.as_slice() else {
            return None;
        };
        let selected = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .filter(|observation| {
                let bounds = observation.bounds();
                band.contains_normalized_y(bounds.y() + bounds.height() / 2.0)
            })
            .collect::<Vec<_>>();
        let locus = |expected: &str| {
            let matches = selected
                .iter()
                .copied()
                .filter(|observation| canonical_value_text(observation.text()) == expected)
                .collect::<Vec<_>>();
            unique_physical_text_locus(&matches)
        };
        if selected.iter().any(|observation| {
            !matches!(
                canonical_value_text(observation.text()).as_str(),
                "use" | "aprs" | "[apk005]" | "altnet"
            )
        }) || locus("use").is_none()
        {
            return None;
        }
        if locus("aprs").is_some() && locus("[apk005]").is_some() && locus("altnet").is_none() {
            Some(vec!["Network=APRS [APK005]".to_owned()])
        } else if locus("altnet").is_some()
            && locus("aprs").is_none()
            && locus("[apk005]").is_none()
        {
            Some(vec!["Network=Altnet".to_owned()])
        } else {
            None
        }
    }

    fn memory_size_payload(screen: &CapturedScreen) -> Option<Vec<String>> {
        let observations = screen
            .observations
            .iter()
            .filter(|observation| observation.confidence() >= MIN_OCR_CONFIDENCE)
            .collect::<Vec<_>>();
        if observations
            .iter()
            .filter(|observation| {
                canonical_text(observation.text()) == "free"
                    && (0.15..0.50).contains(&observation.bounds().y())
            })
            .count()
            != 1
        {
            return None;
        }
        let free_values = observations
            .iter()
            .filter(|observation| (0.15..0.50).contains(&observation.bounds().y()))
            .filter_map(|observation| parse_gigabytes(observation.text()))
            .map(f32::to_bits)
            .collect::<BTreeSet<_>>();
        let capacity_values = observations
            .iter()
            .filter(|observation| (0.55..0.85).contains(&observation.bounds().y()))
            .filter(|observation| canonical_value_text(observation.text()).starts_with("capacity:"))
            .filter_map(|observation| parse_gigabytes(observation.text()))
            .map(f32::to_bits)
            .collect::<BTreeSet<_>>();
        let recording_values = observations
            .iter()
            .filter_map(|observation| parse_recording_duration(observation.text()))
            .collect::<BTreeSet<_>>();
        if free_values.len() != 1 || capacity_values.len() != 1 || recording_values.len() != 1 {
            return None;
        }
        let free = f32::from_bits(free_values.into_iter().next()?);
        let capacity = f32::from_bits(capacity_values.into_iter().next()?);
        let (hours, minutes) = recording_values.into_iter().next()?;
        (free <= capacity).then(|| {
            vec![
                format!("Free={free}GB"),
                format!("Capacity={capacity}GB"),
                format!("Recording={hours}h{minutes}m"),
            ]
        })
    }

    fn parse_gigabytes(text: &str) -> Option<f32> {
        let candidate = text
            .rsplit_once(':')
            .map_or(text, |(_, value)| value)
            .trim();
        let number = candidate.strip_suffix("GB")?.trim().parse::<f32>().ok()?;
        (number.is_finite() && number > 0.0).then_some(number)
    }

    fn parse_recording_duration(text: &str) -> Option<(u32, u8)> {
        let canonical = canonical_value_text(text);
        let value = canonical.strip_prefix("(rec:")?.strip_suffix(')')?.trim();
        let (hours, minutes) = value.split_once('h')?;
        let minutes = minutes.strip_suffix('m')?;
        let hours = hours.trim().parse::<u32>().ok()?;
        let minutes = minutes.trim().parse::<u8>().ok()?;
        (minutes < 60).then_some((hours, minutes))
    }

    fn battery_level_payload(screen: &CapturedScreen) -> Option<Vec<String>> {
        let mut shell = ColorExtent::default();
        let mut green = ColorExtent::default();
        let mut yellow = ColorExtent::default();
        let mut red = ColorExtent::default();
        let mut fill = ColorExtent::default();
        for y in 25..155 {
            for x in 65..175 {
                let pixel = screen.frame.pixel(x, y).ok()?;
                let red5 = (pixel >> 11) & 0x1F;
                let green6 = (pixel >> 5) & 0x3F;
                let blue5 = pixel & 0x1F;
                let red6 = red5.saturating_mul(2);
                let blue6 = blue5.saturating_mul(2);
                let neutral_min = red6.min(green6).min(blue6);
                let neutral_max = red6.max(green6).max(blue6);
                if (18..=58).contains(&green6) && neutral_max.saturating_sub(neutral_min) <= 6 {
                    shell.observe(x, y);
                }
                let is_green = red5 <= 8 && green6 >= 40 && blue5 <= 12;
                let is_yellow = red5 >= 24 && green6 >= 40 && blue5 <= 10;
                let is_red = red5 >= 20 && green6 <= 30 && blue5 <= 12;
                if is_green {
                    green.observe(x, y);
                }
                if is_yellow {
                    yellow.observe(x, y);
                }
                if is_red {
                    red.observe(x, y);
                }
                if is_green || is_yellow || is_red {
                    fill.observe(x, y);
                }
            }
        }
        let shell_matches = shell.count >= 400
            && (60..=90).contains(&shell.width())
            && (90..=125).contains(&shell.height());
        let fill_matches = fill.count == 0
            || (fill.count >= 20
                && fill.width() >= 4
                && fill.height() >= 4
                && shell.contains_extent_with_margin(&fill, 0));
        if !shell_matches || !fill_matches {
            return None;
        }
        let fill_color = match (green.count >= 20, yellow.count >= 20, red.count >= 20) {
            (false, false, false) => "none",
            (true, false, false) => "green",
            (false, true, false) => "yellow",
            (false, false, true) => "red",
            (true, true, false) => "green+yellow",
            (true, false, true) => "green+red",
            (false, true, true) => "yellow+red",
            (true, true, true) => "green+yellow+red",
        };
        Some(vec![
            format!("BatteryShell={}x{}", shell.width(), shell.height()),
            format!(
                "BatteryFill={}x{}:{}px",
                fill.width(),
                fill.height(),
                fill.count
            ),
            format!("BatteryFillColor={fill_color}"),
        ])
    }

    #[derive(Debug, Default)]
    struct ColorExtent {
        count: usize,
        min_x: usize,
        max_x: usize,
        min_y: usize,
        max_y: usize,
    }

    impl ColorExtent {
        fn observe(&mut self, x: usize, y: usize) {
            if self.count == 0 {
                self.min_x = x;
                self.max_x = x;
                self.min_y = y;
                self.max_y = y;
            } else {
                self.min_x = self.min_x.min(x);
                self.max_x = self.max_x.max(x);
                self.min_y = self.min_y.min(y);
                self.max_y = self.max_y.max(y);
            }
            self.count = self.count.saturating_add(1);
        }

        const fn width(&self) -> usize {
            if self.count == 0 {
                0
            } else {
                self.max_x.saturating_sub(self.min_x).saturating_add(1)
            }
        }

        const fn height(&self) -> usize {
            if self.count == 0 {
                0
            } else {
                self.max_y.saturating_sub(self.min_y).saturating_add(1)
            }
        }

        const fn contains_extent_with_margin(&self, other: &Self, margin: usize) -> bool {
            self.count > 0
                && other.count > 0
                && other.min_x.saturating_add(margin) >= self.min_x
                && other.max_x <= self.max_x.saturating_add(margin)
                && other.min_y.saturating_add(margin) >= self.min_y
                && other.max_y <= self.max_y.saturating_add(margin)
        }
    }

    fn is_safe_back_context(screen: &CapturedScreen) -> bool {
        if !has_one_rendered_bottom_left_control(screen, "back") {
            return false;
        }
        let has_locator = screen
            .observations
            .iter()
            .any(|observation| is_menu_locator(observation.text()));
        let selected_list = !screen.selected.is_empty()
            && has_exact_text(&screen.observations, "OK")
            && has_locator;
        let value_or_editor_page = has_exact_text(&screen.observations, "OK")
            && v103_selection_bands(&screen.frame).len() == 1;
        selected_list || value_or_editor_page || has_reviewed_safe_title(screen)
    }

    fn menu_exit_key(screen: &CapturedScreen) -> Option<(FrontPanelKey, &'static str)> {
        if is_top_level_menu(screen) || is_restoration_top_level_menu(screen) {
            Some((FrontPanelKey::Menu, "exit-top-level-menu"))
        } else if is_safe_back_context(screen) {
            Some((FrontPanelKey::Mode, "menu-back"))
        } else {
            None
        }
    }

    fn canonical_text(text: &str) -> String {
        let mut canonical = String::new();
        let mut pending_space = false;
        for character in text.trim().chars() {
            if character.is_whitespace() {
                pending_space = !canonical.is_empty();
                continue;
            }
            if pending_space {
                canonical.push(' ');
                pending_space = false;
            }
            match character {
                // Vision occasionally returns visually identical Cyrillic
                // glyphs for this English-only stock UI. Canonicalize only the
                // confusables observed in retained hardware frames.
                'А' | 'а' => canonical.push('a'),
                'В' | 'в' => canonical.push('b'),
                'К' | 'к' => canonical.push('k'),
                'М' | 'м' => canonical.push('m'),
                'Н' | 'н' => canonical.push('h'),
                'О' | 'о' => canonical.push('o'),
                'Р' | 'р' => canonical.push('p'),
                'С' | 'с' => canonical.push('c'),
                'Т' | 'т' => canonical.push('t'),
                'Е' | 'е' => canonical.push('e'),
                'Х' | 'х' => canonical.push('x'),
                // Vision's Latin recognizer emitted a dotless i for the exact
                // stock English title "Gain" in the retained Menu 151 frame.
                'І' | 'і' | 'ı' => canonical.push('i'),
                'Л' | 'л' => canonical.push('l'),
                _ => canonical.extend(character.to_lowercase()),
            }
        }
        canonical.replace("/ ", "/").replace(" /", "/")
    }

    fn canonical_selected_label(text: &str) -> String {
        let canonical = canonical_text(text);
        canonical
            .strip_prefix('•')
            .map_or_else(|| canonical.clone(), |label| label.trim_start().to_owned())
    }

    fn three_frames_are_identical(
        first: &ScreenFrame,
        second: &ScreenFrame,
        third: &ScreenFrame,
    ) -> bool {
        first == second && second == third
    }

    fn require_conclusive(
        summary: &Summary,
        selected_total: usize,
        expected_value_total: usize,
        expected_safe_inspection_total: usize,
        expected_located_not_entered_total: usize,
    ) -> AuditResult<()> {
        if selected_total == 0 {
            return Err(io::Error::other("an empty audit selection cannot pass").into());
        }
        if summary.inconclusive != 0
            || summary.attempted != selected_total
            || summary.located_rows != selected_total
            || summary.value_or_information_validated != expected_value_total
            || summary.row_only_safe_inspected != expected_safe_inspection_total
            || summary.row_only_located_not_entered != expected_located_not_entered_total
            || expected_value_total
                .saturating_add(expected_safe_inspection_total)
                .saturating_add(expected_located_not_entered_total)
                != selected_total
            || summary.restored != selected_total
        {
            return Err(io::Error::other(format!(
                "audit coverage mismatch: selected={selected_total}, expected_value_or_information={expected_value_total}, expected_safe_inspection={expected_safe_inspection_total}, expected_located_not_entered={expected_located_not_entered_total}, attempted={}, located_rows={}, value_or_information_validated={}, row_only_safe_inspected={}, row_only_located_not_entered={}, restored={}, inconclusive={}",
                summary.attempted,
                summary.located_rows,
                summary.value_or_information_validated,
                summary.row_only_safe_inspected,
                summary.row_only_located_not_entered,
                summary.restored,
                summary.inconclusive
            ))
            .into());
        }
        Ok(())
    }

    fn has_exact_text(observations: &[TextObservation], expected: &str) -> bool {
        require_unique_text(
            observations,
            expected,
            0.90,
            NormalizedBounds::FULL_SCREEN,
            1.0,
        )
        .is_ok()
    }

    fn looks_like_frequency(text: &str) -> bool {
        let Some((whole, fraction)) = text.split_once('.') else {
            return false;
        };
        whole.len() == 3
            && (3..=5).contains(&fraction.len())
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    }

    fn millis(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1_000.0
    }

    fn invalid_input(message: impl Into<String>) -> AuditError {
        Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
    }

    #[cfg(test)]
    mod tests {
        use super::{
            APRS_ICON_NAMES, AuditClass, CTCSS_FREQUENCIES, ConfigurationSnapshot, CoverageScope,
            DESTRUCTIVE_ACTION_NUMBERS, EXPECTED_CONFIGURATION_SNAPSHOT_FIELD_COUNT,
            EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT, EXPECTED_MCP_TOTAL_PAGE_COUNT,
            EXPECTED_MENU_COUNT, EXPECTED_SAFE_INSPECTION_COUNT, Endpoint,
            HOME_MASK_INCLUDED_PIXELS, HOME_MASK_SIGNAL_METER_RECT, MENU_134_DATA_PAGE,
            MENU_134_FLAG_PAGE, MENU_134_PRI_CHANNEL, MENU_134_PRI_FLAG_OFFSET,
            MENU_134_PRI_RECORD_OFFSET, MENU_134_WX1_CHANNEL, MENU_134_WX1_FLAG_OFFSET,
            MENU_134_WX1_RECORD_OFFSET, MENU_134_WX1_RX_HZ, MULTI_RECORD_EDITOR_NUMBERS,
            MY_POSITION_ROWS, Menu134PriDisposition, POSITION_COMMENTS, PreMcpTransportPolicy,
            REVIEWED_MANUAL, ROW_ONLY_NUMBERS, RowOnlyPolicy, SAFE_INSPECTION_NUMBERS,
            SafeInspectionOracle, Summary, aligned_use_marker, anchor_page_title,
            apply_pre_mcp_transport_policy, battery_level_payload, canonical_value_text,
            category_parts, centered_scalar_documented_payload, checkbox_payload,
            checkbox_row_has_unique_label, checkbox_row_label_matches, class_for,
            combine_primary_and_cleanup_errors, compare_dual_band_home,
            configuration_snapshot_pages, configuration_snapshots_match, coverage_scope,
            direct_access_keys, dv_gateway_callsign_payload, dynamic_date_time_payload,
            entry_has_typed_value_oracle, entry_value_identity, eq_payload,
            firmware_version_payload, has_one_rendered_bottom_left_control,
            has_reviewed_safe_title, is_restoration_top_level_menu, is_reviewed_single_band_b_home,
            is_reviewed_single_band_home, is_safe_back_context, is_top_level_menu,
            journal_screen_text, looks_like_bluetooth_address, looks_like_bluetooth_device_class,
            looks_like_date, looks_like_time, looks_like_utc_offset, manifest_entry,
            masked_home_bytes, menu_710_is_exact_reviewed_singleton,
            menu_710_singleton_memory_submenu_matches, menu_935_bluetooth_address_identity,
            menu_935_bluetooth_class_identity, network_payload,
            normalized_menu_935_bluetooth_address, numbered_row_documented_payload,
            numbered_row_matches, observed_operation_band, ordered_home_anchor_texts_match,
            ordinary_documented_payload, parse_args_from, parse_eq_frequency, parse_eq_level,
            parse_frequency_khz, parse_menu_manifest, plan_menu_134_pri_pages,
            plan_menu_134_restore_pages, recoverable_menu_failures_result, require_conclusive,
            require_exact_short_text, require_menu_134_priority_scan_off, retained_ui_label_alias,
            reviewed_single_band_home_matches, row_only_anchor, row_only_policy,
            safe_inspection_oracle, safe_inspection_title, screen_has_exact_menu_locator,
            screen_matches_label, scrollable_checkbox_labels, selected_matches_label,
            selected_matches_label_for_menu, sha256_hex, speed_payload, three_frames_are_identical,
            validate_manifest,
        };
        use kenwood_thd75::Radio;
        use kenwood_thd75::memory::MCP_D75_MENU_FIELDS;
        use kenwood_thd75::protocol::programming;
        use kenwood_thd75::radio::automation::FrontPanelKey;
        use kenwood_thd75::screen::ui::{V103_SELECTION_RGB565, v103_selection_bands};
        use kenwood_thd75::screen::vision::{NormalizedBounds, TextObservation};
        use kenwood_thd75::screen::{SCREEN_BYTES, SCREEN_WIDTH, ScreenFrame};
        use kenwood_thd75::transport::MockTransport;
        use kenwood_thd75::types::{
            ChannelMode, Frequency, RadioModel, ShiftDirection, StoredChannel,
        };

        type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

        fn require_error<T>(
            result: super::AuditResult<T>,
            message: &'static str,
        ) -> super::AuditResult<super::AuditError> {
            match result {
                Ok(_) => Err(super::invalid_input(message)),
                Err(error) => Ok(error),
            }
        }

        fn write_bytes(destination: &mut [u8], offset: usize, source: &[u8]) -> TestResult {
            let end = offset
                .checked_add(source.len())
                .ok_or_else(|| super::invalid_input("test fixture byte range overflowed"))?;
            let target = destination
                .get_mut(offset..end)
                .ok_or_else(|| super::invalid_input("test fixture byte range was out of bounds"))?;
            target.copy_from_slice(source);
            Ok(())
        }

        fn set_rgb565_pixel(bytes: &mut [u8], x: usize, y: usize, value: u16) -> TestResult {
            let pixel_index = y
                .checked_mul(SCREEN_WIDTH)
                .and_then(|row| row.checked_add(x))
                .ok_or_else(|| super::invalid_input("test pixel coordinate overflowed"))?;
            let byte_offset = pixel_index
                .checked_mul(2)
                .ok_or_else(|| super::invalid_input("test pixel byte offset overflowed"))?;
            write_bytes(bytes, byte_offset, &value.to_le_bytes())
        }

        fn fill_rgb565_rect(
            bytes: &mut [u8],
            x_range: std::ops::Range<usize>,
            y_range: std::ops::Range<usize>,
            value: u16,
        ) -> TestResult {
            for y in y_range {
                for x in x_range.clone() {
                    set_rgb565_pixel(bytes, x, y, value)?;
                }
            }
            Ok(())
        }

        fn selected_frame(
            x_range: std::ops::Range<usize>,
            y_range: std::ops::Range<usize>,
        ) -> Result<ScreenFrame, Box<dyn std::error::Error + Send + Sync>> {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, x_range, y_range, V103_SELECTION_RGB565)?;
            Ok(ScreenFrame::from_rgb565_le(bytes)?)
        }

        fn single_changed_byte_frame(
            offset: usize,
            description: &'static str,
        ) -> Result<ScreenFrame, Box<dyn std::error::Error + Send + Sync>> {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            let byte = bytes.get_mut(offset).ok_or(description)?;
            *byte = 1;
            Ok(ScreenFrame::from_rgb565_le(bytes)?)
        }

        fn replace_observation(
            observations: &mut [TextObservation],
            index: usize,
            replacement: TextObservation,
        ) -> TestResult {
            let observation = observations
                .get_mut(index)
                .ok_or_else(|| super::invalid_input("synthetic screen has too few observations"))?;
            *observation = replacement;
            Ok(())
        }

        fn check_fm_auto_row(frame: ScreenFrame) -> Result<ScreenFrame, super::AuditError> {
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "FM Auto Det. on",
                        1.0,
                        NormalizedBounds::new(
                            0.006_956_39,
                            0.254_447_85,
                            0.760_902_17,
                            0.139_248_36,
                        )?,
                    )?,
                    TextObservation::new(
                        "DV",
                        1.0,
                        NormalizedBounds::new(0.808_333_34, 0.266_666_68, 0.108_333_334, 0.10)?,
                    )?,
                    TextObservation::new(
                        "On",
                        1.0,
                        NormalizedBounds::new(
                            0.866_666_7,
                            0.388_888_9,
                            0.091_666_67,
                            0.077_777_78,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&screen, "FM Auto Det. on DV"));
            screen.observations.push(TextObservation::new(
                "Unexpected",
                1.0,
                NormalizedBounds::new(0.35, 46.0 / 180.0, 0.30, 18.0 / 180.0)?,
            )?);
            assert!(!selected_matches_label(&screen, "FM Auto Det. on DV"));
            assert!(screen.observations.pop().is_some());
            screen.observations.push(TextObservation::new(
                "DV",
                1.0,
                NormalizedBounds::new(0.50, 46.0 / 180.0, 0.10, 18.0 / 180.0)?,
            )?);
            assert!(!selected_matches_label(&screen, "FM Auto Det. on DV"));
            Ok(screen.frame)
        }

        fn check_display_hold_row(frame: ScreenFrame) -> Result<ScreenFrame, super::AuditError> {
            let mut observations = vec![TextObservation::new(
                "Display Hold Time",
                1.0,
                NormalizedBounds::new(0.013_724_981, 0.248_474_39, 0.849_638_6, 0.138_628_24)?,
            )?];
            observations.extend([
                TextObservation::new(
                    "Display Hold",
                    1.0,
                    NormalizedBounds::new(0.007_609_933, 0.260_035_2, 0.609_440_5, 0.132_493_35)?,
                )?,
                TextObservation::new(
                    "Time",
                    1.0,
                    NormalizedBounds::new(0.627_642_3, 0.255_962_04, 0.245_325_18, 0.136_314_36)?,
                )?,
                TextObservation::new(
                    "5",
                    1.0,
                    NormalizedBounds::new(0.708_333_3, 0.377_777_79, 0.05, 0.088_888_89)?,
                )?,
                TextObservation::new(
                    "sec",
                    1.0,
                    NormalizedBounds::new(0.841_666_64, 0.40, 0.116_666_67, 0.066_666_67)?,
                )?,
            ]);
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations,
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&screen, "Display Hold Time"));
            let fragments_only = super::CapturedScreen {
                crc32: screen.crc32,
                frame: screen.frame,
                observations: screen.observations.into_iter().skip(1).collect(),
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&fragments_only, "Display Hold Time"));
            Ok(fragments_only.frame)
        }

        fn check_usb_audio_level_row() -> TestResult {
            let frame = selected_frame(0..SCREEN_WIDTH, 124..164)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "USB Audio Out. Lvl.",
                        1.0,
                        NormalizedBounds::new(
                            0.011_640_143,
                            0.696_791_65,
                            0.935_191,
                            0.141_157_87,
                        )?,
                    )?,
                    TextObservation::new(
                        "USB Audio Out.",
                        1.0,
                        NormalizedBounds::new(
                            0.007_155_005_4,
                            0.700_637_04,
                            0.702_457,
                            0.133_807_17,
                        )?,
                    )?,
                    TextObservation::new(
                        "LVI.",
                        1.0,
                        NormalizedBounds::new(
                            0.763_974_9,
                            0.718_633_23,
                            0.181_526_56,
                            0.095_427_72,
                        )?,
                    )?,
                    TextObservation::new(
                        "Level 7",
                        1.0,
                        NormalizedBounds::new(
                            0.712_242_8,
                            0.827_381_55,
                            0.246_300_73,
                            0.075_562_306,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(!selected_matches_label(&screen, "Display Hold Lvl."));
            screen.observations.push(TextObservation::new(
                "LVI.",
                1.0,
                NormalizedBounds::new(0.50, 0.70, 0.18, 0.10)?,
            )?);
            assert!(!selected_matches_label(&screen, "USB Audio Out. Lvl."));
            Ok(())
        }

        fn parse_cli(arguments: &[&str]) -> Result<super::Config, super::AuditError> {
            parse_args_from(arguments.iter().map(|argument| (*argument).to_owned()))
        }

        fn test_entry_identity(
            entries: &[super::MenuEntry],
            number: &str,
            displayed: &str,
        ) -> super::AuditResult<Option<String>> {
            Ok(entry_value_identity(
                manifest_entry(entries, number)?,
                displayed,
            ))
        }

        fn assert_domain_value(
            entries: &[super::MenuEntry],
            number: &str,
            displayed: &str,
            expected_valid: bool,
        ) -> TestResult {
            let actual_valid = test_entry_identity(entries, number, displayed)?.is_some();
            assert_eq!(
                actual_valid, expected_valid,
                "menu {number} validity for reviewed value {displayed:?}"
            );
            Ok(())
        }

        fn check_numeric_domain_group_one(entries: &[super::MenuEntry]) -> TestResult {
            for value in 1..=8 {
                assert_domain_value(entries, "101", &format!("Type {value}"), true)?;
            }
            for value in ["Type 0", "Type 9"] {
                assert_domain_value(entries, "101", value, false)?;
            }
            for number in ["132", "133"] {
                for value in ["1 sec", "10 sec"] {
                    assert_domain_value(entries, number, value, true)?;
                }
                for value in ["0 sec", "11 sec"] {
                    assert_domain_value(entries, number, value, false)?;
                }
            }
            for value in ["Off", "15 min", "30 min", "60 min"] {
                assert_domain_value(entries, "136", value, true)?;
            }
            for value in ["On", "10 min", "45 min", "90 min"] {
                assert_domain_value(entries, "136", value, false)?;
            }
            for (value, valid) in [("0", true), ("9", true), ("10", false)] {
                assert_domain_value(entries, "151", value, valid)?;
            }
            for value in (400..=1000).step_by(100) {
                assert_domain_value(entries, "170", &format!("{value} Hz"), true)?;
            }
            for value in ["399 Hz", "450 Hz", "1001 Hz"] {
                assert_domain_value(entries, "170", value, false)?;
            }
            assert_domain_value(entries, "402", "Off", true)?;
            for value in 1..=4 {
                assert_domain_value(entries, "402", &format!("{value}-Digit"), true)?;
            }
            for value in ["0-Digit", "5-Digit"] {
                assert_domain_value(entries, "402", value, false)?;
            }
            for value in ["Off", "1 min", "2 min", "4 min", "8 min", "Auto"] {
                assert_domain_value(entries, "404", value, true)?;
            }
            for value in ["3 min", "16 min"] {
                assert_domain_value(entries, "404", value, false)?;
            }
            for (number, minimum, maximum, suffix) in [
                ("413", 2_u16, 1800_u16, "sec"),
                ("531", 1, 100, "min"),
                ("532", 10, 180, "sec"),
                ("533", 5, 90, "deg"),
                ("535", 5, 180, "sec"),
                ("581", 1, 250, "sec"),
                ("701", 1, 10, "sec"),
                ("901", 3, 60, "sec"),
            ] {
                assert_domain_value(entries, number, &format!("{minimum} {suffix}"), true)?;
                assert_domain_value(entries, number, &format!("{maximum} {suffix}"), true)?;
                if let Some(adjacent) = minimum.checked_sub(1) {
                    assert_domain_value(entries, number, &format!("{adjacent} {suffix}"), false)?;
                }
                assert_domain_value(entries, number, &format!("{} {suffix}", maximum + 1), false)?;
            }
            for (value, valid) in [
                ("0.01 mile", true),
                ("9.99 km", true),
                ("0.00 mile", false),
                ("10.00 nm", false),
                ("0.1 mile", false),
                ("00.01 mile", false),
            ] {
                assert_domain_value(entries, "414", value, valid)?;
            }
            for (value, valid) in [
                ("1 (10deg/speed)", true),
                ("255 10deg/speed", true),
                ("0 (10deg/speed)", false),
                ("256 (10deg/speed)", false),
            ] {
                assert_domain_value(entries, "534", value, valid)?;
            }
            Ok(())
        }

        fn check_numeric_domain_group_two(entries: &[super::MenuEntry]) -> TestResult {
            for frequency in CTCSS_FREQUENCIES {
                assert_domain_value(entries, "593", &format!("{frequency} Hz"), true)?;
            }
            for value in ["66.9 Hz", "67.1 Hz", "254.2 Hz"] {
                assert_domain_value(entries, "593", value, false)?;
            }
            for (value, valid) in [
                ("Level 1", true),
                ("Level 50", true),
                ("1", false),
                ("Level 0", false),
                ("Level 51", false),
            ] {
                assert_domain_value(entries, "615", value, valid)?;
            }
            for value in 0..=99 {
                assert_domain_value(entries, "621", &format!("{value:02}"), true)?;
            }
            for value in ["0", "000", "100"] {
                assert_domain_value(entries, "621", value, false)?;
            }
            for number in ["915", "917"] {
                for value in ["Volume Link", "VOL Link", "VOL"] {
                    assert_domain_value(entries, number, value, true)?;
                }
                for value in 1..=7 {
                    assert_domain_value(entries, number, &format!("Level {value}"), true)?;
                }
                for value in ["Level 0", "Level 8"] {
                    assert_domain_value(entries, number, value, false)?;
                }
            }
            for (number, prefix, maximum) in [("918", "Speed", 4), ("91A", "Level", 7)] {
                for value in 1..=maximum {
                    assert_domain_value(entries, number, &format!("{prefix} {value}"), true)?;
                }
                assert_domain_value(entries, number, &format!("{prefix} 0"), false)?;
                assert_domain_value(entries, number, &format!("{prefix} {}", maximum + 1), false)?;
            }
            assert_eq!(APRS_ICON_NAMES.len(), 68);
            for icon in APRS_ICON_NAMES {
                assert_domain_value(entries, "501", icon, true)?;
            }
            assert_domain_value(entries, "501", "Unknown Icon", false)?;
            assert_eq!(POSITION_COMMENTS.len(), 15);
            for comment in POSITION_COMMENTS {
                assert_domain_value(entries, "502", comment, true)?;
            }
            assert_domain_value(entries, "502", "CUSTOM7", false)?;
            Ok(())
        }

        fn check_centered_scalar_cases(
            entries: &[super::MenuEntry],
            frame: &ScreenFrame,
        ) -> TestResult {
            let cases: [(&str, &[&str]); 21] = [
                ("120", &["2. 4 kHz"]),
                ("121", &["1.0 kHz", "1.0 KHZ"]),
                ("122", &["6.0 kHz", "6. 0 kHz"]),
                ("132", &["8 sec"]),
                ("133", &["4 sec"]),
                ("140", &["5.00 MHz"]),
                ("170", &["800 Hz"]),
                ("413", &["10 sec"]),
                ("414", &["0.01 mile"]),
                ("523", &["Off"]),
                ("531", &["30 min"]),
                ("532", &["120 sec", "120", "sec"]),
                ("533", &["28 deg"]),
                ("534", &["26 (10deg/speed)"]),
                ("535", &["60 sec", "60", "sec"]),
                ("550", &["Off"]),
                ("593", &["100.0 Hz", "100. 0 Hz"]),
                ("615", &["Level 25"]),
                ("621", &["00"]),
                ("901", &["13 sec"]),
                ("91A", &["Level 7"]),
            ];
            for (number, values) in cases {
                let entry = manifest_entry(entries, number)?;
                let value_bounds = match number {
                    "413" => NormalizedBounds::new(
                        0.379_942_54,
                        0.392_260_55,
                        0.315_402_3,
                        0.131_187_74,
                    )?,
                    "414" => {
                        NormalizedBounds::new(0.257_482_05, 0.383_476_5, 0.460_035_9, 0.144_158_14)?
                    }
                    _ => NormalizedBounds::new(0.31, 0.388, 0.36, 0.125)?,
                };
                let mut observations = values
                    .iter()
                    .map(|value| TextObservation::new(*value, 1.0, value_bounds))
                    .collect::<Result<Vec<_>, _>>()?;
                observations.push(TextObservation::new(
                    anchor_page_title(entry),
                    1.0,
                    NormalizedBounds::new(0.0, 0.02, 0.60, 0.10)?,
                )?);
                observations.push(TextObservation::new(
                    "Back",
                    1.0,
                    NormalizedBounds::new(0.08, 0.90, 0.16, 0.08)?,
                )?);
                let screen = super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame: frame.clone(),
                    observations,
                    selected: Vec::new(),
                };
                assert!(
                    centered_scalar_documented_payload(entry, &screen).is_some(),
                    "centered scalar Menu {number} should validate"
                );
            }
            Ok(())
        }

        #[test]
        fn cli_defaults_to_the_existing_named_bluetooth_endpoint() -> TestResult {
            let config = parse_cli(&["--output-dir", "/private/tmp/audit"])?;
            assert_eq!(config.endpoint, Endpoint::Bluetooth("TH-D75".to_owned()));
            Ok(())
        }

        #[test]
        fn cli_accepts_an_explicit_usb_cdc_port_for_scoped_menu_audits() -> TestResult {
            let config = parse_cli(&[
                "--port",
                "/dev/cu.usbmodem1234",
                "--output-dir",
                "/private/tmp/audit",
                "--menu",
                "991",
            ])?;
            assert_eq!(
                config.endpoint,
                Endpoint::Usb("/dev/cu.usbmodem1234".to_owned())
            );
            assert_eq!(
                config.endpoint.pre_mcp_transport_policy(),
                PreMcpTransportPolicy::ReopenUsbCdcAndIdentify
            );
            assert_eq!(config.only_menu.as_deref(), Some("991"));
            Ok(())
        }

        #[test]
        fn cli_preserves_an_explicit_bluetooth_device_name() -> TestResult {
            let config = parse_cli(&[
                "--device",
                "Workshop D75",
                "--output-dir",
                "/private/tmp/audit",
            ])?;
            assert_eq!(
                config.endpoint,
                Endpoint::Bluetooth("Workshop D75".to_owned())
            );
            assert_eq!(
                config.endpoint.pre_mcp_transport_policy(),
                PreMcpTransportPolicy::ReuseQualifiedLink
            );
            Ok(())
        }

        #[tokio::test(start_paused = true)]
        async fn usb_pre_mcp_boundary_reopens_then_completes_mcp_and_cat_recovery() -> TestResult {
            let mut mock = MockTransport::new();
            let page: u16 = 0x0010;
            let page_bytes = [0x5A; programming::PAGE_SIZE];
            let [page_hi, page_lo] = page.to_be_bytes();
            let mut page_response = vec![b'W', page_hi, page_lo, 0, 0];
            page_response.extend_from_slice(&page_bytes);

            mock.expect_reopen(Ok(()));
            mock.expect_reopen(Ok(()));
            mock.expect(b"ID\r", b"ID TH-D75\r");
            mock.expect(programming::ENTER_PROGRAMMING, b"0M\r");
            mock.expect(
                &programming::build_read_command(programming::McpPage::new(page)?),
                &page_response,
            );
            mock.expect(&[programming::ACK], &[programming::ACK]);
            mock.expect(b"E", &[programming::ACK]);
            mock.expect(b"ID\r", b"ID TH-D75\r");
            mock.expect(b"ID\r", b"ID TH-D75\r");
            let mut radio = Radio::new(mock);

            apply_pre_mcp_transport_policy(
                &mut radio,
                PreMcpTransportPolicy::ReopenUsbCdcAndIdentify,
            )
            .await?;

            let typed_page = programming::McpPage::new(page)?;
            let pages = radio.read_sparse_memory_pages(&[typed_page]).await?;
            assert_eq!(pages, vec![(typed_page, page_bytes)]);
            let identity = radio.identify().await?;
            assert_eq!(identity.model, RadioModel::ThD75);
            Ok(())
        }

        #[tokio::test]
        async fn bluetooth_pre_mcp_boundary_reuses_the_qualified_link() -> TestResult {
            let mut mock = MockTransport::new();
            mock.expect(b"ID\r", b"ID TH-D75\r");
            let mut radio = Radio::new(mock);

            apply_pre_mcp_transport_policy(&mut radio, PreMcpTransportPolicy::ReuseQualifiedLink)
                .await?;

            let identity = radio.identify().await?;
            assert_eq!(identity.model, RadioModel::ThD75);
            Ok(())
        }

        #[test]
        fn cli_rejects_usb_and_bluetooth_endpoints_together_in_either_order() -> TestResult {
            for arguments in [
                [
                    "--port",
                    "/dev/cu.usbmodem1234",
                    "--device",
                    "TH-D75",
                    "--output-dir",
                    "/private/tmp/audit",
                ],
                [
                    "--device",
                    "TH-D75",
                    "--port",
                    "/dev/cu.usbmodem1234",
                    "--output-dir",
                    "/private/tmp/audit",
                ],
            ] {
                let error = require_error(
                    parse_cli(&arguments),
                    "USB and Bluetooth endpoints must be mutually exclusive",
                )?
                .to_string();
                assert!(error.contains("--port and --device are mutually exclusive"));
            }
            Ok(())
        }

        #[test]
        fn cli_rejects_non_absolute_or_bluetooth_serial_paths() -> TestResult {
            let relative = require_error(
                parse_cli(&[
                    "--port",
                    "cu.usbmodem1234",
                    "--output-dir",
                    "/private/tmp/audit",
                ]),
                "relative serial path must fail",
            )?
            .to_string();
            assert!(relative.contains("--port must be an absolute path"));

            let bluetooth = require_error(
                parse_cli(&[
                    "--port",
                    "/dev/cu.TH-D75",
                    "--output-dir",
                    "/private/tmp/audit",
                ]),
                "Bluetooth serial alias must not be accepted as USB",
            )?
            .to_string();
            assert!(bluetooth.contains("--port requires a USB CDC path"));
            Ok(())
        }

        fn audit_error(message: &'static str) -> super::AuditResult<()> {
            Err(std::io::Error::other(message).into())
        }

        fn synthetic_stored_channel(receive_frequency: Frequency) -> StoredChannel {
            let mut wire = [0_u8; StoredChannel::BYTE_SIZE];
            wire[..4].copy_from_slice(&receive_frequency.to_le_bytes());
            StoredChannel::from_bytes(&wire).unwrap_or_else(|error| {
                unreachable!("fixed all-zero synthetic channel record must decode: {error}")
            })
        }

        fn menu_134_stock_wx1() -> StoredChannel {
            StoredChannel {
                receive_frequency: Frequency::new(MENU_134_WX1_RX_HZ),
                transmit_offset_or_frequency: Frequency::new(0),
                mode: ChannelMode::Fm,
                split: false,
                shift: ShiftDirection::Simplex,
                ..synthetic_stored_channel(Frequency::new(MENU_134_WX1_RX_HZ))
            }
        }

        fn menu_134_empty_pri_fixture()
        -> super::AuditResult<([u8; programming::PAGE_SIZE], [u8; programming::PAGE_SIZE])>
        {
            let mut flag = [0xA5; programming::PAGE_SIZE];
            flag[MENU_134_PRI_FLAG_OFFSET] = programming::FLAG_EMPTY;
            flag[MENU_134_WX1_FLAG_OFFSET] = programming::FLAG_VHF;
            let mut data = [0x5A; programming::PAGE_SIZE];
            write_bytes(
                &mut data,
                MENU_134_WX1_RECORD_OFFSET,
                &menu_134_stock_wx1().to_bytes(),
            )?;
            Ok((flag, data))
        }

        fn synthetic_home_screen(
            mode_anchor: &str,
            first_frequency_center_px: f32,
            second_frequency_center_px: f32,
        ) -> Result<super::CapturedScreen, Box<dyn std::error::Error + Send + Sync>> {
            let observations = vec![
                TextObservation::new(
                    mode_anchor,
                    1.0,
                    NormalizedBounds::new(0.05, 22.0 / 180.0, 0.20, 6.0 / 180.0)?,
                )?,
                TextObservation::new(
                    "144.000",
                    1.0,
                    NormalizedBounds::new(
                        0.15,
                        (first_frequency_center_px - 9.0) / 180.0,
                        0.40,
                        18.0 / 180.0,
                    )?,
                )?,
                TextObservation::new(
                    "440.000",
                    1.0,
                    NormalizedBounds::new(
                        0.15,
                        (second_frequency_center_px - 9.0) / 180.0,
                        0.40,
                        18.0 / 180.0,
                    )?,
                )?,
            ];
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            Ok(super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations,
                selected: Vec::new(),
            })
        }

        fn synthetic_top_menu_screen(
            conflicting_title: bool,
        ) -> Result<super::CapturedScreen, Box<dyn std::error::Error + Send + Sync>> {
            let mut observations = vec![
                TextObservation::new("Menu", 1.0, NormalizedBounds::new(0.0, 0.02, 0.16, 0.10)?)?,
                // Vision may return an overlapping alternate recognition for
                // the same rendered title. It is not a second screen label.
                TextObservation::new(
                    "tenu",
                    1.0,
                    NormalizedBounds::new(0.016, 0.033, 0.13, 0.067)?,
                )?,
                TextObservation::new("MEM", 1.0, NormalizedBounds::new(0.43, 0.24, 0.16, 0.067)?)?,
                TextObservation::new("APRS", 1.0, NormalizedBounds::new(0.42, 0.41, 0.15, 0.067)?)?,
                TextObservation::new(
                    "Digital",
                    1.0,
                    NormalizedBounds::new(0.31, 0.79, 0.35, 0.10)?,
                )?,
                TextObservation::new("OK", 1.0, NormalizedBounds::new(0.78, 0.91, 0.09, 0.078)?)?,
                // Vision may likewise return two overlapping readings of the
                // same physical soft-key control.
                TextObservation::new(
                    "Ok",
                    1.0,
                    NormalizedBounds::new(0.793, 0.914, 0.078, 0.060)?,
                )?,
            ];
            if conflicting_title {
                observations.push(TextObservation::new(
                    "Setup",
                    1.0,
                    NormalizedBounds::new(0.23, 0.02, 0.11, 0.10)?,
                )?);
            }
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            Ok(super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations,
                selected: Vec::new(),
            })
        }

        #[test]
        fn reviewed_manual_is_an_exact_partitioned_manifest() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            validate_manifest(&entries)?;
            assert_eq!(entries.len(), EXPECTED_MENU_COUNT);
            assert_eq!(
                entries.first().map(|entry| entry.number.as_str()),
                Some("100")
            );
            assert_eq!(
                entries.last().map(|entry| entry.number.as_str()),
                Some("999")
            );
            assert_eq!(class_for("820")?, AuditClass::RowOnly);
            assert_eq!(class_for("312")?, AuditClass::RowOnly);
            assert_eq!(row_only_policy("312")?, RowOnlyPolicy::MultiRecordEditor);
            assert_eq!(row_only_anchor("312")?, "311");
            assert_eq!(class_for("930")?, AuditClass::Guarded);
            assert_eq!(class_for("991")?, AuditClass::Information);
            Ok(())
        }

        #[test]
        fn evidence_sha256_matches_standard_vectors() -> TestResult {
            assert_eq!(
                sha256_hex(b"")?,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
            assert_eq!(
                sha256_hex(b"abc")?,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            );
            assert_eq!(
                sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")?,
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
            );
            Ok(())
        }

        #[test]
        fn direct_access_rejects_non_decimal_91a_route() -> TestResult {
            assert_eq!(
                direct_access_keys("920")?,
                vec![
                    FrontPanelKey::Pf1_9,
                    FrontPanelKey::Mr2,
                    FrontPanelKey::Mark0,
                ]
            );
            assert_eq!(
                direct_access_keys("919")?,
                vec![
                    FrontPanelKey::Pf1_9,
                    FrontPanelKey::Vfo1,
                    FrontPanelKey::Pf1_9,
                ]
            );
            assert!(direct_access_keys("91A").is_err());
            Ok(())
        }

        #[test]
        fn every_row_only_leaf_has_a_harmless_same_category_anchor() -> TestResult {
            let manifest = parse_menu_manifest(REVIEWED_MANUAL)?;
            let row_only = ROW_ONLY_NUMBERS
                .split_ascii_whitespace()
                .collect::<Vec<_>>();
            assert_eq!(row_only.len(), 55);
            for number in row_only {
                let target = manifest_entry(&manifest, number)?;
                let anchor_number = row_only_anchor(number)?;
                let anchor = manifest_entry(&manifest, anchor_number)?;
                assert_ne!(anchor.number, target.number);
                assert!(matches!(
                    anchor.class,
                    AuditClass::Value | AuditClass::Information
                ));
                assert_eq!(
                    category_parts(&anchor.category_path)?.0,
                    category_parts(&target.category_path)?.0,
                    "menu {number} crosses top-level categories"
                );
                assert_eq!(direct_access_keys(anchor_number)?.len(), 3);
            }
            assert_eq!(
                anchor_page_title(manifest_entry(&manifest, "840")?),
                "microSD Card"
            );
            Ok(())
        }

        #[test]
        fn row_only_handling_is_an_exact_14_safe_and_41_never_entered_partition() -> TestResult {
            let row_only = ROW_ONLY_NUMBERS
                .split_ascii_whitespace()
                .collect::<std::collections::BTreeSet<_>>();
            let safe = SAFE_INSPECTION_NUMBERS
                .split_ascii_whitespace()
                .collect::<std::collections::BTreeSet<_>>();
            let destructive = DESTRUCTIVE_ACTION_NUMBERS
                .split_ascii_whitespace()
                .collect::<std::collections::BTreeSet<_>>();
            let editors = MULTI_RECORD_EDITOR_NUMBERS
                .split_ascii_whitespace()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(safe.len(), EXPECTED_SAFE_INSPECTION_COUNT);
            assert_eq!(destructive.len(), 16);
            assert_eq!(editors.len(), 25);
            assert!(safe.is_disjoint(&destructive));
            assert!(safe.is_disjoint(&editors));
            assert!(destructive.is_disjoint(&editors));
            let safe_and_destructive = safe
                .union(&destructive)
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let union = safe_and_destructive
                .union(&editors)
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(union, row_only);
            for number in &safe {
                assert_eq!(row_only_policy(number)?, RowOnlyPolicy::SafeInspection);
                let _oracle = safe_inspection_oracle(number)?;
            }
            for number in &destructive {
                assert_eq!(row_only_policy(number)?, RowOnlyPolicy::DestructiveAction);
                assert!(safe_inspection_oracle(number).is_err());
            }
            for number in &editors {
                assert_eq!(row_only_policy(number)?, RowOnlyPolicy::MultiRecordEditor);
                assert!(safe_inspection_oracle(number).is_err());
            }
            Ok(())
        }

        #[test]
        fn every_safe_inspection_has_the_reviewed_page_specific_oracle() -> TestResult {
            let field_cases = [
                ("401", "gps.MyPositionSelect"),
                ("500", "aprs.MyCallsign"),
                ("503", "aprs.StatusTextSelect"),
                ("504", "aprs.PacketPathType"),
                ("516", "aprs.ObjectUsedNo"),
                ("562", "aprs.AutoReplyTargetCall"),
                ("572", "aprs.SpecialCall"),
                ("585", "aprs.UIfloodAliases"),
                ("588", "aprs.UItraceAliases"),
            ];
            for (number, expected_field) in field_cases {
                let actual_field = match safe_inspection_oracle(number)? {
                    SafeInspectionOracle::ActiveChoice { field, .. }
                    | SafeInspectionOracle::ShortText { field, .. } => field,
                    oracle => {
                        return Err(super::invalid_input(format!(
                            "menu {number} has unexpected oracle {oracle:?}"
                        )));
                    }
                };
                assert_eq!(actual_field, expected_field, "menu {number}");
            }
            assert!(matches!(
                safe_inspection_oracle("100")?,
                SafeInspectionOracle::ProgrammableVfo
            ));
            assert!(matches!(
                safe_inspection_oracle("651")?,
                SafeInspectionOracle::DvGatewayCallsign
            ));
            assert!(matches!(
                safe_inspection_oracle("911")?,
                SafeInspectionOracle::EqualizerCheckboxes
            ));
            assert!(matches!(
                safe_inspection_oracle("935")?,
                SafeInspectionOracle::BluetoothInformation
            ));
            assert!(matches!(
                safe_inspection_oracle("950")?,
                SafeInspectionOracle::DynamicDateTime
            ));
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            assert_eq!(
                safe_inspection_title(manifest_entry(&entries, "950")?),
                "Date & Time"
            );
            assert_eq!(
                safe_inspection_title(manifest_entry(&entries, "516")?),
                "APRS Object"
            );
            assert_eq!(
                safe_inspection_title(manifest_entry(&entries, "935")?),
                "Bluetooth Device Information"
            );
            Ok(())
        }

        #[test]
        fn retained_r24_menu651_matches_the_exact_mcp_selected_numbered_callsign_row() -> TestResult
        {
            let mut page = [0_u8; programming::PAGE_SIZE];
            page[0xA1] = 0;
            page[0xA8..0xB0].copy_from_slice(b"KQ4NIT  ");
            page[0xB0..0xB4].copy_from_slice(b"D75A");
            let before = ConfigurationSnapshot {
                pages: vec![(0x001C, page)],
                sha256: [0; 32],
                artifact: "synthetic-menu-651-before.bin".to_owned(),
            };

            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 20..44, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![TextObservation::new(
                    "1:KQ4NIT /D75A",
                    1.0,
                    NormalizedBounds::new(0.01, 20.0 / 180.0, 0.65, 24.0 / 180.0)?,
                )?],
                selected: vec!["1:KQ4NIT /D75A".to_owned()],
            };
            let payload = dv_gateway_callsign_payload(&screen, &before)?;
            assert_eq!(payload.get("selected_index"), Some(&serde_json::json!(0)));
            assert_eq!(
                payload.get("selected_row_ordinal"),
                Some(&serde_json::json!(1))
            );
            assert!(payload.get("selected_callsign").is_none());
            assert!(payload.get("selected_memo").is_none());

            let observation = screen.observations.first_mut().ok_or_else(|| {
                super::invalid_input("synthetic callsign screen has no observation")
            })?;
            *observation = TextObservation::new(
                "1:KQ4NIT /OTHER",
                1.0,
                NormalizedBounds::new(0.01, 20.0 / 180.0, 0.65, 24.0 / 180.0)?,
            )?;
            assert!(dv_gateway_callsign_payload(&screen, &before).is_err());
            Ok(())
        }

        #[test]
        fn retained_r19_menu_401_uses_the_stock_mcp_index_order() {
            assert_eq!(
                MY_POSITION_ROWS,
                [
                    "My Position 1",
                    "My Position 2",
                    "My Position 3",
                    "My Position 4",
                    "My Position 5",
                    "GPS",
                ]
            );
        }

        #[test]
        fn retained_r19_menu_516_page_is_a_reviewed_safe_back_context() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "APRS Object",
                        1.0,
                        NormalizedBounds::new(
                            0.006_532_849,
                            0.015_020_279,
                            0.386_800_47,
                            0.109_894_56,
                        )?,
                    )?,
                    TextObservation::new(
                        "APRS Obiect",
                        1.0,
                        NormalizedBounds::new(0.016_666_666, 0.027_777_778, 0.368_75, 0.075)?,
                    )?,
                    TextObservation::new(
                        "Back",
                        1.0,
                        NormalizedBounds::new(0.091_666_67, 0.911_111_1, 0.15, 0.077_777_78)?,
                    )?,
                ],
                selected: vec!["Object1".to_owned(), "USE".to_owned()],
            };
            assert!(screen_matches_label(&screen, "APRS Object"));
            assert!(has_reviewed_safe_title(&screen));
            assert!(is_safe_back_context(&screen));
            Ok(())
        }

        #[test]
        fn retained_r20_space_soft_key_cannot_be_treated_as_mode_back() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Reply Message Text",
                        1.0,
                        NormalizedBounds::new(0.0, 0.011_111_111, 0.616_666_7, 0.111_111_11)?,
                    )?,
                    TextObservation::new(
                        "Space",
                        1.0,
                        NormalizedBounds::new(0.066_666_66, 0.9, 0.191_666_66, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "Clear",
                        1.0,
                        NormalizedBounds::new(0.725, 0.9, 0.2, 0.088_888_89)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(!has_one_rendered_bottom_left_control(&screen, "back"));
            assert!(!is_safe_back_context(&screen));
            Ok(())
        }

        #[test]
        fn safe_inspection_format_parsers_accept_bounds_and_reject_near_misses() {
            assert_eq!(parse_frequency_khz("136.000"), Some(136_000));
            assert_eq!(parse_frequency_khz("136"), Some(136_000));
            assert_eq!(parse_frequency_khz("173 MHz"), Some(173_000));
            assert_eq!(parse_frequency_khz("470.00000 MHz"), Some(470_000));
            assert_eq!(parse_frequency_khz("36.000"), None);
            assert!(looks_like_bluetooth_address("00:1A:2B:3C:4D:5E"));
            assert!(!looks_like_bluetooth_address("42:F3:BO:AE:1C:95"));
            assert_eq!(
                normalized_menu_935_bluetooth_address("42:f3:BO:ae:1c:95").as_deref(),
                Some("42:F3:B0:AE:1C:95")
            );
            assert_eq!(
                normalized_menu_935_bluetooth_address("42:F3:BO:AE:1C:9O"),
                None
            );
            assert_eq!(
                normalized_menu_935_bluetooth_address("42:F3:Bo:AE:1C:95"),
                None
            );
            assert!(!looks_like_bluetooth_address("00:1A:2B:3C:4D"));
            assert!(!looks_like_bluetooth_address("00:1A:2B:3C:4G:5E"));
            assert!(looks_like_bluetooth_device_class("0x001F00"));
            assert!(looks_like_bluetooth_device_class("Phone"));
            assert!(!looks_like_bluetooth_device_class("0x001F0"));
            assert!(!looks_like_bluetooth_device_class("Computer"));
            assert!(looks_like_date("2026/07/31"));
            assert!(looks_like_date("07-31-2026"));
            assert!(!looks_like_date("2026/13/31"));
            assert!(looks_like_time("23:59:59"));
            assert!(looks_like_time("00:00"));
            assert!(!looks_like_time("24:00"));
            assert!(looks_like_utc_offset("UTC -05:00"));
            assert!(looks_like_utc_offset("UTC+12:45"));
            assert!(!looks_like_utc_offset("UTC +12:20"));
        }

        #[test]
        fn retained_r35_menu950_accepts_the_low_timezone_row_and_rejects_wrong_geometry()
        -> TestResult {
            let mut page = [0_u8; programming::PAGE_SIZE];
            page[0x83] = 0;
            let before = ConfigurationSnapshot {
                pages: vec![(0x0010, page)],
                sha256: [0; 32],
                artifact: "synthetic-menu-950-before.bin".to_owned(),
            };
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "07/31/2026",
                        1.0,
                        NormalizedBounds::new(0.45, 0.244_444_44, 0.525, 0.133_333_34)?,
                    )?,
                    TextObservation::new(
                        "19:15",
                        1.0,
                        NormalizedBounds::new(
                            0.708_333_3,
                            0.522_222_2,
                            0.258_333_33,
                            0.122_222_22,
                        )?,
                    )?,
                    TextObservation::new(
                        "UTC -05:00",
                        1.0,
                        NormalizedBounds::new(
                            0.449_186_56,
                            0.782_445_55,
                            0.518_293_56,
                            0.135_108_95,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };

            let payload = dynamic_date_time_payload(&screen, &before)?;
            assert_eq!(payload.get("mcp_raw"), Some(&serde_json::json!(0)));
            assert_eq!(
                payload
                    .get("comparison")
                    .and_then(serde_json::Value::as_str),
                Some(
                    "MCP-time-zone-domain-and-unique-right-column-live-date-time-UTC-offset-syntax-in-three-exact-value-rows"
                )
            );

            replace_observation(
                &mut screen.observations,
                2,
                TextObservation::new(
                    "UTC -05:00",
                    1.0,
                    NormalizedBounds::new(0.45, 0.91, 0.52, 0.08)?,
                )?,
            )?;
            assert!(dynamic_date_time_payload(&screen, &before).is_err());

            replace_observation(
                &mut screen.observations,
                2,
                TextObservation::new(
                    "UTC -05:00",
                    1.0,
                    NormalizedBounds::new(0.42, 0.78, 0.20, 0.13)?,
                )?,
            )?;
            screen.observations.push(TextObservation::new(
                "UTC +09:00",
                1.0,
                NormalizedBounds::new(0.76, 0.80, 0.20, 0.11)?,
            )?);
            assert!(dynamic_date_time_payload(&screen, &before).is_err());
            Ok(())
        }

        #[test]
        fn retained_r29_menu935_resolves_one_private_address_and_low_device_class_locus()
        -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let exact = TextObservation::new(
                "42:F3:B0:AE:1C:95",
                1.0,
                NormalizedBounds::new(0.108_333_34, 0.522_222_2, 0.858_333_35, 0.122_222_22)?,
            )?;
            let one_o = TextObservation::new(
                "42:F3:BO:AE:1C:95",
                1.0,
                NormalizedBounds::new(0.116_666_67, 0.527_777_8, 0.858_333_35, 0.105_555_56)?,
            )?;
            let phone = TextObservation::new(
                "Phone",
                1.0,
                NormalizedBounds::new(0.699_743_57, 0.780_854_7, 0.273_974_36, 0.154_700_85)?,
            )?;
            let edit = TextObservation::new(
                "Edit",
                1.0,
                NormalizedBounds::new(0.775, 0.911_111_1, 0.116_666_67, 0.066_666_67)?,
            )?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: vec![exact, one_o.clone(), phone, edit],
                selected: Vec::new(),
            };
            assert_eq!(
                menu_935_bluetooth_address_identity(&screen)?,
                "42:F3:B0:AE:1C:95"
            );
            assert_eq!(
                menu_935_bluetooth_class_identity(&screen)?,
                ("phone".to_owned(), "stock-v1.03-major-class-label-phone")
            );

            drop(screen.observations.remove(0));
            assert_eq!(
                menu_935_bluetooth_address_identity(&screen)?,
                "42:F3:B0:AE:1C:95"
            );
            screen.observations.push(TextObservation::new(
                "42:F3:B0:AE:1C:96",
                1.0,
                NormalizedBounds::new(0.0, 0.635, 0.80, 0.08)?,
            )?);
            assert!(menu_935_bluetooth_address_identity(&screen).is_err());

            let soft_key_only = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    one_o,
                    TextObservation::new(
                        "Phone",
                        1.0,
                        NormalizedBounds::new(0.70, 0.91, 0.25, 0.08)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(menu_935_bluetooth_class_identity(&soft_key_only).is_err());
            Ok(())
        }

        #[test]
        fn retained_r30_menu960_973_and_984_forms_are_page_scoped_and_one_locus() -> TestResult {
            let selected_frame = selected_frame(0..SCREEN_WIDTH, 20..44)?;
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let first_row =
                NormalizedBounds::new(0.033_333_328, 0.122_222_22, 0.475, 0.122_222_22)?;
            let alternate_first_row =
                NormalizedBounds::new(0.045_117_605, 0.122_907_765, 0.465_995_16, 0.126_114_01)?;
            let mut menu_960 = super::CapturedScreen {
                crc32: selected_frame.crc32(),
                frame: selected_frame.clone(),
                observations: vec![
                    TextObservation::new("MKey Lock", 1.0, first_row)?,
                    TextObservation::new("aKey Lock", 1.0, alternate_first_row)?,
                ],
                selected: Vec::new(),
            };
            let menu_960_entry = manifest_entry(&entries, "960")?;
            assert!(ordinary_documented_payload(menu_960_entry, &menu_960).is_some());
            assert_eq!(
                entry_value_identity(menu_960_entry, "mKey Lock extra"),
                None
            );
            assert_eq!(
                entry_value_identity(manifest_entry(&entries, "961")?, "aKey Lock"),
                None
            );
            menu_960.observations.push(TextObservation::new(
                "aKey Lock",
                1.0,
                NormalizedBounds::new(0.55, 0.122, 0.40, 0.126)?,
            )?);
            assert_eq!(ordinary_documented_payload(menu_960_entry, &menu_960), None);

            let menu_973_entry = manifest_entry(&entries, "973")?;
            let mut menu_973 = super::CapturedScreen {
                crc32: selected_frame.crc32(),
                frame: selected_frame,
                observations: vec![
                    TextObservation::new(
                        "dd \"mm. mmi",
                        1.0,
                        NormalizedBounds::new(0.0, 0.094_098_31, 0.467_740_92, 0.146_950_53)?,
                    )?,
                    TextObservation::new(
                        "dd °mm. mm'",
                        1.0,
                        NormalizedBounds::new(
                            0.018_181_302,
                            0.107_781_775,
                            0.448_308_77,
                            0.139_655_75,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(ordinary_documented_payload(menu_973_entry, &menu_973).is_some());
            assert_eq!(entry_value_identity(menu_973_entry, "dd \"mm. mm1"), None);
            assert_eq!(entry_value_identity(menu_973_entry, "dd °mm. mm"), None);
            assert_eq!(
                entry_value_identity(manifest_entry(&entries, "970")?, "dd \"mm. mmi"),
                None
            );
            menu_973.observations.push(TextObservation::new(
                "dd °mm. mm'",
                1.0,
                NormalizedBounds::new(0.52, 0.11, 0.44, 0.14)?,
            )?);
            assert_eq!(ordinary_documented_payload(menu_973_entry, &menu_973), None);

            let title_frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut menu_984 = super::CapturedScreen {
                crc32: title_frame.crc32(),
                frame: title_frame,
                observations: vec![
                    TextObservation::new(
                        "DVYDR",
                        1.0,
                        NormalizedBounds::new(
                            0.012_050_208,
                            0.025_984_721,
                            0.171_732_92,
                            0.081_363_89,
                        )?,
                    )?,
                    TextObservation::new(
                        "DV/DK",
                        1.0,
                        NormalizedBounds::new(0.0, 0.031_837_422, 0.192_051_11, 0.091_880_72)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(screen_matches_label(&menu_984, "DV/DR"));
            assert!(!retained_ui_label_alias("dvyok", "dv/dr"));
            menu_984.observations.push(TextObservation::new(
                "DV/DK",
                1.0,
                NormalizedBounds::new(0.55, 0.03, 0.19, 0.09)?,
            )?);
            assert!(!screen_matches_label(&menu_984, "DV/DR"));
            Ok(())
        }

        #[test]
        fn retained_r35_menu999_reset_alias_is_exact_page_scoped_and_one_locus() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 124..164, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let exact_bounds = NormalizedBounds::new(0.0, 0.7, 0.258_333_33, 0.122_222_22)?;
            let alias_bounds =
                NormalizedBounds::new(0.016_666_666, 0.711_111_1, 0.241_666_66, 0.097_222_22)?;
            let mut row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "999",
                        1.0,
                        NormalizedBounds::new(0.866_666_7, 0.022_222_223, 0.125, 0.1)?,
                    )?,
                    TextObservation::new("Reset", 1.0, exact_bounds)?,
                    TextObservation::new("Řeset", 1.0, alias_bounds)?,
                ],
                selected: vec!["Reset".to_owned(), "Řeset".to_owned()],
            };

            assert!(numbered_row_matches(&row, "999", "Reset"));
            assert!(!selected_matches_label(&row, "Reset"));

            row.observations
                .push(TextObservation::new("Řesett", 1.0, alias_bounds)?);
            row.selected.push("Řesett".to_owned());
            assert!(!numbered_row_matches(&row, "999", "Reset"));
            Ok(())
        }

        #[test]
        fn retained_r30_menu911_checkbox_payload_uses_all_rows_pixels_and_unique_loci() -> TestResult
        {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            let gray: u16 = (0x0F << 11) | (0x1E << 5) | 0x0F;
            for center_y in [32_usize, 56, 80] {
                for x in 7..12 {
                    set_rgb565_pixel(&mut bytes, x, center_y, gray)?;
                }
            }
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "•RX EQ",
                        1.0,
                        NormalizedBounds::new(0.025, 0.122_222_22, 0.341_666_67, 0.122_222_22)?,
                    )?,
                    TextObservation::new(
                        "DTX EQ (FM, NFM)",
                        1.0,
                        NormalizedBounds::new(0.0, 0.255_555_57, 0.758_333_3, 0.144_444_45)?,
                    )?,
                    TextObservation::new(
                        "OTX EQ(DV)",
                        1.0,
                        NormalizedBounds::new(0.0375, 0.383_333_33, 0.514_583_35, 0.113_888_89)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert_eq!(
                checkbox_payload(&screen, &["RX EQ", "TX EQ(FM, NFM)", "TX EQ(DV)"]),
                Some(vec![
                    "RX EQ=unchecked".to_owned(),
                    "TX EQ(FM, NFM)=unchecked".to_owned(),
                    "TX EQ(DV)=unchecked".to_owned(),
                ])
            );
            screen.observations.push(TextObservation::new(
                "DTX EQ (FM, NFM)",
                1.0,
                NormalizedBounds::new(0.55, 0.255, 0.44, 0.145)?,
            )?);
            assert_eq!(
                checkbox_payload(&screen, &["RX EQ", "TX EQ(FM, NFM)", "TX EQ(DV)"]),
                None
            );
            Ok(())
        }

        #[test]
        fn retained_r26_battery_level_accepts_a_bounded_shell_with_or_without_fill() -> TestResult {
            let neutral = (0x14_u16 << 11) | (0x28_u16 << 5) | 0x14_u16;
            let green = 0x32_u16 << 5;
            let build = |with_fill: bool| -> TestResult {
                let mut bytes = vec![0_u8; SCREEN_BYTES];
                fill_rgb565_rect(&mut bytes, 84..156, 27..138, neutral)?;
                if with_fill {
                    fill_rgb565_rect(&mut bytes, 93..147, 73..125, green)?;
                }
                let frame = ScreenFrame::from_rgb565_le(bytes)?;
                let screen = super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame,
                    observations: Vec::new(),
                    selected: Vec::new(),
                };
                let payload = battery_level_payload(&screen)
                    .ok_or_else(|| std::io::Error::other("battery graphic was rejected"))?;
                assert!(payload.iter().any(|item| item == "BatteryShell=72x111"));
                if with_fill {
                    assert!(payload.iter().any(|item| item == "BatteryFillColor=green"));
                    assert!(
                        payload
                            .iter()
                            .any(|item| item == "BatteryFill=54x52:2808px")
                    );
                } else {
                    assert!(payload.iter().any(|item| item == "BatteryFillColor=none"));
                    assert!(payload.iter().any(|item| item == "BatteryFill=0x0:0px"));
                }
                Ok(())
            };
            build(false)?;
            build(true)?;

            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let no_shell = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: Vec::new(),
                selected: Vec::new(),
            };
            assert_eq!(battery_level_payload(&no_shell), None);
            Ok(())
        }

        #[test]
        fn high_risk_capture_context_redacts_duplicate_or_missing_title_ocr() -> TestResult {
            let title = TextObservation::new(
                "Secret Access Code",
                1.0,
                NormalizedBounds::new(0.0, 0.0, 0.8, 0.1)?,
            )?;
            let code =
                TextObservation::new("123", 1.0, NormalizedBounds::new(0.2, 0.4, 0.2, 0.1)?)?;
            let selected = vec!["123".to_owned()];
            let (journal_selected, journal_observations, redacted) = journal_screen_text(
                Some("946"),
                &selected,
                &[title.clone(), title, code.clone()],
            )?;
            assert!(redacted);
            let encoded = serde_json::to_string(&(journal_selected, journal_observations))?;
            assert!(encoded.contains("Secret Access Code"));
            assert!(
                encoded
                    .contains("a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3")
            );
            assert!(!encoded.contains("\"123\""));

            for menu_number in ["516", "651", "935", "946"] {
                let (journal_selected, journal_observations, redacted) =
                    journal_screen_text(Some(menu_number), &selected, std::slice::from_ref(&code))?;
                assert!(
                    redacted,
                    "menu {menu_number} must fail safe without title OCR"
                );
                let encoded = serde_json::to_string(&(journal_selected, journal_observations))?;
                assert!(!encoded.contains("\"123\""));
            }

            let (journal_selected, journal_observations, redacted) =
                journal_screen_text(Some("950"), &selected, std::slice::from_ref(&code))?;
            assert!(!redacted);
            let encoded = serde_json::to_string(&(journal_selected, journal_observations))?;
            assert!(encoded.contains("\"123\""));
            Ok(())
        }

        #[test]
        fn label_matches_require_confident_unique_punctuation_preserving_equality() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..200, 20..40, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let selected_bounds = NormalizedBounds::new(0.0, 20.0 / 180.0, 0.5, 20.0 / 180.0)?;
            let selected_observation =
                TextObservation::new("USB Audio Out. Lvl.", 1.0, selected_bounds)?;
            let title_observation = TextObservation::new(
                "USB Audio Out. Lvl.",
                1.0,
                NormalizedBounds::new(0.0, 1.0 / 180.0, 0.6, 18.0 / 180.0)?,
            )?;
            let bands = v103_selection_bands(&frame);
            let observations = vec![title_observation.clone(), selected_observation.clone()];
            let selected = kenwood_thd75::screen::ui::selected_text(&observations, &bands)
                .into_iter()
                .map(|value| value.text().to_owned())
                .collect();
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations,
                selected,
            };
            assert!(selected_matches_label(&screen, "usb audio out. lvl."));
            assert!(!selected_matches_label(&screen, "USB Audio Out Lvl"));
            assert!(!selected_matches_label(&screen, "USB Audio Out. Lvl.!"));
            assert!(!selected_matches_label(&screen, "Reset"));
            assert!(screen_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(!screen_matches_label(&screen, "USB Audio Out Lvl"));

            let slash_title = TextObservation::new(
                "MW/SW Antenna",
                1.0,
                NormalizedBounds::new(0.0, 1.0 / 180.0, 0.6, 18.0 / 180.0)?,
            )?;
            let slash_screen = super::CapturedScreen {
                crc32: screen.crc32,
                frame: screen.frame.clone(),
                observations: vec![slash_title],
                selected: Vec::new(),
            };
            assert!(screen_matches_label(&slash_screen, "MW/ SW Antenna"));
            assert!(!screen_matches_label(&slash_screen, "MW-SW Antenna"));

            screen
                .observations
                .push(TextObservation::new("Reset", 1.0, selected_bounds)?);
            assert!(!selected_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(screen.observations.pop().is_some());
            screen.observations.push(TextObservation::new(
                "Different Title",
                1.0,
                NormalizedBounds::new(0.0, 1.0 / 180.0, 0.6, 18.0 / 180.0)?,
            )?);
            assert!(screen_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(screen.observations.pop().is_some());
            screen.observations.push(TextObservation::new(
                "Different Title",
                1.0,
                NormalizedBounds::new(0.65, 1.0 / 180.0, 0.2, 18.0 / 180.0)?,
            )?);
            assert!(!screen_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(screen.observations.pop().is_some());

            let locator_bounds =
                NormalizedBounds::new(208.0 / 240.0, 4.0 / 180.0, 30.0 / 240.0, 18.0 / 180.0)?;
            screen
                .observations
                .push(TextObservation::new("91A", 1.0, locator_bounds)?);
            screen.observations.push(TextObservation::new(
                "919",
                1.0,
                NormalizedBounds::new(210.0 / 240.0, 5.0 / 180.0, 28.0 / 240.0, 17.0 / 180.0)?,
            )?);
            assert!(selected_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(screen_has_exact_menu_locator(&screen, "91A"));
            assert!(numbered_row_matches(&screen, "91A", "USB Audio Out. Lvl."));
            assert!(!screen_has_exact_menu_locator(&screen, "920"));
            screen.observations.push(TextObservation::new(
                "920",
                1.0,
                NormalizedBounds::new(0.72, 4.0 / 180.0, 0.10, 18.0 / 180.0)?,
            )?);
            assert!(!screen_has_exact_menu_locator(&screen, "91A"));
            assert!(screen.observations.pop().is_some());
            assert!(screen.observations.pop().is_some());
            assert!(screen.observations.pop().is_some());

            screen.observations.push(selected_observation);
            assert!(selected_matches_label(&screen, "USB Audio Out. Lvl."));
            screen.observations.push(TextObservation::new(
                "USB Audio Out. Lvl.",
                1.0,
                NormalizedBounds::new(0.60, 20.0 / 180.0, 0.35, 20.0 / 180.0)?,
            )?);
            assert!(!selected_matches_label(&screen, "USB Audio Out. Lvl."));
            assert!(screen.observations.pop().is_some());
            screen.observations.push(title_observation);
            assert!(screen_matches_label(&screen, "USB Audio Out. Lvl."));
            Ok(())
        }

        #[test]
        fn retained_v103_checkbox_and_value_ocr_forms_resolve_to_one_physical_locus() -> TestResult
        {
            let frame = selected_frame(0..200, 20..44)?;
            let row_zero = NormalizedBounds::new(0.03, 20.0 / 180.0, 0.45, 24.0 / 180.0)?;
            let row_one = NormalizedBounds::new(0.03, 44.0 / 180.0, 0.45, 24.0 / 180.0)?;
            let weather_alias = NormalizedBounds::new(0.04, 22.0 / 180.0, 0.43, 20.0 / 180.0)?;
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new("RY", 1.0, row_zero)?,
                    TextObservation::new("OРТT", 1.0, row_one)?,
                    TextObservation::new("Weather", 1.0, row_zero)?,
                    TextObservation::new("Cweather", 1.0, weather_alias)?,
                ],
                selected: Vec::new(),
            };
            assert!(checkbox_row_has_unique_label(&screen, 0, "RX"));
            assert!(checkbox_row_has_unique_label(&screen, 1, "PTT"));
            let weather_frame = screen.frame;
            let weather_screen = super::CapturedScreen {
                crc32: weather_frame.crc32(),
                frame: weather_frame,
                observations: vec![
                    TextObservation::new("Weather", 1.0, row_zero)?,
                    TextObservation::new("Cweather", 1.0, weather_alias)?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&weather_screen, "Weather"));

            let object_alias = NormalizedBounds::new(0.047, 21.0 / 180.0, 0.62, 22.0 / 180.0)?;
            let mut object_screen = super::CapturedScreen {
                crc32: weather_screen.crc32,
                frame: weather_screen.frame,
                observations: vec![
                    TextObservation::new("2Object/Item", 1.0, object_alias)?,
                    TextObservation::new("MObject/Item", 1.0, row_zero)?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&object_screen, "Object/Item"));
            object_screen.observations.push(TextObservation::new(
                "2Object/Items",
                1.0,
                object_alias,
            )?);
            assert!(!selected_matches_label(&object_screen, "Object/Item"));

            let gpgsa_frame = selected_frame(0..200, 68..92)?;
            let merged_gpgsa_bounds =
                NormalizedBounds::new(0.066_523_16, 0.377_088_81, 0.358_620_35, 0.134_711_24)?;
            let exact_gpgsa_bounds =
                NormalizedBounds::new(0.114_583_34, 0.388_888_9, 0.302_083_34, 0.116_666_67)?;
            let mut gpgsa_screen = super::CapturedScreen {
                crc32: gpgsa_frame.crc32(),
                frame: gpgsa_frame,
                observations: vec![
                    TextObservation::new("I $GPGSA", 1.0, merged_gpgsa_bounds)?,
                    TextObservation::new("$GPGSA", 1.0, exact_gpgsa_bounds)?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&gpgsa_screen, "$GPGSA"));
            gpgsa_screen.observations.push(TextObservation::new(
                "I $GPGSB",
                1.0,
                merged_gpgsa_bounds,
            )?);
            assert!(!selected_matches_label(&gpgsa_screen, "$GPGSA"));

            let row_frame = selected_frame(0..SCREEN_WIDTH, 44..84)?;
            let numbered_row = super::CapturedScreen {
                crc32: row_frame.crc32(),
                frame: row_frame,
                observations: vec![
                    TextObservation::new(
                        "Programmable VFO",
                        1.0,
                        NormalizedBounds::new(0.008, 0.255, 0.810, 0.137)?,
                    )?,
                    TextObservation::new(
                        "136 - 173 WHz",
                        1.0,
                        NormalizedBounds::new(0.454, 0.378, 0.502, 0.089)?,
                    )?,
                    TextObservation::new(
                        "136 - 173 MHz",
                        1.0,
                        NormalizedBounds::new(0.467, 0.378, 0.492, 0.100)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&numbered_row, "Programmable VFO"));
            Ok(())
        }

        #[test]
        fn retained_r40_menu_631_gpvtg_vision_form_is_page_scoped() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..232, 140..164, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let exact_bounds =
                NormalizedBounds::new(0.091_448_15, 0.776_800_1, 0.333_770_36, 0.135_288_75)?;
            let merged_bounds =
                NormalizedBounds::new(0.114_504_606, 0.788_497_3, 0.302_240_8, 0.109_116_46)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new("$GPVTG", 1.0, exact_bounds)?,
                    TextObservation::new("SGPYTG", 1.0, merged_bounds)?,
                ],
                selected: vec!["$GPVTG".to_owned(), "SGPYTG".to_owned()],
            };
            assert!(selected_matches_label_for_menu(
                &screen,
                Some("631"),
                "$GPVTG"
            ));
            assert!(!selected_matches_label_for_menu(
                &screen,
                Some("406"),
                "$GPVTG"
            ));
            assert!(!selected_matches_label(&screen, "$GPVTG"));

            screen
                .observations
                .push(TextObservation::new("SGPYTB", 1.0, merged_bounds)?);
            assert!(!selected_matches_label_for_menu(
                &screen,
                Some("631"),
                "$GPVTG"
            ));
            Ok(())
        }

        #[test]
        fn retained_r40_menu_710_uses_only_the_exact_singleton_submenu_locator() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let menu_710 = manifest_entry(&entries, "710")?;
            assert!(menu_710_is_exact_reviewed_singleton(&entries, menu_710));

            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..232, 44..68, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let memory_bounds =
                NormalizedBounds::new(0.0, 0.257_669_48, 0.317_925_75, 0.128_735_51)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "FM Broadcasting",
                        1.0,
                        NormalizedBounds::new(0.0, 0.016_836_98, 0.526_526_75, 0.117_080_29)?,
                    )?,
                    TextObservation::new(
                        "71-",
                        1.0,
                        NormalizedBounds::new(0.875, 0.022_222_22, 0.116_666_67, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "FV Broadcasting",
                        1.0,
                        NormalizedBounds::new(
                            0.016_038_388,
                            0.022_704_232,
                            0.501_332_64,
                            0.086_125_51,
                        )?,
                    )?,
                    TextObservation::new(
                        "Basic Settings",
                        1.0,
                        NormalizedBounds::new(
                            0.007_293_480_4,
                            0.112_043_284,
                            0.710_615_5,
                            0.145_361_63,
                        )?,
                    )?,
                    TextObservation::new("Memory", 1.0, memory_bounds)?,
                    TextObservation::new(
                        "Back",
                        1.0,
                        NormalizedBounds::new(0.075, 0.911_111_1, 0.166_666_67, 0.077_777_78)?,
                    )?,
                    TextObservation::new(
                        "OK",
                        1.0,
                        NormalizedBounds::new(0.775, 0.911_111_1, 0.108_333_334, 0.077_777_78)?,
                    )?,
                    TextObservation::new(
                        "Ok",
                        1.0,
                        NormalizedBounds::new(0.793_75, 0.913_888_9, 0.075, 0.058_333_334)?,
                    )?,
                    TextObservation::new(
                        "Baсk",
                        1.0,
                        NormalizedBounds::new(
                            0.097_614_83,
                            0.919_010_4,
                            0.134_085_03,
                            0.057_241_995,
                        )?,
                    )?,
                ],
                selected: vec!["Memory".to_owned()],
            };
            assert!(menu_710_singleton_memory_submenu_matches(&screen));

            let exact_frame = screen.frame.clone();
            let mut shifted_bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut shifted_bytes, 0..232, 45..69, V103_SELECTION_RGB565)?;
            screen.frame = ScreenFrame::from_rgb565_le(shifted_bytes)?;
            assert!(!menu_710_singleton_memory_submenu_matches(&screen));
            screen.frame = exact_frame;
            replace_observation(
                &mut screen.observations,
                4,
                TextObservation::new("Memories", 1.0, memory_bounds)?,
            )?;
            assert!(!menu_710_singleton_memory_submenu_matches(&screen));
            replace_observation(
                &mut screen.observations,
                4,
                TextObservation::new("Memory", 1.0, memory_bounds)?,
            )?;
            screen.observations.push(TextObservation::new(
                "Memory",
                1.0,
                NormalizedBounds::new(0.55, 46.0 / 180.0, 0.30, 20.0 / 180.0)?,
            )?);
            assert!(!menu_710_singleton_memory_submenu_matches(&screen));
            Ok(())
        }

        #[test]
        fn retained_r17_row_ocr_alternatives_are_one_locus_not_competing_labels() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 124..164, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut wx_row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: vec![
                    TextObservation::new(
                        "WX Alert",
                        1.0,
                        NormalizedBounds::new(0.0, 0.700_225_9, 0.409_501_6, 0.119_193_05)?,
                    )?,
                    TextObservation::new(
                        "*X Alert",
                        1.0,
                        NormalizedBounds::new(
                            0.022_641_15,
                            0.706_409_9,
                            0.385_967_7,
                            0.103_846_95,
                        )?,
                    )?,
                    TextObservation::new(
                        "Off",
                        1.0,
                        NormalizedBounds::new(0.833_333_3, 0.822_222_23, 0.125, 0.088_888_89)?,
                    )?,
                ],
                selected: vec![
                    "WX Alert".to_owned(),
                    "*X Alert".to_owned(),
                    "Off".to_owned(),
                ],
            };
            assert!(selected_matches_label(&wx_row, "WX Alert"));
            wx_row.observations.push(TextObservation::new(
                "Different",
                1.0,
                NormalizedBounds::new(0.50, 0.70, 0.35, 0.10)?,
            )?);
            assert!(!selected_matches_label(&wx_row, "WX Alert"));

            let reverse_row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Reverse",
                        1.0,
                        NormalizedBounds::new(0.0, 0.698_887_47, 0.366_873_44, 0.124_446_84)?,
                    )?,
                    TextObservation::new(
                        "Řeverse",
                        1.0,
                        NormalizedBounds::new(
                            0.016_513_012,
                            0.710_130_45,
                            0.346_140_65,
                            0.099_184_416,
                        )?,
                    )?,
                    TextObservation::new(
                        "Normal",
                        1.0,
                        NormalizedBounds::new(
                            0.738_183_14,
                            0.819_700_9,
                            0.225_616_86,
                            0.095_833_33,
                        )?,
                    )?,
                ],
                selected: vec![
                    "Reverse".to_owned(),
                    "Řeverse".to_owned(),
                    "Normal".to_owned(),
                ],
            };
            assert!(selected_matches_label(&reverse_row, "Reverse"));
            Ok(())
        }

        #[test]
        fn retained_r17_exact_title_fragments_require_one_physical_title_locus() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let cases = [
                (
                    "Mic. Sensitivity",
                    ("Mic.", 0.0, 0.016_260_177, 0.139_532_5, 0.113_143_615),
                    (
                        "Sensitivity",
                        0.157_266_07,
                        0.014_160_972,
                        0.393_801_2,
                        0.116_122_5,
                    ),
                    (
                        "Hic. Sensitivity",
                        0.014_583_332,
                        0.030_555_556,
                        0.531_25,
                        0.072_222_225,
                    ),
                ),
                (
                    "Auto Weather Scan",
                    ("Auto", 0.0, 0.022_222_223, 0.15, 0.10),
                    ("Weather Scan", 0.166_666_67, 0.022_222_223, 0.425, 0.10),
                    (
                        "auto teather Scan",
                        0.014_583_332,
                        0.027_777_778,
                        0.570_833_3,
                        0.075,
                    ),
                ),
                (
                    "QSO Log",
                    ("QSO", 0.0, 0.022_222_223, 0.116_666_67, 0.10),
                    ("Log", 0.133_333_33, 0.022_222_22, 0.125, 0.10),
                    (
                        "OSO Log",
                        0.009_713_409,
                        0.025_959_017,
                        0.241_226_45,
                        0.080_476_38,
                    ),
                ),
            ];
            for (expected, left, right, merged_alias) in cases {
                let mut observations = Vec::new();
                for (text, x, y, width, height) in [left, right, merged_alias] {
                    let bounds = NormalizedBounds::new(x, y, width, height)?;
                    observations.push(TextObservation::new(text, 1.0, bounds)?);
                }
                let mut screen = super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame: frame.clone(),
                    observations,
                    selected: Vec::new(),
                };
                assert!(screen_matches_label(&screen, expected));
                screen.observations.push(TextObservation::new(
                    "Different",
                    1.0,
                    NormalizedBounds::new(0.60, 0.02, 0.25, 0.10)?,
                )?);
                assert!(!screen_matches_label(&screen, expected));
            }

            let gain = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![TextObservation::new(
                    "Gaın",
                    1.0,
                    NormalizedBounds::new(
                        0.013_620_692,
                        0.027_892_718,
                        0.137_356_31,
                        0.080_095_79,
                    )?,
                )?],
                selected: Vec::new(),
            };
            assert!(screen_matches_label(&gain, "Gain"));
            Ok(())
        }

        #[test]
        fn retained_r26_menu921_accepts_only_its_exact_scoped_title_fragments() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "АРО=",
                        1.0,
                        NormalizedBounds::new(0.0, 0.022_222_223, 0.15, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "Auto Power Off",
                        1.0,
                        NormalizedBounds::new(0.175, 0.022_222_223, 0.483_333_32, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "АРО: Auto Pожer Off",
                        1.0,
                        NormalizedBounds::new(
                            0.016_666_666,
                            0.027_777_778,
                            0.635_416_7,
                            0.077_777_78,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(screen_matches_label(&screen, "APO: Auto Power Off"));
            assert!(!screen_matches_label(&screen, "APO: Auto Power On"));
            screen.observations.push(TextObservation::new(
                "Different",
                1.0,
                NormalizedBounds::new(0.65, 0.02, 0.20, 0.09)?,
            )?);
            assert!(!screen_matches_label(&screen, "APO: Auto Power Off"));
            Ok(())
        }

        #[test]
        fn retained_r17_narrow_row_aliases_do_not_become_global_fuzzy_matching() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 84..124, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let cw = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: vec![
                    TextObservation::new(
                        "C# Width",
                        1.0,
                        NormalizedBounds::new(
                            0.014_364_989,
                            0.468_137_74,
                            0.398_353_37,
                            0.122_057_84,
                        )?,
                    )?,
                    TextObservation::new(
                        "CH Hidth",
                        1.0,
                        NormalizedBounds::new(0.007_411_215, 0.482_951, 0.410_002_9, 0.121_658_65)?,
                    )?,
                    TextObservation::new(
                        "1.0 kHz",
                        1.0,
                        NormalizedBounds::new(
                            0.707_816_8,
                            0.597_653_8,
                            0.251_033_16,
                            0.104_692_39,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&cw, "CW Width"));
            assert!(!selected_matches_label(&cw, "CH Width"));

            let delay = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "De lay",
                        1.0,
                        NormalizedBounds::new(
                            0.003_435_886,
                            0.474_085_48,
                            0.267_205_15,
                            0.140_649_57,
                        )?,
                    )?,
                    TextObservation::new(
                        "500",
                        1.0,
                        NormalizedBounds::new(0.70, 0.60, 0.125, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "ms",
                        1.0,
                        NormalizedBounds::new(
                            0.866_666_7,
                            0.611_111_1,
                            0.091_666_67,
                            0.088_888_89,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&delay, "Delay"));
            assert!(!selected_matches_label(&delay, "De Lay Timer"));
            Ok(())
        }

        #[test]
        fn retained_r20_turn_time_and_mobile_alternatives_share_one_exact_locus() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 124..164, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let turn_time = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "lurn Time",
                        1.0,
                        NormalizedBounds::new(
                            0.007_342_386,
                            0.703_312_4,
                            0.460_315_23,
                            0.126_708_58,
                        )?,
                    )?,
                    TextObservation::new(
                        "Turn Time",
                        1.0,
                        NormalizedBounds::new(
                            0.016_666_666,
                            0.705_555_56,
                            0.445_833_33,
                            0.108_333_334,
                        )?,
                    )?,
                    TextObservation::new(
                        "60 sec",
                        1.0,
                        NormalizedBounds::new(
                            0.741_015_4,
                            0.825_972_6,
                            0.218_006_94,
                            0.080_332_26,
                        )?,
                    )?,
                ],
                selected: vec![
                    "lurn Time".to_owned(),
                    "Turn Time".to_owned(),
                    "60 sec".to_owned(),
                ],
            };
            assert!(selected_matches_label(&turn_time, "Turn Time"));
            assert!(!selected_matches_label(&turn_time, "Lure Time"));

            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mobile = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Mobile",
                        1.0,
                        NormalizedBounds::new(0.110_105_01, 0.384_613_2, 0.304_79, 0.116_884_75)?,
                    )?,
                    TextObservation::new(
                        "aMobile",
                        1.0,
                        NormalizedBounds::new(
                            0.041_224_144,
                            0.397_281_5,
                            0.375_885_04,
                            0.116_548_136,
                        )?,
                    )?,
                ],
                selected: vec!["Mobile".to_owned(), "aMobile".to_owned()],
            };
            assert!(checkbox_row_has_unique_label(&mobile, 2, "Mobile"));
            assert!(!checkbox_row_has_unique_label(&mobile, 2, "Automobile"));
            Ok(())
        }

        #[test]
        fn retained_r21_digipeat_and_uicheck_ocr_forms_are_narrowly_accepted() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 44..84, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Digipeat (MyCalI)",
                        1.0,
                        NormalizedBounds::new(0.0, 0.251_081_4, 0.801_312_15, 0.148_740_41)?,
                    )?,
                    TextObservation::new(
                        "Off",
                        1.0,
                        NormalizedBounds::new(0.833_333_3, 0.377_777_79, 0.125, 0.088_888_89)?,
                    )?,
                ],
                selected: vec!["Digipeat (MyCalI)".to_owned(), "Off".to_owned()],
            };
            assert!(selected_matches_label(&row, "Digipeat(MyCall)"));
            assert!(!selected_matches_label(&row, "Digipeat(MyCalls)"));

            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let page = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Ulcheck",
                        1.0,
                        NormalizedBounds::new(0.0, 0.019_185_802, 0.258_976, 0.106_072_836)?,
                    )?,
                    TextObservation::new(
                        "U check",
                        1.0,
                        NormalizedBounds::new(0.0, 0.034_121_8, 0.249_109_25, 0.073_235_03)?,
                    )?,
                    TextObservation::new(
                        "28 sec",
                        1.0,
                        NormalizedBounds::new(
                            0.358_333_32,
                            0.388_888_9,
                            0.308_333_34,
                            0.122_222_22,
                        )?,
                    )?,
                    TextObservation::new(
                        "Back",
                        1.0,
                        NormalizedBounds::new(0.066_666_66, 0.9, 0.175, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "OK",
                        1.0,
                        NormalizedBounds::new(
                            0.758_333_3,
                            0.911_111_1,
                            0.133_333_34,
                            0.077_777_78,
                        )?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(screen_matches_label(&page, "UIcheck"));
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            assert!(
                centered_scalar_documented_payload(manifest_entry(&entries, "581")?, &page)
                    .is_some()
            );
            Ok(())
        }

        #[test]
        fn retained_r22_ui_prefix_aliases_are_exact_and_same_locus_only() -> TestResult {
            for (expected, observed) in [
                ("uidigipeat", "uldigipeat"),
                ("uiflood", "ulflood"),
                ("uiflood", "vlflood"),
                ("uiflood alias", "ulflood alias"),
                ("uifloodsubstitution", "ulf loodsubstitution"),
                ("uifloodsubstitution", "ulfloodsubstitution"),
                ("uitrace", "ultrace"),
                ("uitrace alias", "ultrace alias"),
                ("uidigi aliases", "uldigi aliases"),
                ("$gpgll", "sgpigll"),
                ("tx/rx eq", "txyrx eq"),
            ] {
                assert!(retained_ui_label_alias(observed, expected));
            }
            assert!(!retained_ui_label_alias("ultrace extra", "uitrace"));
            assert!(!retained_ui_label_alias("ulflood", "uidigipeat"));
            assert!(!retained_ui_label_alias("j$gpgll", "$gpgll"));

            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut page = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Ulflood",
                        1.0,
                        NormalizedBounds::new(0.0, 0.02, 0.30, 0.11)?,
                    )?,
                    TextObservation::new(
                        "Vlflood",
                        1.0,
                        NormalizedBounds::new(0.0, 0.025, 0.30, 0.10)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(screen_matches_label(&page, "UIflood"));
            page.observations.push(TextObservation::new(
                "Different",
                1.0,
                NormalizedBounds::new(0.40, 0.02, 0.25, 0.10)?,
            )?);
            assert!(!screen_matches_label(&page, "UIflood"));
            Ok(())
        }

        #[test]
        fn retained_r23_row_aliases_require_the_exact_observed_form_and_one_locus() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 44..84, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let alias_bounds = NormalizedBounds::new(0.0, 46.0 / 180.0, 0.72, 20.0 / 180.0)?;
            let mut row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new("Uldigi Aliases", 1.0, alias_bounds)?,
                    TextObservation::new(
                        "WIDE1-1, WIDE2-1",
                        1.0,
                        NormalizedBounds::new(0.43, 68.0 / 180.0, 0.52, 16.0 / 180.0)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&row, "UIdigi Aliases"));
            row.observations.push(TextObservation::new(
                "Uldigi Aliases",
                1.0,
                NormalizedBounds::new(0.30, 46.0 / 180.0, 0.65, 20.0 / 180.0)?,
            )?);
            assert!(!selected_matches_label(&row, "UIdigi Aliases"));

            let mut tx_rx_eq = super::CapturedScreen {
                crc32: row.crc32,
                frame: row.frame,
                observations: vec![TextObservation::new("TXYRX EQ", 1.0, alias_bounds)?],
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&tx_rx_eq, "TX/RX EQ"));
            tx_rx_eq.observations.push(TextObservation::new(
                "TXYRX EQ",
                1.0,
                NormalizedBounds::new(0.30, 46.0 / 180.0, 0.65, 20.0 / 180.0)?,
            )?);
            assert!(!selected_matches_label(&tx_rx_eq, "TX/RX EQ"));

            let live_menu_911_tx_fm = TextObservation::new(
                "DTX EQ (FM, NFM)",
                1.0,
                NormalizedBounds::new(0.0, 0.255_555_57, 0.758_333_3, 0.144_444_45)?,
            )?;
            assert!(checkbox_row_label_matches(
                &live_menu_911_tx_fm,
                1,
                "tx eq(fm, nfm)"
            ));
            assert!(!checkbox_row_label_matches(
                &live_menu_911_tx_fm,
                0,
                "tx eq(fm, nfm)"
            ));
            assert!(!checkbox_row_label_matches(
                &live_menu_911_tx_fm,
                1,
                "tx eq(dv)"
            ));

            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut checkbox_page = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![TextObservation::new(
                    "SGPIGLL",
                    1.0,
                    NormalizedBounds::new(0.05, 46.0 / 180.0, 0.36, 20.0 / 180.0)?,
                )?],
                selected: Vec::new(),
            };
            assert!(checkbox_row_has_unique_label(&checkbox_page, 1, "$GPGLL"));
            checkbox_page.observations.push(TextObservation::new(
                "SGPIGLL",
                1.0,
                NormalizedBounds::new(0.50, 46.0 / 180.0, 0.36, 20.0 / 180.0)?,
            )?);
            assert!(!checkbox_row_has_unique_label(&checkbox_page, 1, "$GPGLL"));
            Ok(())
        }

        #[test]
        fn retained_r23_40_pixel_rows_accept_only_exact_ordered_upper_lane_fragments() -> TestResult
        {
            let frame = selected_frame(0..SCREEN_WIDTH, 44..84)?;
            let frame = check_fm_auto_row(frame)?;
            let _frame = check_display_hold_row(frame)?;
            check_usb_audio_level_row()?;
            Ok(())
        }

        #[test]
        fn retained_r35_menu701_accepts_only_the_exact_merged_value_fragment() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 84..124, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let merged_bounds =
                NormalizedBounds::new(0.011_052_416, 0.399_691_64, 0.764_512_8, 0.289_719_8)?;
            let left_bounds =
                NormalizedBounds::new(0.007_238_44, 0.467_255_2, 0.702_393_2, 0.144_879_12)?;
            let right_bounds =
                NormalizedBounds::new(0.75, 0.488_888_9, 0.216_666_67, 0.111_111_11)?;
            let lower_sec_bounds =
                NormalizedBounds::new(0.841_666_64, 0.622_222_24, 0.116_666_67, 0.066_666_67)?;
            let lower_sec_alias_bounds =
                NormalizedBounds::new(0.829_166_65, 0.627_777_76, 0.118_75, 0.052_777_78)?;
            let exact_fragments_and_value = vec![
                TextObservation::new("Auto Mute RET.", 1.0, left_bounds)?,
                TextObservation::new("Time", 1.0, right_bounds)?,
                TextObservation::new("sec", 1.0, lower_sec_bounds)?,
                TextObservation::new("Sec", 0.3, lower_sec_alias_bounds)?,
            ];
            let menu_locator = TextObservation::new(
                "701",
                1.0,
                NormalizedBounds::new(0.866_666_7, 0.022_222_22, 0.116_666_67, 0.088_888_89)?,
            )?;
            let mut live_observations = vec![
                menu_locator.clone(),
                TextObservation::new("Auto Mute RET-3", 1.0, merged_bounds)?,
            ];
            live_observations.extend(exact_fragments_and_value.clone());
            let live = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: live_observations,
                selected: Vec::new(),
            };
            assert!(selected_matches_label(&live, "Auto Mute RET. Time"));
            assert!(numbered_row_matches(&live, "701", "Auto Mute RET. Time"));

            let merged_only = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: vec![TextObservation::new("Auto Mute RET-3", 1.0, merged_bounds)?],
                selected: Vec::new(),
            };
            assert!(!selected_matches_label(&merged_only, "Auto Mute RET. Time"));

            let mut near_miss_observations = vec![
                menu_locator,
                TextObservation::new("Auto Mute RET-4", 1.0, merged_bounds)?,
            ];
            near_miss_observations.extend(exact_fragments_and_value);
            let near_miss = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: near_miss_observations,
                selected: Vec::new(),
            };
            assert!(!selected_matches_label(&near_miss, "Auto Mute RET. Time"));
            assert!(!numbered_row_matches(
                &near_miss,
                "701",
                "Auto Mute RET. Time"
            ));
            Ok(())
        }

        #[test]
        fn aligned_use_marker_ignores_bottom_soft_key_and_rejects_two_body_markers() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "APRS",
                        1.0,
                        NormalizedBounds::new(0.05, 20.0 / 180.0, 0.25, 20.0 / 180.0)?,
                    )?,
                    TextObservation::new(
                        "USE",
                        1.0,
                        NormalizedBounds::new(0.85, 20.0 / 180.0, 0.12, 20.0 / 180.0)?,
                    )?,
                    TextObservation::new(
                        "Use",
                        1.0,
                        NormalizedBounds::new(0.77, 164.0 / 180.0, 0.13, 14.0 / 180.0)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(aligned_use_marker(&screen, "APRS").is_some());
            screen.observations.push(TextObservation::new(
                "APRS",
                1.0,
                NormalizedBounds::new(0.05, 60.0 / 180.0, 0.25, 20.0 / 180.0)?,
            )?);
            assert!(aligned_use_marker(&screen, "APRS").is_some());
            screen.observations.push(TextObservation::new(
                "USE",
                1.0,
                NormalizedBounds::new(0.85, 60.0 / 180.0, 0.12, 20.0 / 180.0)?,
            )?);
            assert_eq!(aligned_use_marker(&screen, "APRS"), None);
            Ok(())
        }

        #[test]
        fn retained_r19_menu312_is_located_as_a_multi_record_page_without_entry() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let entry = manifest_entry(&entries, "312")?;
            assert_eq!(entry.class, AuditClass::RowOnly);
            assert_eq!(row_only_policy("312")?, RowOnlyPolicy::MultiRecordEditor);
            assert_eq!(row_only_anchor("312")?, "311");
            assert!(!entry_has_typed_value_oracle(entry));

            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 124..164, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "312",
                        1.0,
                        NormalizedBounds::new(0.866_666_7, 0.022_222_223, 0.125, 0.10)?,
                    )?,
                    TextObservation::new(
                        "Digital Auto Reply",
                        1.0,
                        NormalizedBounds::new(0.0, 0.699_193_54, 0.917_528_3, 0.145_248_52)?,
                    )?,
                ],
                selected: vec!["Digital Auto Reply".to_owned()],
            };
            assert!(numbered_row_matches(&row, "312", "Digital Auto Reply"));
            assert!(safe_inspection_oracle("312").is_err());
            Ok(())
        }

        #[test]
        fn retained_r19_menu404_accepts_only_its_split_selected_duration() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let entry = manifest_entry(&entries, "404")?;
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 116..140, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "8",
                        1.0,
                        NormalizedBounds::new(
                            0.020_833_334,
                            0.666_666_7,
                            0.039_583_333,
                            0.086_111_11,
                        )?,
                    )?,
                    TextObservation::new(
                        "min",
                        1.0,
                        NormalizedBounds::new(
                            0.258_333_33,
                            0.666_666_7,
                            0.158_333_33,
                            0.111_111_11,
                        )?,
                    )?,
                ],
                selected: vec!["8".to_owned(), "min".to_owned()],
            };
            let payload = ordinary_documented_payload(entry, &screen)
                .ok_or_else(|| std::io::Error::other("Menu 404 split value was rejected"))?;
            assert!(
                payload
                    .iter()
                    .any(|value| value == "DocumentedDomain=discrete:8")
            );

            screen.observations.push(TextObservation::new(
                "16",
                1.0,
                NormalizedBounds::new(0.50, 0.666_666_7, 0.10, 0.10)?,
            )?);
            assert_eq!(ordinary_documented_payload(entry, &screen), None);
            Ok(())
        }

        #[test]
        fn retained_r19_menu501_row_accepts_only_the_overlapping_icon_ocr_alternative() -> TestResult
        {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 44..84, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "501",
                        1.0,
                        NormalizedBounds::new(0.866_666_7, 0.022_222_22, 0.116_666_67, 0.10)?,
                    )?,
                    TextObservation::new(
                        "rcon",
                        1.0,
                        NormalizedBounds::new(
                            0.021_566_095,
                            0.269_674_33,
                            0.194_123_57,
                            0.109_003_84,
                        )?,
                    )?,
                    TextObservation::new(
                        "Icon",
                        1.0,
                        NormalizedBounds::new(
                            0.033_333_33,
                            0.288_888_9,
                            0.183_333_34,
                            0.088_888_89,
                        )?,
                    )?,
                    TextObservation::new(
                        "Digipeater",
                        1.0,
                        NormalizedBounds::new(
                            0.607_679_55,
                            0.373_069_52,
                            0.342_974_2,
                            0.098_305_404,
                        )?,
                    )?,
                ],
                selected: vec![
                    "rcon".to_owned(),
                    "Icon".to_owned(),
                    "Digipeater".to_owned(),
                ],
            };
            assert!(numbered_row_matches(&row, "501", "Icon"));
            row.observations.push(TextObservation::new(
                "Icom",
                1.0,
                NormalizedBounds::new(0.30, 0.288_888_9, 0.18, 0.088_888_89)?,
            )?);
            assert!(!selected_matches_label(&row, "Icon"));
            Ok(())
        }

        #[test]
        fn retained_r19_active_choice_labels_match_exact_stock_503_and_504_rendering() -> TestResult
        {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let status = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: vec![
                    TextObservation::new(
                        "Status Text1",
                        1.0,
                        NormalizedBounds::new(0.008_333_327, 0.122_222_22, 0.6, 0.122_222_22)?,
                    )?,
                    TextObservation::new(
                        "USE",
                        1.0,
                        NormalizedBounds::new(
                            0.858_333_35,
                            0.144_444_45,
                            0.108_333_334,
                            0.077_777_78,
                        )?,
                    )?,
                    TextObservation::new(
                        "Use",
                        1.0,
                        NormalizedBounds::new(0.766_666_65, 0.911_111_1, 0.125, 0.077_777_78)?,
                    )?,
                ],
                selected: vec!["Status Text1".to_owned(), "USE".to_owned()],
            };
            let SafeInspectionOracle::ActiveChoice { labels, .. } = safe_inspection_oracle("503")?
            else {
                return Err("Menu 503 lost its active-choice oracle".into());
            };
            let label = labels
                .first()
                .ok_or("Menu 503 active-choice oracle has no labels")?;
            assert_eq!(*label, "Status Text1");
            assert!(aligned_use_marker(&status, label).is_some());
            assert_eq!(aligned_use_marker(&status, "Status Text 1"), None);

            let packet = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Type: New-N",
                        1.0,
                        NormalizedBounds::new(
                            0.016_471_084,
                            0.120_783_45,
                            0.500_391_2,
                            0.125_099_76,
                        )?,
                    )?,
                    TextObservation::new(
                        "Тype: New-N",
                        1.0,
                        NormalizedBounds::new(
                            0.011_929_496,
                            0.124_443_516,
                            0.499_119_97,
                            0.119_500_704,
                        )?,
                    )?,
                    TextObservation::new(
                        "USE",
                        1.0,
                        NormalizedBounds::new(0.866_666_7, 0.144_444_45, 0.10, 0.066_666_67)?,
                    )?,
                    TextObservation::new(
                        "Use",
                        1.0,
                        NormalizedBounds::new(0.766_666_65, 0.911_111_1, 0.125, 0.066_666_67)?,
                    )?,
                ],
                selected: vec!["Type: New-N".to_owned(), "USE".to_owned()],
            };
            let SafeInspectionOracle::ActiveChoice { labels, .. } = safe_inspection_oracle("504")?
            else {
                return Err("Menu 504 lost its active-choice oracle".into());
            };
            let label = labels
                .first()
                .ok_or("Menu 504 active-choice oracle has no labels")?;
            assert_eq!(*label, "Type: New-N");
            assert!(aligned_use_marker(&packet, label).is_some());
            assert_eq!(aligned_use_marker(&packet, "New-N"), None);
            Ok(())
        }

        #[test]
        fn retained_r19_menu513_accepts_only_its_exact_full_title_and_fragments() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Prop.",
                        1.0,
                        NormalizedBounds::new(0.0, 0.015_040_662, 0.189_024_39, 0.122_899_71)?,
                    )?,
                    TextObservation::new(
                        "Pathing",
                        1.0,
                        NormalizedBounds::new(0.20, 0.022_222_22, 0.258_333_33, 0.10)?,
                    )?,
                    TextObservation::new(
                        "Prop. Pathing",
                        1.0,
                        NormalizedBounds::new(
                            0.016_108_334,
                            0.024_088_288,
                            0.434_480_22,
                            0.084_691_42,
                        )?,
                    )?,
                ],
                selected: vec!["On".to_owned()],
            };
            assert!(screen_matches_label(&screen, "Prop. Pathing"));
            screen.observations.push(TextObservation::new(
                "Setup",
                1.0,
                NormalizedBounds::new(0.55, 0.02, 0.20, 0.10)?,
            )?);
            assert!(!screen_matches_label(&screen, "Prop. Pathing"));
            Ok(())
        }

        #[test]
        fn reviewed_character_and_record_editors_are_never_safe_inspected_or_entered() -> TestResult
        {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            for (number, anchor) in [
                ("564", "563"),
                ("583", "570"),
                ("594", "593"),
                ("595", "593"),
                ("652", "645"),
                ("653", "645"),
                ("654", "645"),
                ("903", "902"),
                ("946", "945"),
            ] {
                assert_eq!(manifest_entry(&entries, number)?.class, AuditClass::RowOnly);
                assert_eq!(
                    row_only_policy(number)?,
                    RowOnlyPolicy::MultiRecordEditor,
                    "menu {number}"
                );
                assert_eq!(row_only_anchor(number)?, anchor, "menu {number}");
                assert!(safe_inspection_oracle(number).is_err(), "menu {number}");
            }

            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let editor = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "Reply Message Text",
                        1.0,
                        NormalizedBounds::new(0.0, 0.011_111_111, 0.616_666_7, 0.111_111_11)?,
                    )?,
                    TextObservation::new(
                        "Space",
                        1.0,
                        NormalizedBounds::new(0.066_666_66, 0.90, 0.191_666_66, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "0/50",
                        1.0,
                        NormalizedBounds::new(0.416_666_66, 0.90, 0.208_333_33, 0.088_888_89)?,
                    )?,
                    TextObservation::new(
                        "Clear",
                        1.0,
                        NormalizedBounds::new(0.725, 0.90, 0.20, 0.088_888_89)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            assert!(!is_safe_back_context(&editor));
            assert!(!has_reviewed_safe_title(&editor));
            Ok(())
        }

        #[test]
        fn short_text_oracle_accepts_overlapping_vision_alternatives_only() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let bounds = NormalizedBounds::new(0.10, 40.0 / 180.0, 0.60, 20.0 / 180.0)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new("Manual", 1.0, bounds)?,
                    TextObservation::new("Manua l", 1.0, bounds)?,
                ],
                selected: Vec::new(),
            };
            require_exact_short_text(&screen, "Manual")?;
            screen.observations.push(TextObservation::new(
                "Other",
                1.0,
                NormalizedBounds::new(0.72, 40.0 / 180.0, 0.20, 20.0 / 180.0)?,
            )?);
            assert!(require_exact_short_text(&screen, "Manual").is_err());
            Ok(())
        }

        #[test]
        fn quiescence_requires_three_consecutive_identical_frames() -> TestResult {
            let first = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let mut changed = vec![0_u8; SCREEN_BYTES];
            *changed
                .first_mut()
                .ok_or("synthetic screen unexpectedly has no bytes")? = 1;
            let second = ScreenFrame::from_rgb565_le(changed)?;
            let third = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;

            assert!(!three_frames_are_identical(&first, &second, &third));
            assert!(three_frames_are_identical(&first, &first, &first));
            Ok(())
        }

        #[test]
        fn audit_verdict_requires_exact_nonempty_scoped_counts() {
            assert!(require_conclusive(&Summary::default(), 0, 0, 0, 0).is_err());
            let passing = Summary {
                attempted: 4,
                located_rows: 4,
                value_or_information_validated: 2,
                row_only_safe_inspected: 1,
                row_only_located_not_entered: 1,
                restored: 4,
                inconclusive: 0,
            };
            assert!(require_conclusive(&passing, 4, 2, 1, 1).is_ok());
            assert!(require_conclusive(&passing, 217, 162, 15, 40).is_err());
            let inconclusive = Summary {
                inconclusive: 1,
                ..passing
            };
            assert!(require_conclusive(&inconclusive, 4, 2, 1, 1).is_err());
        }

        #[test]
        fn coverage_labels_never_misrepresent_partial_work_as_full() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let all = entries.iter().collect::<Vec<_>>();
            let first = entries
                .first()
                .ok_or("reviewed manifest unexpectedly empty")?;
            let second = entries
                .get(1)
                .ok_or("reviewed manifest unexpectedly has fewer than two rows")?;
            assert_eq!(coverage_scope(&entries, &all), CoverageScope::FullManifest);
            assert_eq!(
                coverage_scope(&entries, &[first]),
                CoverageScope::SingleMenu
            );
            assert_eq!(
                coverage_scope(&entries, &[first, second]),
                CoverageScope::PartialManifest
            );
            assert_eq!(
                CoverageScope::FullManifest.pass_label(),
                "FULL_217_ROWS_162_VALUES_14_SAFE_INSPECTIONS_PASS"
            );
            assert_eq!(CoverageScope::PartialManifest.pass_label(), "SCOPED_PASS");
            Ok(())
        }

        #[test]
        fn ordinary_values_must_belong_to_an_explicit_typed_domain() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            assert!(test_entry_identity(&entries, "510", "Auto")?.is_some());
            assert_eq!(test_entry_identity(&entries, "510", "Manua l")?, None);
            assert!(test_entry_identity(&entries, "904", "GPS(GS)")?.is_some());
            assert!(test_entry_identity(&entries, "910", "A:100/B:100")?.is_some());
            assert_eq!(test_entry_identity(&entries, "980", "IF Output")?, None);
            assert!(test_entry_identity(&entries, "980", "COM+AF/IF Output")?.is_some());
            assert_eq!(test_entry_identity(&entries, "970", "mi")?, None);
            Ok(())
        }

        #[test]
        fn retained_r23_menu611_uses_only_the_index_and_never_the_opaque_message_as_identity()
        -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let entry = manifest_entry(&entries, "611")?;
            for displayed in ["Off", "1", "1:Dylan WNC", "5:opaque:with punctuation"] {
                let expected = if displayed == "Off" {
                    "choice:off"
                } else if displayed.starts_with('1') {
                    "choice:1"
                } else {
                    "choice:5"
                };
                assert_eq!(
                    entry_value_identity(entry, displayed).as_deref(),
                    Some(expected)
                );
            }
            for malformed in [
                "0",
                "6",
                "10",
                "1:",
                "1 Dylan WNC",
                ":message",
                "Off:message",
            ] {
                assert_eq!(
                    entry_value_identity(entry, malformed),
                    None,
                    "Menu 611 accepted malformed value {malformed:?}"
                );
            }

            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..200, 44..68, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let primary_bounds =
                NormalizedBounds::new(0.016_475_836, 0.259_522_35, 0.552_465, 0.122_621_98)?;
            let alternate_bounds =
                NormalizedBounds::new(0.008_333_333, 0.266_666_68, 0.566_666_66, 0.111_111_11)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new("1:Dylan WNC", 1.0, primary_bounds)?,
                    TextObservation::new("1:Dylan #NC", 1.0, alternate_bounds)?,
                ],
                selected: Vec::new(),
            };
            let payload = ordinary_documented_payload(entry, &screen)
                .ok_or("same-index Menu 611 alternatives should validate")?;
            let value = payload
                .first()
                .ok_or("Menu 611 typed payload unexpectedly has no value")?;
            assert_eq!(value, "DocumentedDomain=choice:1");
            assert!(!value.contains("Dylan"));
            screen
                .observations
                .push(TextObservation::new("2:other", 1.0, primary_bounds)?);
            assert_eq!(ordinary_documented_payload(entry, &screen), None);
            Ok(())
        }

        #[test]
        fn retained_r23_aii_confusable_is_all_only_on_menus618_and640() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            for number in ["618", "640"] {
                assert_eq!(
                    entry_value_identity(manifest_entry(&entries, number)?, "AII").as_deref(),
                    Some("choice:all")
                );
            }
            assert_eq!(
                entry_value_identity(manifest_entry(&entries, "542")?, "AII"),
                None
            );

            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..200, 20..44, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let exact_bounds =
                NormalizedBounds::new(0.008_333_332, 0.122_222_22, 0.15, 0.122_222_22)?;
            let overlapping_bounds = NormalizedBounds::new(0.010, 0.124, 0.145, 0.118)?;
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new("AII", 1.0, exact_bounds)?,
                    TextObservation::new("Аll", 1.0, overlapping_bounds)?,
                ],
                selected: Vec::new(),
            };
            assert!(
                ordinary_documented_payload(manifest_entry(&entries, "618")?, &screen).is_some()
            );
            Ok(())
        }

        #[test]
        fn every_observed_manifest_leaf_has_a_reviewed_typed_oracle() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let missing = entries
                .iter()
                .filter(|entry| entry.class != AuditClass::RowOnly)
                .filter(|entry| !entry_has_typed_value_oracle(entry))
                .map(|entry| entry.number.as_str())
                .collect::<Vec<_>>();
            assert!(
                missing.is_empty(),
                "menus without typed oracles: {missing:?}"
            );
            for number in [
                "101", "132", "133", "140", "151", "170", "402", "404", "413", "414", "501", "502",
                "523", "531", "532", "533", "534", "535", "550", "581", "593", "615", "621", "701",
                "901", "915", "917", "918", "91A", "940", "941", "942", "943", "944", "970", "980",
            ] {
                assert!(
                    entry_has_typed_value_oracle(manifest_entry(&entries, number)?),
                    "menu {number} lost its page-specific typed oracle"
                );
            }
            Ok(())
        }

        #[test]
        fn page_specific_domains_expand_shorthand_and_preserve_embedded_punctuation() -> TestResult
        {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;

            assert!(test_entry_identity(&entries, "140", "00.00 MHz")?.is_some());
            assert!(test_entry_identity(&entries, "140", "0.600 MHz")?.is_some());
            assert_eq!(test_entry_identity(&entries, "140", "29.96 MHz")?, None);

            assert!(test_entry_identity(&entries, "550", "Off")?.is_some());
            assert!(test_entry_identity(&entries, "550", "10 mile")?.is_some());
            assert!(test_entry_identity(&entries, "550", "2500 km")?.is_some());
            assert_eq!(test_entry_identity(&entries, "550", "15 mile")?, None);
            assert_eq!(test_entry_identity(&entries, "550", "2510 km")?, None);

            assert!(test_entry_identity(&entries, "940", "Balance")?.is_some());
            assert!(test_entry_identity(&entries, "940", "Balance (PF1)")?.is_some());
            assert!(test_entry_identity(&entries, "941", "GPS")?.is_some());
            assert!(test_entry_identity(&entries, "942", "A/B")?.is_some());
            assert!(test_entry_identity(&entries, "943", "VFO (PF2 Mic)")?.is_some());
            assert!(test_entry_identity(&entries, "944", "MR")?.is_some());
            assert_eq!(
                test_entry_identity(&entries, "940", "same options as PF1")?,
                None
            );

            assert!(test_entry_identity(&entries, "970", "mi/h, mile")?.is_some());
            assert!(test_entry_identity(&entries, "970", "km/h, km")?.is_some());
            assert!(test_entry_identity(&entries, "970", "knots, nm")?.is_some());
            assert_eq!(test_entry_identity(&entries, "970", "mi")?, None);
            assert!(test_entry_identity(&entries, "980", "COM+AF/IF Output")?.is_some());
            assert!(test_entry_identity(&entries, "980", "Mass Storage")?.is_some());
            assert_eq!(test_entry_identity(&entries, "980", "IF Output")?, None);
            Ok(())
        }

        #[test]
        fn reviewed_numeric_and_discrete_domains_accept_all_values_and_adjacent_invalids()
        -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            check_numeric_domain_group_one(&entries)?;
            check_numeric_domain_group_two(&entries)?;
            Ok(())
        }

        #[test]
        fn ordinary_value_requires_one_selected_complete_value_without_conflicts() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let entry = manifest_entry(&entries, "103")?;
            let selected_bounds = NormalizedBounds::new(0.0, 20.0 / 180.0, 0.5, 20.0 / 180.0)?;

            let no_band_frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let no_band = super::CapturedScreen {
                crc32: no_band_frame.crc32(),
                frame: no_band_frame,
                observations: vec![TextObservation::new("Off", 1.0, selected_bounds)?],
                selected: vec!["Off".to_owned()],
            };
            assert_eq!(ordinary_documented_payload(entry, &no_band), None);

            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..200, 20..40, V103_SELECTION_RGB565)?;
            let selected_frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut selected = super::CapturedScreen {
                crc32: selected_frame.crc32(),
                frame: selected_frame,
                observations: vec![TextObservation::new("Off", 1.0, selected_bounds)?],
                selected: vec!["Off".to_owned()],
            };
            assert!(ordinary_documented_payload(entry, &selected).is_some());
            selected.observations.push(TextObservation::new(
                "conflicting text",
                1.0,
                selected_bounds,
            )?);
            assert!(ordinary_documented_payload(entry, &selected).is_some());
            assert!(selected.observations.pop().is_some());
            selected.observations.push(TextObservation::new(
                "conflicting text",
                1.0,
                NormalizedBounds::new(0.55, 20.0 / 180.0, 0.4, 20.0 / 180.0)?,
            )?);
            assert_eq!(ordinary_documented_payload(entry, &selected), None);

            let type_entry = manifest_entry(&entries, "101")?;
            selected.observations = vec![
                TextObservation::new("Тyре 1", 1.0, selected_bounds)?,
                TextObservation::new("Lype 1", 1.0, selected_bounds)?,
            ];
            assert!(ordinary_documented_payload(type_entry, &selected).is_some());
            Ok(())
        }

        #[test]
        fn retained_r17_r20_r23_and_r26_centered_scalar_pages_validate_one_typed_body_locus()
        -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            check_centered_scalar_cases(&entries, &frame)?;
            assert_eq!(canonical_value_text("2. 4 kHz"), "2.4 khz");
            assert_eq!(canonical_value_text("St. Louis"), "st. louis");

            let entry = manifest_entry(&entries, "132")?;
            let mut conflicting = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        anchor_page_title(entry),
                        1.0,
                        NormalizedBounds::new(0.0, 0.02, 0.60, 0.10)?,
                    )?,
                    TextObservation::new(
                        "8 sec",
                        1.0,
                        NormalizedBounds::new(0.37, 0.388, 0.27, 0.122)?,
                    )?,
                    TextObservation::new(
                        "Back",
                        1.0,
                        NormalizedBounds::new(0.08, 0.90, 0.16, 0.08)?,
                    )?,
                ],
                selected: Vec::new(),
            };
            conflicting.observations.push(TextObservation::new(
                "unrelated",
                1.0,
                NormalizedBounds::new(0.02, 0.55, 0.30, 0.10)?,
            )?);
            assert_eq!(
                centered_scalar_documented_payload(entry, &conflicting),
                None
            );
            assert!(conflicting.observations.pop().is_some());
            assert!(centered_scalar_documented_payload(entry, &conflicting).is_some());
            conflicting
                .observations
                .retain(|observation| observation.text() != anchor_page_title(entry));
            assert_eq!(
                centered_scalar_documented_payload(entry, &conflicting),
                None
            );
            Ok(())
        }

        #[test]
        fn retained_r17_and_r26_use_only_exact_numbered_row_lower_value_lane() -> TestResult {
            let entries = parse_menu_manifest(REVIEWED_MANUAL)?;
            let entry = manifest_entry(&entries, "151")?;
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..SCREEN_WIDTH, 44..84, V103_SELECTION_RGB565)?;
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame: frame.clone(),
                observations: vec![
                    TextObservation::new(
                        "151",
                        1.0,
                        NormalizedBounds::new(0.866_666_7, 0.022_222_223, 0.116_666_67, 0.10)?,
                    )?,
                    TextObservation::new(
                        "Gain",
                        1.0,
                        NormalizedBounds::new(
                            0.008_333_332,
                            0.255_555_54,
                            0.208_333_33,
                            0.122_222_22,
                        )?,
                    )?,
                    TextObservation::new(
                        "4",
                        1.0,
                        NormalizedBounds::new(
                            0.908_333_36,
                            0.377_777_79,
                            0.041_666_668,
                            0.077_777_78,
                        )?,
                    )?,
                ],
                selected: vec!["Gain".to_owned(), "4".to_owned()],
            };
            let payload = numbered_row_documented_payload(entry, &row)
                .ok_or_else(|| std::io::Error::other("Menu 151 row payload missing"))?;
            assert!(
                payload
                    .iter()
                    .any(|value| value == "DocumentedDomain=integer:4")
            );

            row.observations.push(TextObservation::new(
                "5",
                1.0,
                NormalizedBounds::new(0.80, 0.39, 0.05, 0.08)?,
            )?);
            assert_eq!(numbered_row_documented_payload(entry, &row), None);

            let beep = manifest_entry(&entries, "915")?;
            let mut beep_row = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![
                    TextObservation::new(
                        "915",
                        1.0,
                        NormalizedBounds::new(0.866_666_7, 0.022_222_223, 0.125, 0.10)?,
                    )?,
                    TextObservation::new(
                        "Beep Volume",
                        1.0,
                        NormalizedBounds::new(
                            0.008_333_332,
                            0.266_666_68,
                            0.558_333_34,
                            0.122_222_22,
                        )?,
                    )?,
                    TextObservation::new(
                        "VOL Link",
                        1.0,
                        NormalizedBounds::new(
                            0.664_539_4,
                            0.375_940_32,
                            0.295_736_2,
                            0.111_074_12,
                        )?,
                    )?,
                ],
                selected: vec!["Beep Volume".to_owned(), "VOL Link".to_owned()],
            };
            let payload = numbered_row_documented_payload(beep, &beep_row)
                .ok_or_else(|| std::io::Error::other("Menu 915 row payload missing"))?;
            assert!(
                payload
                    .iter()
                    .any(|value| value == "DocumentedDomain=choice:vol link")
            );
            beep_row.observations.push(TextObservation::new(
                "Level 1",
                1.0,
                NormalizedBounds::new(0.30, 0.39, 0.20, 0.08)?,
            )?);
            assert_eq!(numbered_row_documented_payload(beep, &beep_row), None);
            Ok(())
        }

        #[test]
        fn menu_134_pri_and_wx1_addresses_match_the_mcp_channel_layout() {
            let flag_address = |channel: u16| 0x2000_usize + usize::from(channel) * 4;
            let data_address = |channel: u16| {
                0x4000_usize
                    + (usize::from(channel) / programming::CHANNELS_PER_MEMGROUP)
                        * programming::PAGE_SIZE
                    + (usize::from(channel) % programming::CHANNELS_PER_MEMGROUP)
                        * programming::CHANNEL_RECORD_SIZE
            };

            assert_eq!(flag_address(MENU_134_PRI_CHANNEL), 0x3130);
            assert_eq!(flag_address(MENU_134_WX1_CHANNEL), 0x3134);
            assert_eq!(data_address(MENU_134_PRI_CHANNEL), 0xF750);
            assert_eq!(data_address(MENU_134_WX1_CHANNEL), 0xF778);
            assert_eq!(
                flag_address(MENU_134_PRI_CHANNEL) / 256,
                usize::from(MENU_134_FLAG_PAGE)
            );
            assert_eq!(
                flag_address(MENU_134_PRI_CHANNEL) % 256,
                MENU_134_PRI_FLAG_OFFSET
            );
            assert_eq!(
                flag_address(MENU_134_WX1_CHANNEL) % 256,
                MENU_134_WX1_FLAG_OFFSET
            );
            assert_eq!(
                data_address(MENU_134_PRI_CHANNEL) / 256,
                usize::from(MENU_134_DATA_PAGE)
            );
            assert_eq!(
                data_address(MENU_134_PRI_CHANNEL) % 256,
                MENU_134_PRI_RECORD_OFFSET
            );
            assert_eq!(
                data_address(MENU_134_WX1_CHANNEL) % 256,
                MENU_134_WX1_RECORD_OFFSET
            );
            assert_eq!(
                MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE,
                MENU_134_WX1_RECORD_OFFSET
            );
            const {
                assert!(
                    MENU_134_WX1_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE
                        <= programming::PAGE_SIZE
                );
            }
        }

        #[test]
        fn menu_134_real_radio_fixtures_pin_stock_wx1_special_flag() -> TestResult {
            let fixtures: [(&str, &[u8]); 3] = [
                (
                    "memory_dump.bin",
                    include_bytes!("../tests/fixtures/memory_dump.bin"),
                ),
                (
                    "memory_dump_a.bin",
                    include_bytes!("../tests/fixtures/memory_dump_a.bin"),
                ),
                (
                    "memory_dump_b.bin",
                    include_bytes!("../tests/fixtures/memory_dump_b.bin"),
                ),
            ];

            for (name, raw) in fixtures {
                let flag_start = usize::from(MENU_134_FLAG_PAGE) * programming::PAGE_SIZE;
                let flag_end = flag_start + programming::PAGE_SIZE;
                let flag: [u8; programming::PAGE_SIZE] = raw
                    .get(flag_start..flag_end)
                    .ok_or_else(|| {
                        std::io::Error::other(format!(
                            "{name} does not contain Menu 134 flag page 0x{MENU_134_FLAG_PAGE:04X}"
                        ))
                    })?
                    .try_into()?;
                let data_start = usize::from(MENU_134_DATA_PAGE) * programming::PAGE_SIZE;
                let data_end = data_start + programming::PAGE_SIZE;
                let data: [u8; programming::PAGE_SIZE] = raw
                    .get(data_start..data_end)
                    .ok_or_else(|| {
                        std::io::Error::other(format!(
                            "{name} does not contain Menu 134 data page 0x{MENU_134_DATA_PAGE:04X}"
                        ))
                    })?
                    .try_into()?;

                assert_eq!(
                    flag[MENU_134_PRI_FLAG_OFFSET],
                    programming::FLAG_EMPTY,
                    "{name} Pri must remain the retained empty prerequisite case"
                );
                assert_eq!(
                    flag[MENU_134_WX1_FLAG_OFFSET],
                    programming::FLAG_VHF,
                    "{name} proves WX1 uses special-channel flag byte 0x00"
                );
                let donor = StoredChannel::from_bytes(
                    &data[MENU_134_WX1_RECORD_OFFSET
                        ..MENU_134_WX1_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE],
                )?;
                assert_eq!(
                    donor.receive_frequency.as_hz(),
                    MENU_134_WX1_RX_HZ,
                    "{name}"
                );
                assert_eq!(donor.mode, ChannelMode::Fm, "{name}");
                assert!(!donor.split, "{name}");
                assert_eq!(donor.shift, ShiftDirection::Simplex, "{name}");
                assert_eq!(donor.transmit_offset_or_frequency.as_hz(), 0, "{name}");

                let plan = plan_menu_134_pri_pages(flag, data)?;
                assert_eq!(
                    plan.disposition,
                    Menu134PriDisposition::StagedFromStockWx1,
                    "{name}"
                );
                assert_eq!(
                    plan.flag_staged[MENU_134_PRI_FLAG_OFFSET],
                    programming::FLAG_VHF,
                    "{name} must copy the retained WX1 flag byte unchanged"
                );
            }
            Ok(())
        }

        #[test]
        fn menu_134_empty_pri_plan_changes_only_one_flag_byte_and_one_record() -> TestResult {
            let (flag, data) = menu_134_empty_pri_fixture()?;
            let donor = data[MENU_134_WX1_RECORD_OFFSET
                ..MENU_134_WX1_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
                .to_vec();
            let plan = plan_menu_134_pri_pages(flag, data)?;
            assert_eq!(plan.disposition, Menu134PriDisposition::StagedFromStockWx1);
            assert!(plan.temporary_write_required());

            let flag_differences = plan
                .flag_before
                .iter()
                .zip(plan.flag_staged.iter())
                .enumerate()
                .filter_map(|(offset, (before, after))| (before != after).then_some(offset))
                .collect::<Vec<_>>();
            assert_eq!(flag_differences, vec![MENU_134_PRI_FLAG_OFFSET]);
            assert_eq!(
                plan.flag_staged[MENU_134_PRI_FLAG_OFFSET],
                programming::FLAG_VHF
            );

            let data_differences = plan
                .data_before
                .iter()
                .zip(plan.data_staged.iter())
                .enumerate()
                .filter_map(|(offset, (before, after))| (before != after).then_some(offset))
                .collect::<Vec<_>>();
            assert!(data_differences.iter().all(|offset| {
                (MENU_134_PRI_RECORD_OFFSET
                    ..MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE)
                    .contains(offset)
            }));
            assert_eq!(
                &plan.data_staged[MENU_134_PRI_RECORD_OFFSET
                    ..MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE],
                donor
            );
            assert_eq!(
                &plan.data_staged[..MENU_134_PRI_RECORD_OFFSET],
                &plan.data_before[..MENU_134_PRI_RECORD_OFFSET]
            );
            assert_eq!(
                &plan.data_staged[MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE..],
                &plan.data_before[MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE..]
            );

            let setup = plan.setup_exchanges()?;
            assert_eq!(
                setup
                    .iter()
                    .map(|exchange| exchange.page().as_raw())
                    .collect::<Vec<_>>(),
                vec![MENU_134_DATA_PAGE, MENU_134_FLAG_PAGE]
            );
            let restore = plan.direct_restore_exchanges()?;
            assert_eq!(
                restore
                    .iter()
                    .map(|exchange| exchange.page().as_raw())
                    .collect::<Vec<_>>(),
                vec![MENU_134_FLAG_PAGE, MENU_134_DATA_PAGE]
            );
            Ok(())
        }

        #[test]
        fn menu_134_valid_existing_pri_is_a_page_exact_no_op() -> TestResult {
            let (mut flag, mut data) = menu_134_empty_pri_fixture()?;
            let existing = StoredChannel {
                receive_frequency: Frequency::new(446_000_000),
                ..synthetic_stored_channel(Frequency::new(446_000_000))
            };
            flag[MENU_134_PRI_FLAG_OFFSET] = programming::FLAG_UHF;
            data[MENU_134_PRI_RECORD_OFFSET
                ..MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
                .copy_from_slice(&existing.to_bytes());
            let plan = plan_menu_134_pri_pages(flag, data)?;
            assert_eq!(plan.disposition, Menu134PriDisposition::ExistingValid);
            assert!(!plan.temporary_write_required());
            assert_eq!(plan.flag_before, plan.flag_staged);
            assert_eq!(plan.data_before, plan.data_staged);
            assert!(
                plan.setup_exchanges()?
                    .iter()
                    .all(|exchange| { exchange.expected() == exchange.replacement() })
            );
            assert!(
                plan.direct_restore_exchanges()?
                    .iter()
                    .all(|exchange| { exchange.expected() == exchange.replacement() })
            );
            Ok(())
        }

        #[test]
        fn menu_134_rejects_invalid_existing_pri_and_each_wx1_fixture_constraint() -> TestResult {
            assert!(require_menu_134_priority_scan_off(false).is_ok());
            assert!(require_menu_134_priority_scan_off(true).is_err());

            let (mut flag, mut data) = menu_134_empty_pri_fixture()?;
            flag[MENU_134_PRI_FLAG_OFFSET] = 0x03;
            data[MENU_134_PRI_RECORD_OFFSET
                ..MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
                .copy_from_slice(
                    &StoredChannel {
                        receive_frequency: Frequency::new(446_000_000),
                        ..synthetic_stored_channel(Frequency::new(446_000_000))
                    }
                    .to_bytes(),
                );
            assert!(plan_menu_134_pri_pages(flag, data).is_err());

            let (mut out_of_range_flag, mut out_of_range_data) = menu_134_empty_pri_fixture()?;
            out_of_range_flag[MENU_134_PRI_FLAG_OFFSET] = programming::FLAG_VHF;
            out_of_range_data[MENU_134_PRI_RECORD_OFFSET
                ..MENU_134_PRI_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
                .copy_from_slice(
                    &StoredChannel {
                        receive_frequency: Frequency::new(99_999),
                        ..synthetic_stored_channel(Frequency::new(99_999))
                    }
                    .to_bytes(),
                );
            assert!(plan_menu_134_pri_pages(out_of_range_flag, out_of_range_data).is_err());

            let (mut malformed_flag, mut malformed_data) = menu_134_empty_pri_fixture()?;
            malformed_flag[MENU_134_PRI_FLAG_OFFSET] = programming::FLAG_VHF;
            malformed_data[MENU_134_PRI_RECORD_OFFSET + 0x08] = 0xF0;
            assert!(plan_menu_134_pri_pages(malformed_flag, malformed_data).is_err());

            let mutate_donor = |channel: StoredChannel, band: u8| -> super::AuditResult<_> {
                let (mut flag, mut data) = menu_134_empty_pri_fixture()?;
                flag[MENU_134_WX1_FLAG_OFFSET] = band;
                data[MENU_134_WX1_RECORD_OFFSET
                    ..MENU_134_WX1_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
                    .copy_from_slice(&channel.to_bytes());
                plan_menu_134_pri_pages(flag, data)
            };
            assert!(mutate_donor(menu_134_stock_wx1(), programming::FLAG_220).is_err());
            let mut malformed_donor = menu_134_stock_wx1().to_bytes();
            malformed_donor[0x08] = 0xF0;
            let (malformed_flag, mut malformed_data) = menu_134_empty_pri_fixture()?;
            malformed_data[MENU_134_WX1_RECORD_OFFSET
                ..MENU_134_WX1_RECORD_OFFSET + programming::CHANNEL_RECORD_SIZE]
                .copy_from_slice(&malformed_donor);
            assert!(plan_menu_134_pri_pages(malformed_flag, malformed_data).is_err());
            assert!(
                mutate_donor(
                    StoredChannel {
                        receive_frequency: Frequency::new(MENU_134_WX1_RX_HZ - 5_000),
                        ..menu_134_stock_wx1()
                    },
                    programming::FLAG_VHF,
                )
                .is_err()
            );
            assert!(
                mutate_donor(
                    StoredChannel {
                        mode: ChannelMode::Am,
                        ..menu_134_stock_wx1()
                    },
                    programming::FLAG_VHF,
                )
                .is_err()
            );
            assert!(
                mutate_donor(
                    StoredChannel {
                        split: true,
                        ..menu_134_stock_wx1()
                    },
                    programming::FLAG_VHF,
                )
                .is_err()
            );
            assert!(
                mutate_donor(
                    StoredChannel {
                        shift: ShiftDirection::Plus,
                        ..menu_134_stock_wx1()
                    },
                    programming::FLAG_VHF,
                )
                .is_err()
            );
            assert!(
                mutate_donor(
                    StoredChannel {
                        transmit_offset_or_frequency: Frequency::new(600_000),
                        ..menu_134_stock_wx1()
                    },
                    programming::FLAG_VHF,
                )
                .is_err()
            );
            Ok(())
        }

        #[test]
        fn menu_134_restore_planner_covers_every_partial_write_state_and_rejects_drift()
        -> TestResult {
            let (flag, data) = menu_134_empty_pri_fixture()?;
            let plan = plan_menu_134_pri_pages(flag, data)?;
            for (live_flag, live_data) in [
                (plan.flag_before, plan.data_before),
                (plan.flag_staged, plan.data_before),
                (plan.flag_before, plan.data_staged),
                (plan.flag_staged, plan.data_staged),
            ] {
                let exchanges = plan_menu_134_restore_pages(&plan, live_flag, live_data)?;
                assert_eq!(exchanges[0].page().as_raw(), MENU_134_FLAG_PAGE);
                assert_eq!(exchanges[1].page().as_raw(), MENU_134_DATA_PAGE);
                assert_eq!(exchanges[0].expected(), &live_flag);
                assert_eq!(exchanges[1].expected(), &live_data);
                assert_eq!(exchanges[0].replacement(), &plan.flag_before);
                assert_eq!(exchanges[1].replacement(), &plan.data_before);
            }

            let mut flag_drift = plan.flag_staged;
            flag_drift[0] ^= 1;
            assert!(plan_menu_134_restore_pages(&plan, flag_drift, plan.data_staged).is_err());
            let mut data_drift = plan.data_staged;
            data_drift[programming::PAGE_SIZE - 1] ^= 1;
            assert!(plan_menu_134_restore_pages(&plan, plan.flag_staged, data_drift).is_err());
            Ok(())
        }

        #[test]
        fn menu_134_failure_aggregation_never_hides_primary_or_cleanup_paths() -> TestResult {
            for mask in 0_u8..16 {
                let cleanup = |bit: u8, label: &'static str| {
                    if mask & bit == 0 {
                        Ok(())
                    } else {
                        audit_error(label)
                    }
                };
                let result = combine_primary_and_cleanup_errors(
                    Ok(()),
                    [
                        ("ui", cleanup(1, "ui-error")),
                        ("reconnect", cleanup(2, "reconnect-error")),
                        ("restore", cleanup(4, "restore-error")),
                        ("verify", cleanup(8, "verify-error")),
                    ],
                );
                assert_eq!(result.is_err(), mask != 0, "cleanup mask {mask:04b}");
                if let Err(error) = result {
                    let rendered = error.to_string();
                    for (bit, label) in [
                        (1, "ui-error"),
                        (2, "reconnect-error"),
                        (4, "restore-error"),
                        (8, "verify-error"),
                    ] {
                        assert_eq!(
                            rendered.contains(label),
                            mask & bit != 0,
                            "cleanup mask {mask:04b}, label {label}"
                        );
                    }
                }
            }
            let both = require_error(
                combine_primary_and_cleanup_errors(
                    audit_error("menu-134-primary"),
                    [("restore", audit_error("menu-134-restore"))],
                ),
                "primary plus restoration failure must fail",
            )?
            .to_string();
            assert!(both.contains("menu-134-primary"));
            assert!(both.contains("menu-134-restore"));
            Ok(())
        }

        #[test]
        fn recoverable_failure_verdict_is_empty_only_for_no_failures() -> TestResult {
            assert!(recoverable_menu_failures_result(&[]).is_ok());
            let error = require_error(
                recoverable_menu_failures_result(&["134: setup".to_owned(), "151: OCR".to_owned()]),
                "nonempty failure set must fail",
            )?
            .to_string();
            assert!(error.contains("2 recoverable menu audit failure(s)"));
            assert!(error.contains("134: setup"));
            assert!(error.contains("151: OCR"));
            Ok(())
        }

        #[test]
        fn configuration_snapshot_scope_is_nonempty_and_byte_drift_fails_equality() -> TestResult {
            let pages = configuration_snapshot_pages()?;
            assert_eq!(
                MCP_D75_MENU_FIELDS.len(),
                EXPECTED_CONFIGURATION_SNAPSHOT_FIELD_COUNT
            );
            assert_eq!(pages.len(), EXPECTED_CONFIGURATION_SNAPSHOT_PAGE_COUNT);
            assert_eq!(
                usize::from(programming::TOTAL_PAGES),
                EXPECTED_MCP_TOTAL_PAGE_COUNT
            );
            assert!(pages.windows(2).all(|pair| {
                let [first, second] = pair else {
                    return false;
                };
                first < second
            }));
            assert!(pages.iter().all(|page| *page < programming::TOTAL_PAGES));

            let first_page = *pages
                .first()
                .ok_or("configuration page registry unexpectedly empty")?;
            let before_pages = vec![(first_page, [0_u8; programming::PAGE_SIZE])];
            let mut after_pages = before_pages.clone();
            let first_after = after_pages
                .first_mut()
                .ok_or("synthetic after snapshot unexpectedly empty")?;
            let changed = first_after
                .1
                .get_mut(17)
                .ok_or("synthetic snapshot byte unavailable")?;
            *changed = 1;
            let before = ConfigurationSnapshot {
                pages: before_pages,
                sha256: [0; 32],
                artifact: "before.bin".to_owned(),
            };
            let after = ConfigurationSnapshot {
                pages: after_pages,
                sha256: [1; 32],
                artifact: "after.bin".to_owned(),
            };
            assert!(!configuration_snapshots_match(&before, &after));
            assert!(configuration_snapshots_match(&before, &before));
            Ok(())
        }

        #[test]
        fn network_payload_coalesces_one_locus_and_rejects_conflicting_loci() -> TestResult {
            let mut bytes = vec![0_u8; SCREEN_BYTES];
            fill_rgb565_rect(&mut bytes, 0..200, 20..40, V103_SELECTION_RGB565)?;
            let aprs_bounds = NormalizedBounds::new(0.0, 20.0 / 180.0, 0.20, 20.0 / 180.0)?;
            let code_bounds = NormalizedBounds::new(0.35, 20.0 / 180.0, 0.38, 20.0 / 180.0)?;
            let use_bounds = NormalizedBounds::new(0.84, 20.0 / 180.0, 0.12, 20.0 / 180.0)?;
            let observations = vec![
                TextObservation::new("APRS", 1.0, aprs_bounds)?,
                TextObservation::new("[АРК005]", 1.0, code_bounds)?,
                TextObservation::new("[APK005]", 1.0, code_bounds)?,
                TextObservation::new("USE", 1.0, use_bounds)?,
            ];
            let frame = ScreenFrame::from_rgb565_le(bytes)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                selected: Vec::new(),
                observations,
            };
            assert_eq!(
                network_payload(&screen),
                Some(vec!["Network=APRS [APK005]".to_owned()])
            );
            screen
                .observations
                .push(TextObservation::new("Altnet", 1.0, aprs_bounds)?);
            assert_eq!(network_payload(&screen), None);
            let removed = screen.observations.pop();
            assert!(removed.is_some());
            screen
                .observations
                .push(TextObservation::new("USE", 1.0, use_bounds)?);
            assert_eq!(
                network_payload(&screen),
                Some(vec!["Network=APRS [APK005]".to_owned()])
            );
            screen.observations.push(TextObservation::new(
                "USE",
                1.0,
                NormalizedBounds::new(0.70, 20.0 / 180.0, 0.20, 20.0 / 180.0)?,
            )?);
            assert_eq!(network_payload(&screen), None);
            Ok(())
        }

        #[test]
        fn scrollable_checkbox_routes_cover_every_row_in_order() {
            assert_eq!(
                scrollable_checkbox_labels("551"),
                Some(
                    [
                        "Weather",
                        "Digipeater",
                        "Mobile",
                        "Object/Item",
                        "NAVITRA",
                        "1-Way",
                        "Others",
                    ]
                    .as_slice()
                )
            );
            assert_eq!(
                scrollable_checkbox_labels("631"),
                Some(
                    [
                        "$GPGGA",
                        "$GPGLL",
                        "$GPGSA",
                        "$GPGSV",
                        "$GPRMC",
                        "$GPVTG",
                        "APRS Sentence",
                    ]
                    .as_slice()
                )
            );
            assert_eq!(scrollable_checkbox_labels("406"), None);
        }

        #[test]
        fn eq_text_parsers_accept_screen_forms_and_reject_ambiguity() {
            assert_eq!(parse_eq_frequency("0. 4 kHz"), Some("0.4"));
            assert_eq!(parse_eq_frequency("6.4 KHz"), Some("6.4"));
            assert_eq!(parse_eq_frequency("6.4"), None);
            assert_eq!(parse_eq_level("±0"), Some(0));
            assert_eq!(parse_eq_level("+Ø"), Some(0));
            assert_eq!(parse_eq_level("+3"), Some(3));
            assert_eq!(parse_eq_level("-9"), Some(-9));
            assert_eq!(parse_eq_level("±3"), None);
            assert_eq!(parse_eq_level("3"), None);
            assert_eq!(parse_eq_level("+10"), None);
        }

        #[test]
        fn eq_payload_requires_one_frequency_and_level_per_screen_row() -> TestResult {
            let mut observations = Vec::new();
            for (slot, frequency) in ["0.4", "0.8", "1.6", "3.2", "6.4"].into_iter().enumerate() {
                let slot = u16::try_from(slot)?;
                let y = 24.0_f32.mul_add(f32::from(slot), 22.0) / 180.0;
                observations.push(TextObservation::new(
                    format!("{frequency} kHz"),
                    1.0,
                    NormalizedBounds::new(0.0, y, 0.4, 20.0 / 180.0)?,
                )?);
                observations.push(TextObservation::new(
                    if slot == 4 { "-9" } else { "+Ø" },
                    1.0,
                    NormalizedBounds::new(0.85, y, 0.1, 20.0 / 180.0)?,
                )?);
            }
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations,
                selected: Vec::new(),
            };
            assert_eq!(
                eq_payload(&screen, &["0.4", "0.8", "1.6", "3.2", "6.4"], -9..=9,),
                Some(vec![
                    "0.4 kHz=±0 dB".to_owned(),
                    "0.8 kHz=±0 dB".to_owned(),
                    "1.6 kHz=±0 dB".to_owned(),
                    "3.2 kHz=±0 dB".to_owned(),
                    "6.4 kHz=-9 dB".to_owned(),
                ])
            );
            assert_eq!(
                eq_payload(&screen, &["0.4", "0.8", "1.6", "3.2"], -9..=-1),
                None
            );
            Ok(())
        }

        #[test]
        fn low_and_high_speed_may_legally_be_equal() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![TextObservation::new(
                    "5 - 5 mile/h",
                    1.0,
                    NormalizedBounds::new(0.0, 0.30, 0.8, 0.10)?,
                )?],
                selected: Vec::new(),
            };
            assert_eq!(
                speed_payload(&screen),
                Some(vec![
                    "Low Speed=5".to_owned(),
                    "High Speed=5".to_owned(),
                    "Unit=mile/h".to_owned(),
                ])
            );
            Ok(())
        }

        #[test]
        fn top_menu_oracle_accepts_reviewed_v103_labels_and_overlapping_ocr_alternative()
        -> TestResult {
            let actual_shape = synthetic_top_menu_screen(false)?;
            assert!(is_top_level_menu(&actual_shape));

            let mut restoration_only = actual_shape;
            restoration_only
                .observations
                .retain(|observation| !matches!(observation.text(), "TX/RX" | "Digital"));
            assert!(is_top_level_menu(&restoration_only));
            assert!(is_restoration_top_level_menu(&restoration_only));

            let mut one_category_only = restoration_only.clone();
            one_category_only
                .observations
                .retain(|observation| observation.text() != "APRS");
            assert!(!is_top_level_menu(&one_category_only));
            assert!(!is_restoration_top_level_menu(&one_category_only));

            let conflicting_title = synthetic_top_menu_screen(true)?;
            assert!(!is_top_level_menu(&conflicting_title));
            assert!(!is_restoration_top_level_menu(&conflicting_title));
            Ok(())
        }

        #[test]
        fn audit_error_aggregation_preserves_primary_and_every_cleanup_failure() -> TestResult {
            assert!(combine_primary_and_cleanup_errors::<0>(Ok(()), []).is_ok());

            let primary_only = require_error(
                combine_primary_and_cleanup_errors(audit_error("primary"), [("cleanup", Ok(()))]),
                "primary error must remain an error",
            )?
            .to_string();
            assert_eq!(primary_only, "primary");

            let cleanup_only = require_error(
                combine_primary_and_cleanup_errors(
                    Ok(()),
                    [
                        ("first", audit_error("one")),
                        ("second", audit_error("two")),
                    ],
                ),
                "cleanup errors must fail the audit",
            )?
            .to_string();
            assert_eq!(
                cleanup_only,
                "audit cleanup failed: first: one; second: two"
            );

            let both = require_error(
                combine_primary_and_cleanup_errors(
                    audit_error("primary"),
                    [
                        ("first", audit_error("one")),
                        ("second", audit_error("two")),
                    ],
                ),
                "primary and cleanup errors must both remain visible",
            )?
            .to_string();
            assert_eq!(
                both,
                "primary audit failure: primary; cleanup failures: first: one; second: two"
            );
            Ok(())
        }

        #[test]
        fn home_oracle_rejects_frequencies_outside_reviewed_a_b_rows() -> TestResult {
            let wrong_first_row = synthetic_home_screen("FM", 40.0, 150.0)?;
            assert!(compare_dual_band_home(&wrong_first_row, &wrong_first_row).is_err());

            let wrong_second_row = synthetic_home_screen("FM", 60.0, 110.0)?;
            assert!(compare_dual_band_home(&wrong_second_row, &wrong_second_row).is_err());
            Ok(())
        }

        #[test]
        fn home_oracle_requires_a_known_analog_mode_anchor() -> TestResult {
            let unknown_mode = synthetic_home_screen("READY", 60.0, 150.0)?;
            assert!(compare_dual_band_home(&unknown_mode, &unknown_mode).is_err());
            Ok(())
        }

        #[test]
        fn retained_r28_operation_band_accepts_only_canonicalized_ptt_glyphs_in_one_band()
        -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let lower_bounds =
                NormalizedBounds::new(0.016_666_666, 0.433_333_34, 0.30, 0.066_666_67)?;
            let mut screen = super::CapturedScreen {
                crc32: frame.crc32(),
                frame,
                observations: vec![TextObservation::new("ІPТTІ H AМ", 1.0, lower_bounds)?],
                selected: Vec::new(),
            };
            assert_eq!(
                observed_operation_band(&screen),
                Some(kenwood_thd75::types::Band::B)
            );
            screen.observations.push(TextObservation::new(
                "PTT",
                1.0,
                NormalizedBounds::new(0.02, 0.10, 0.15, 0.08)?,
            )?);
            assert_eq!(observed_operation_band(&screen), None);
            Ok(())
        }

        #[test]
        fn home_oracle_rejects_compound_digital_and_overlay_markers() -> TestResult {
            for marker in ["FM GPS", "FM APRS", "FM D-STAR", "FM DV"] {
                let overlay = synthetic_home_screen(marker, 60.0, 150.0)?;
                assert!(
                    compare_dual_band_home(&overlay, &overlay).is_err(),
                    "compound marker {marker:?} was admitted"
                );
            }
            Ok(())
        }

        #[test]
        fn home_anchor_verdict_uses_text_not_vision_geometry() -> TestResult {
            let expected = [super::HomeTextAnchor {
                canonical: "r".to_owned(),
                bounds: NormalizedBounds::new(
                    114.0 / 240.0,
                    20.0 / 180.0,
                    14.0 / 240.0,
                    12.0 / 180.0,
                )?,
            }];
            let same_text_with_different_vision_geometry = [super::HomeTextAnchor {
                canonical: "r".to_owned(),
                bounds: NormalizedBounds::new(
                    80.0 / 240.0,
                    12.0 / 180.0,
                    40.0 / 240.0,
                    28.0 / 180.0,
                )?,
            }];
            assert!(ordered_home_anchor_texts_match(
                &expected,
                &same_text_with_different_vision_geometry
            ));

            let changed_text = [super::HomeTextAnchor {
                canonical: "dv".to_owned(),
                bounds: NormalizedBounds::new(
                    114.0 / 240.0,
                    20.0 / 180.0,
                    14.0 / 240.0,
                    12.0 / 180.0,
                )?,
            }];
            assert!(!ordered_home_anchor_texts_match(&expected, &changed_text));
            Ok(())
        }

        #[test]
        fn long_run_home_oracle_masks_only_reviewed_volatile_regions_and_requires_anchors()
        -> TestResult {
            let observations = vec![
                TextObservation::new(
                    "FM",
                    1.0,
                    NormalizedBounds::new(0.05, 22.0 / 180.0, 0.08, 6.0 / 180.0)?,
                )?,
                TextObservation::new(
                    "R",
                    1.0,
                    NormalizedBounds::new(0.48, 22.0 / 180.0, 0.05, 6.0 / 180.0)?,
                )?,
                TextObservation::new(
                    "144.000",
                    1.0,
                    NormalizedBounds::new(0.15, 50.0 / 180.0, 0.40, 18.0 / 180.0)?,
                )?,
                TextObservation::new(
                    "440.000",
                    1.0,
                    NormalizedBounds::new(0.15, 150.0 / 180.0, 0.40, 18.0 / 180.0)?,
                )?,
            ];
            let baseline_frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let baseline = super::CapturedScreen {
                crc32: baseline_frame.crc32(),
                frame: baseline_frame,
                observations: observations.clone(),
                selected: Vec::new(),
            };
            assert_eq!(
                masked_home_bytes(&baseline.frame).len(),
                HOME_MASK_INCLUDED_PIXELS * 2
            );

            let dropout_frame = baseline.frame.clone();
            let ocr_dropout = super::CapturedScreen {
                crc32: dropout_frame.crc32(),
                frame: dropout_frame,
                observations: observations
                    .iter()
                    .filter(|observation| observation.text() != "R")
                    .cloned()
                    .collect(),
                selected: Vec::new(),
            };
            let dropout_comparison = compare_dual_band_home(&ocr_dropout, &baseline)?;
            assert_eq!(dropout_comparison.masked_differing_pixels, 0);
            assert!(dropout_comparison.restored());

            let volatile_offset = (5 * SCREEN_WIDTH + 10) * 2;
            let volatile_frame =
                single_changed_byte_frame(volatile_offset, "volatile test pixel unavailable")?;
            let volatile = super::CapturedScreen {
                crc32: volatile_frame.crc32(),
                frame: volatile_frame,
                observations: observations.clone(),
                selected: Vec::new(),
            };
            let volatile_comparison = compare_dual_band_home(&volatile, &baseline)?;
            assert_eq!(volatile_comparison.full_differing_pixels, 1);
            assert_eq!(volatile_comparison.masked_differing_pixels, 0);
            assert!(volatile_comparison.restored());

            let (meter_x, meter_y, meter_width, meter_height) = HOME_MASK_SIGNAL_METER_RECT;
            assert_eq!(
                (meter_x, meter_y, meter_width, meter_height),
                (0, 90, 151, 11)
            );
            let meter_offset =
                ((meter_y + meter_height - 1) * SCREEN_WIDTH + meter_x + meter_width - 1) * 2;
            let meter_frame =
                single_changed_byte_frame(meter_offset, "S-meter test pixel unavailable")?;
            let meter = super::CapturedScreen {
                crc32: meter_frame.crc32(),
                frame: meter_frame,
                observations: observations.clone(),
                selected: Vec::new(),
            };
            let meter_comparison = compare_dual_band_home(&meter, &baseline)?;
            assert_eq!(meter_comparison.full_differing_pixels, 1);
            assert_eq!(meter_comparison.masked_differing_pixels, 0);
            assert!(meter_comparison.restored());

            let stable_offset = (80 * SCREEN_WIDTH + 10) * 2;
            let stable_frame =
                single_changed_byte_frame(stable_offset, "stable test pixel unavailable")?;
            let stable = super::CapturedScreen {
                crc32: stable_frame.crc32(),
                frame: stable_frame,
                observations,
                selected: Vec::new(),
            };
            let stable_comparison = compare_dual_band_home(&stable, &baseline)?;
            assert_eq!(stable_comparison.masked_differing_pixels, 1);
            assert!(!stable_comparison.restored());
            Ok(())
        }

        #[test]
        fn menu_102_single_band_oracle_requires_only_the_baseline_band_b_frequency() -> TestResult {
            let baseline = synthetic_home_screen("FM", 60.0, 145.0)?;
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let make_screen = |frequencies: &[&str]| -> Result<_, super::AuditError> {
                let mut observations = vec![TextObservation::new(
                    "FM",
                    1.0,
                    NormalizedBounds::new(0.05, 0.15, 0.10, 0.05)?,
                )?];
                for (index, frequency) in frequencies.iter().enumerate() {
                    observations.push(TextObservation::new(
                        *frequency,
                        1.0,
                        NormalizedBounds::new(
                            0.10,
                            0.45 + u16::try_from(index).map_or(0.0, |value| f32::from(value) * 0.2),
                            0.50,
                            0.15,
                        )?,
                    )?);
                }
                Ok(super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame: frame.clone(),
                    observations,
                    selected: Vec::new(),
                })
            };

            assert!(is_reviewed_single_band_b_home(
                &make_screen(&["440.000"])?,
                &baseline
            ));
            assert!(!is_reviewed_single_band_b_home(
                &make_screen(&["144.000"])?,
                &baseline
            ));
            assert!(!is_reviewed_single_band_b_home(
                &make_screen(&["144.000", "440.000"])?,
                &baseline
            ));
            Ok(())
        }

        #[test]
        fn startup_single_band_oracle_requires_one_top_row_frequency_analog_mode_and_ptt_marker()
        -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let make_screen = |frequency_centers: &[f32],
                               header: Option<&str>|
             -> Result<_, super::AuditError> {
                let mut observations = Vec::new();
                if let Some(header) = header {
                    observations.push(TextObservation::new(
                        header,
                        1.0,
                        NormalizedBounds::new(0.02, 18.0 / 180.0, 0.30, 12.0 / 180.0)?,
                    )?);
                }
                for (index, center) in frequency_centers.iter().copied().enumerate() {
                    observations.push(TextObservation::new(
                        if index == 0 { "146.900" } else { "440.000" },
                        1.0,
                        NormalizedBounds::new(0.05, (center - 9.0) / 180.0, 0.60, 18.0 / 180.0)?,
                    )?);
                }
                Ok(super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame: frame.clone(),
                    observations,
                    selected: Vec::new(),
                })
            };

            assert!(is_reviewed_single_band_home(&make_screen(
                &[60.0],
                Some("PTT H FM")
            )?));
            assert!(!is_reviewed_single_band_home(&make_screen(
                &[60.0],
                Some("FM")
            )?));
            assert!(!is_reviewed_single_band_home(&make_screen(
                &[60.0, 145.0],
                Some("PTT H FM")
            )?));
            assert!(!is_reviewed_single_band_home(&make_screen(
                &[120.0],
                Some("PTT H FM")
            )?));
            Ok(())
        }

        #[test]
        fn startup_single_band_restoration_requires_the_original_frequency_and_operation_band()
        -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let make_screen = |frequency: &str, ptt_y: f32| -> Result<_, super::AuditError> {
                Ok(super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame: frame.clone(),
                    observations: vec![
                        TextObservation::new(
                            "PTT H FM",
                            1.0,
                            NormalizedBounds::new(0.02, ptt_y, 0.30, 12.0 / 180.0)?,
                        )?,
                        TextObservation::new(
                            frequency,
                            1.0,
                            NormalizedBounds::new(0.05, (60.0 - 9.0) / 180.0, 0.60, 18.0 / 180.0)?,
                        )?,
                    ],
                    selected: Vec::new(),
                })
            };

            let baseline = make_screen("146.900", 0.10)?;
            assert!(reviewed_single_band_home_matches(
                &make_screen("146.900", 0.10)?,
                &baseline
            ));
            assert!(!reviewed_single_band_home_matches(
                &make_screen("147.000", 0.10)?,
                &baseline
            ));
            assert!(!reviewed_single_band_home_matches(
                &make_screen("146.900", 0.40)?,
                &baseline
            ));
            Ok(())
        }

        #[test]
        fn firmware_version_payload_requires_one_exact_azimuth_identity() -> TestResult {
            let frame = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
            let make_screen = |values: &[&str]| -> Result<_, super::AuditError> {
                let mut observations = Vec::with_capacity(values.len());
                for (index, value) in values.iter().enumerate() {
                    let bounds = NormalizedBounds::new(
                        0.20,
                        0.35 + u16::try_from(index).map_or(0.0, |value| f32::from(value) * 0.10),
                        0.60,
                        0.08,
                    )?;
                    observations.push(TextObservation::new(*value, 1.0, bounds)?);
                }
                Ok(super::CapturedScreen {
                    crc32: frame.crc32(),
                    frame: frame.clone(),
                    observations,
                    selected: Vec::new(),
                })
            };

            assert_eq!(
                firmware_version_payload(&make_screen(&["V1. 03. AZM"])?),
                Some(vec!["Firmware=V1.03.AZM".to_owned()])
            );
            assert!(firmware_version_payload(&make_screen(&["V1.03"])?).is_none());
            assert!(firmware_version_payload(&make_screen(&["V1.03.000"])?).is_none());
            assert!(firmware_version_payload(&make_screen(&["V1.03.AZM", "V1.03.AZM"])?).is_none());
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    macos::run().await
}

#[cfg(not(target_os = "macos"))]
fn main() {
    use kenwood_thd75 as _;
    use serde_json as _;
    use tokio as _;

    eprintln!("automation_audit requires macOS Vision; USB CDC and native Bluetooth are supported");
    std::process::exit(2);
}
