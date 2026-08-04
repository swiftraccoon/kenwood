//! Inspect or batch-write MCP-D75 menu fields without transferring a full
//! snapshot.
//!
//! The generated registry comes from the official MCP-D75 serializers. By
//! default this command only validates and displays a patch plan. Pass
//! `--write` to enter MCP mode once, read only the touched pages, apply every
//! assignment, write changed pages, and verify them by read-back.
//! `--read` uses a separate, structurally read-only MCP path: it reads only
//! the pages spanning the selected fields, exits MCP, restores CAT, and only
//! then renders the snapshot.
//!
//! Read-only MCP still displays `PROG MCP` and closes/reopens its transport on
//! exit (USB resets and re-enumerates). Catchable termination signals run MCP
//! recovery; an uncatchable process kill or host power loss can still require a
//! physical radio power cycle. Snapshot output may contain callsigns, saved
//! coordinates, messages, and Bluetooth device names; treat it as private
//! configuration.
//!
//! ```text
//! cargo run -p kenwood-thd75 --example mcp_menu -- --list beep
//! cargo run -p kenwood-thd75 --example mcp_menu -- --read interface
//! cargo run -p kenwood-thd75 --example mcp_menu -- --read --json --bluetooth TH-D75
//! cargo run -p kenwood-thd75 --example mcp_menu -- radio.Beep=on radio.BluetoothOnOff=off
//! cargo run -p kenwood-thd75 --example mcp_menu -- --write --port /dev/cu.usbmodem1234 \
//!     radio.Beep=on radio.BluetoothOnOff=off
//! ```
//!
//! `--json` is valid only with `--read`. It keeps stdout machine-readable and
//! reports connection/status messages on stderr. Every selected field record
//! includes its schema ID, absolute offset, exact raw bytes, and decoded value;
//! byte-array values are emitted in full as hexadecimal.

// Deps visible to every `kenwood-thd75` example target but unused here.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json::{Value, json};
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::future::Future;
use std::io;

use kenwood_thd75::memory::{
    DecodedFieldValue, FieldCodec, FieldValue, MCP_D75_MENU_FIELDS, MCP_D75_SCHEMA_FIRMWARE,
    MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, MCP_D75_SCHEMA_MODEL, MCP_D75_SCHEMA_VERSION,
    MCP_D75_SOURCE_SHA256, MenuField, PatchPlanner, PatchSet, SchemaError,
    is_supported_mcp_d75_schema_target, menu_field,
};
use kenwood_thd75::protocol::programming;
#[cfg(target_os = "macos")]
use kenwood_thd75::transport::BluetoothTransport;
use kenwood_thd75::transport::{EitherTransport, SerialTransport, Transport};
use kenwood_thd75::types::{FirmwareIdentity, RadioModel};
use kenwood_thd75::{McpPage, Radio};

type BoxError = Box<dyn std::error::Error>;
type Result<T = ()> = std::result::Result<T, BoxError>;

const DEFAULT_USB_PORT: &str = "/dev/cu.usbmodem1234";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Endpoint {
    Usb(String),
    Bluetooth(String),
}

impl Endpoint {
    fn description(&self) -> String {
        match self {
            Self::Usb(port) => port.clone(),
            Self::Bluetooth(device_name) => format!("Bluetooth device {device_name:?}"),
        }
    }
}

#[derive(Debug)]
struct Arguments {
    endpoint: Endpoint,
    operation: Operation,
}

#[derive(Debug)]
enum Operation {
    Read {
        filter: Option<String>,
        json: bool,
    },
    Patch {
        write: bool,
        assignments: Vec<String>,
    },
}

fn invalid_input(message: impl Into<String>) -> BoxError {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

fn validate_schema_target(model: RadioModel, firmware: &FirmwareIdentity) -> Result {
    if !is_supported_mcp_d75_schema_target(model, firmware) {
        return Err(invalid_input(format!(
            "refusing MCP-D75 schema access to model {model} firmware {firmware}; \
             validated target is {MCP_D75_SCHEMA_MODEL} vendor firmware \
             {MCP_D75_SCHEMA_FIRMWARE}; accepted exact CAT FV identities are \
             {MCP_D75_SCHEMA_FIRMWARE_IDENTITIES:?}"
        )));
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "Usage:\n  mcp_menu --list [filter]\n  mcp_menu --read [filter] [--json] [--port DEVICE | --bluetooth NAME]\n  mcp_menu [--write] [--port DEVICE | --bluetooth NAME] menu.Field=value [...]\n\nIf neither endpoint is supplied, --port {DEFAULT_USB_PORT} is used. --json is read-only and reserves stdout for one deterministic JSON document.\nValues accept official English option labels, raw decimal/0x numbers, on/off booleans, text, or hex:.../@FILE for byte arrays.\nNumbers resolve as 0x hex first, then as the decimal raw value whenever the field accepts that raw, then as an option label."
    );
}

fn parse_arguments(args: Vec<String>) -> Result<Arguments> {
    let mut write = false;
    let mut read = false;
    let mut json = false;
    let mut port = None;
    let mut bluetooth = None;
    let mut positional = Vec::new();
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--write" => write = true,
            "--read" => read = true,
            "--json" => json = true,
            "--port" => {
                port = Some(
                    args.next()
                        .ok_or_else(|| invalid_input("--port requires a device path"))?,
                );
            }
            "--bluetooth" => {
                bluetooth = Some(
                    args.next()
                        .ok_or_else(|| invalid_input("--bluetooth requires a device name"))?,
                );
            }
            "--help" | "-h" => {
                print_usage();
                return Err(invalid_input("help requested"));
            }
            _ if argument.starts_with('-') => {
                return Err(invalid_input(format!("unknown option `{argument}`")));
            }
            _ => positional.push(argument),
        }
    }

    let endpoint = match (port, bluetooth) {
        (Some(port), None) => Endpoint::Usb(port),
        (None, Some(device_name)) => Endpoint::Bluetooth(device_name),
        (None, None) => Endpoint::Usb(DEFAULT_USB_PORT.to_owned()),
        (Some(_), Some(_)) => {
            return Err(invalid_input(
                "--port and --bluetooth are mutually exclusive",
            ));
        }
    };

    let operation = if read {
        if write {
            return Err(invalid_input("--read and --write cannot be combined"));
        }
        if positional.len() > 1 {
            return Err(invalid_input("--read accepts at most one filter"));
        }
        Operation::Read {
            filter: positional.pop(),
            json,
        }
    } else {
        if json {
            return Err(invalid_input("--json is valid only with --read"));
        }
        if positional.is_empty() {
            return Err(invalid_input(
                "at least one menu.Field=value assignment is required",
            ));
        }
        Operation::Patch {
            write,
            assignments: positional,
        }
    };

    Ok(Arguments {
        endpoint,
        operation,
    })
}

fn print_field(field: &MenuField) {
    let domain = match field.descriptor.codec {
        FieldCodec::Bool | FieldCodec::BitBool { .. } => "boolean".to_owned(),
        FieldCodec::Byte { min, max } | FieldCodec::BitField { min, max, .. } => {
            format!("unsigned {min}..={max}")
        }
        FieldCodec::FixedString { len, .. } => format!("text, max {len} bytes"),
        FieldCodec::Unsigned { min, max, .. } => format!("unsigned {min}..={max}"),
        FieldCodec::Signed { min, max, .. } => format!("signed {min}..={max}"),
        FieldCodec::Bytes { len } => format!("exactly {len} bytes"),
    };
    print!(
        "{} @ 0x{:05X} ({})",
        field.descriptor.name, field.descriptor.offset, domain
    );
    if !field.options.is_empty() {
        print!(" [");
        for (index, option) in field.options.iter().enumerate() {
            if index != 0 {
                print!(", ");
            }
            print!("{}={}", option.raw, option.label.unwrap_or(option.member));
        }
        print!("]");
    }
    if !field.allowed_values.is_empty() {
        if field.allowed_values.len() <= 16 {
            let values = field
                .allowed_values
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            print!(" [allowed: {values}]");
        } else {
            print!(" [{} validated raw choices]", field.allowed_values.len());
        }
    }
    if let Some(transform) = field.storage_transform {
        print!(
            " [stored = round({} × {} / {})]",
            transform.input_unit, transform.numerator, transform.denominator
        );
    }
    if field.is_blob {
        print!(" [blob]");
    }
    println!();
}

fn list_fields(filter: Option<&str>) {
    let normalized = filter.unwrap_or_default().to_ascii_lowercase();
    for field in MCP_D75_MENU_FIELDS {
        if normalized.is_empty()
            || field
                .descriptor
                .name
                .to_ascii_lowercase()
                .contains(&normalized)
        {
            print_field(field);
        }
    }
}

fn select_fields(filter: Option<&str>) -> Result<Vec<&'static MenuField>> {
    let normalized = filter.unwrap_or_default().to_ascii_lowercase();
    let mut selected = MCP_D75_MENU_FIELDS
        .iter()
        .filter(|field| {
            normalized.is_empty()
                || field
                    .descriptor
                    .name
                    .to_ascii_lowercase()
                    .contains(&normalized)
        })
        .collect::<Vec<_>>();
    selected.sort_unstable_by_key(|field| field.descriptor.name);
    if selected.is_empty() {
        return Err(invalid_input(format!(
            "no MCP-D75 menu fields match filter `{}`",
            filter.unwrap_or_default()
        )));
    }
    Ok(selected)
}

fn field_len(field: &MenuField) -> Result<usize> {
    let len = match field.descriptor.codec {
        FieldCodec::Byte { .. }
        | FieldCodec::Bool
        | FieldCodec::BitBool { .. }
        | FieldCodec::BitField { .. } => 1,
        FieldCodec::FixedString { len, .. } | FieldCodec::Bytes { len } => len,
        FieldCodec::Unsigned { width, .. } | FieldCodec::Signed { width, .. } => usize::from(width),
    };
    if len == 0 {
        return Err(invalid_input(format!(
            "field {} has a zero-byte codec",
            field.descriptor.name
        )));
    }
    Ok(len)
}

fn required_pages(fields: &[&MenuField]) -> Result<Vec<u16>> {
    let mut pages = BTreeSet::new();
    for field in fields {
        let len = field_len(field)?;
        let end = field
            .descriptor
            .offset
            .checked_add(len - 1)
            .ok_or_else(|| {
                invalid_input(format!(
                    "field {} extends beyond the address space",
                    field.descriptor.name
                ))
            })?;
        let start_page =
            u16::try_from(field.descriptor.offset / programming::PAGE_SIZE).map_err(|_| {
                invalid_input(format!(
                    "field {} offset is too large",
                    field.descriptor.name
                ))
            })?;
        let end_page = u16::try_from(end / programming::PAGE_SIZE).map_err(|_| {
            invalid_input(format!("field {} end is too large", field.descriptor.name))
        })?;
        if end_page >= programming::TOTAL_PAGES {
            return Err(invalid_input(format!(
                "field {} reaches MCP page 0x{end_page:04X}, beyond the image",
                field.descriptor.name
            )));
        }
        pages.extend(start_page..=end_page);
    }
    Ok(pages.into_iter().collect())
}

fn assemble_sparse_image(pages: &[(u16, [u8; programming::PAGE_SIZE])]) -> Result<Vec<u8>> {
    let mut image = vec![0u8; programming::TOTAL_SIZE];
    for (page, data) in pages {
        let start = usize::from(*page)
            .checked_mul(programming::PAGE_SIZE)
            .ok_or_else(|| invalid_input("MCP page offset overflow"))?;
        let end = start
            .checked_add(programming::PAGE_SIZE)
            .ok_or_else(|| invalid_input("MCP page end overflow"))?;
        let destination = image.get_mut(start..end).ok_or_else(|| {
            invalid_input(format!("MCP page 0x{page:04X} lies outside the image"))
        })?;
        destination.copy_from_slice(data);
    }
    Ok(image)
}

fn raw_field_bytes<'a>(field: &MenuField, image: &'a [u8]) -> Result<&'a [u8]> {
    let len = field_len(field)?;
    let end = field
        .descriptor
        .offset
        .checked_add(len)
        .ok_or_else(|| invalid_input("field byte range overflow"))?;
    image
        .get(field.descriptor.offset..end)
        .ok_or_else(|| invalid_input(format!("field {} is out of bounds", field.descriptor.name)))
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        if write!(&mut output, "{byte:02X}").is_err() {
            return String::new();
        }
    }
    output
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

fn known_rendered_value(field: &MenuField, raw: u64) -> Option<String> {
    match (field.descriptor.name, raw) {
        ("aprs.QsyLimit" | "aprs.FilterPositionLimit", 0) => Some("\"Off\" (raw=0)".to_owned()),
        ("aprs.QsyLimit" | "aprs.FilterPositionLimit", 1..=250) => {
            Some(format!("{} (raw={raw})", raw * 10))
        }
        ("radio.Pf1PfKey", 31) => Some(
            "\"Screen Capture\" (raw=31) [matches the official Mic-PF enum; observed by hardware \
             probe as an off-menu PF1 assignment; generic write rejected]"
                .to_owned(),
        ),
        _ => None,
    }
}

fn render_unsigned(field: &MenuField, raw: u64, min: u64, max: u64) -> String {
    if let Some(rendered) = known_rendered_value(field, raw) {
        return rendered;
    }
    if !field.options.is_empty() {
        return field.option(raw).map_or_else(
            || format!("{raw} [unknown enum raw value]"),
            |option| {
                let label = option.label.unwrap_or(option.member);
                format!("{label:?} (raw={raw})")
            },
        );
    }
    if !field.allowed_values.is_empty() && !field.allowed_values.contains(&raw) {
        return format!("{raw} [outside validated choice set]");
    }
    if !(min..=max).contains(&raw) {
        return format!("{raw} [outside accepted write range {min}..={max}]");
    }
    raw.to_string()
}

/// Recover the exact scalar from a strict read error that reports its value.
///
/// Snapshot inspection must distinguish invalid stored data from an
/// undecodable field. The schema reader correctly rejects values that cannot
/// be written, while these error variants retain enough information for this
/// read-only tool to display the observed scalar with an explicit warning.
const fn observed_scalar_after_validation_error(
    codec: FieldCodec,
    error: &SchemaError,
) -> Option<DecodedFieldValue> {
    match (codec, error) {
        (FieldCodec::Bool, SchemaError::UnsignedOutOfRange { value, .. }) => {
            Some(DecodedFieldValue::Bool(*value != 0))
        }
        (
            FieldCodec::Byte { .. } | FieldCodec::BitField { .. } | FieldCodec::Unsigned { .. },
            SchemaError::UnsignedOutOfRange { value, .. }
            | SchemaError::DisallowedValue { value, .. },
        ) => Some(DecodedFieldValue::Unsigned(*value)),
        (FieldCodec::Signed { .. }, SchemaError::SignedOutOfRange { value, .. }) => {
            Some(DecodedFieldValue::Signed(*value))
        }
        _ => None,
    }
}

fn render_field(field: &MenuField, image: &[u8]) -> String {
    let name = field.descriptor.name;
    let raw_bytes = match raw_field_bytes(field, image) {
        Ok(bytes) => bytes,
        Err(error) => return format!("{name} = <error: {error}>"),
    };

    if field.is_blob {
        return format!(
            "{name} = <blob length={} fnv1a64={:016X}>",
            raw_bytes.len(),
            fnv1a64(raw_bytes)
        );
    }

    let decoded = match field.read(image) {
        Ok(value) => value,
        Err(error) => {
            match observed_scalar_after_validation_error(field.descriptor.codec, &error) {
                Some(value) => value,
                None => {
                    return format!(
                        "{name} = hex:{} [decode error: {error}]",
                        hex_bytes(raw_bytes)
                    );
                }
            }
        }
    };

    let rendered = match (field.descriptor.codec, decoded) {
        (FieldCodec::Bool, DecodedFieldValue::Bool(value)) => {
            let raw = raw_bytes.first().copied().unwrap_or_default();
            if raw <= 1 {
                value.to_string()
            } else {
                format!("{value} [noncanonical raw={raw}]")
            }
        }
        (FieldCodec::BitBool { .. }, DecodedFieldValue::Bool(value)) => value.to_string(),
        (
            FieldCodec::Byte { min, max } | FieldCodec::BitField { min, max, .. },
            DecodedFieldValue::Unsigned(raw),
        ) => render_unsigned(field, raw, u64::from(min), u64::from(max)),
        (FieldCodec::Unsigned { min, max, .. }, DecodedFieldValue::Unsigned(raw)) => {
            render_unsigned(field, raw, min, max)
        }
        (FieldCodec::Signed { min, max, .. }, DecodedFieldValue::Signed(raw)) => {
            if (min..=max).contains(&raw) {
                raw.to_string()
            } else {
                format!("{raw} [outside accepted write range {min}..={max}]")
            }
        }
        (FieldCodec::FixedString { .. }, DecodedFieldValue::Text(text)) => format!("{text:?}"),
        (FieldCodec::Bytes { .. }, DecodedFieldValue::Bytes(bytes)) => {
            format!("hex:{}", hex_bytes(&bytes))
        }
        (codec, value) => format!(
            "<decoder mismatch: codec={} value={value:?}>",
            codec.value_kind()
        ),
    };
    format!("{name} = {rendered}")
}

fn render_snapshot(fields: &[&MenuField], pages: usize, image: &[u8]) {
    println!("MCP-D75 schema v{MCP_D75_SCHEMA_VERSION} source sha256={MCP_D75_SOURCE_SHA256}");
    println!(
        "Snapshot: {} field(s) from {pages} read-only MCP page(s).",
        fields.len()
    );
    for field in fields {
        println!("{}", render_field(field, image));
    }
}

fn decoded_json(field: &MenuField, image: &[u8]) -> Value {
    let raw_bytes = match raw_field_bytes(field, image) {
        Ok(bytes) => bytes,
        Err(error) => {
            return json!({
                "kind": "decode_error",
                "message": error.to_string(),
            });
        }
    };
    let (value, validation_error) = match field.read(image) {
        Ok(value) => (value, None),
        Err(error) => {
            let Some(value) =
                observed_scalar_after_validation_error(field.descriptor.codec, &error)
            else {
                return json!({
                    "kind": "decode_error",
                    "message": error.to_string(),
                });
            };
            (value, Some(error.to_string()))
        }
    };

    let mut decoded = match (field.descriptor.codec, value) {
        (FieldCodec::Bool, DecodedFieldValue::Bool(value)) => json!({
            "canonical_raw": raw_bytes.first().is_some_and(|raw| *raw <= 1),
            "kind": "boolean",
            "value": value,
        }),
        (FieldCodec::BitBool { .. }, DecodedFieldValue::Bool(value)) => json!({
            "kind": "boolean",
            "value": value,
        }),
        (
            FieldCodec::Byte { min, max } | FieldCodec::BitField { min, max, .. },
            DecodedFieldValue::Unsigned(raw),
        ) => unsigned_json(field, raw, u64::from(min), u64::from(max)),
        (FieldCodec::Unsigned { min, max, .. }, DecodedFieldValue::Unsigned(raw)) => {
            unsigned_json(field, raw, min, max)
        }
        (FieldCodec::Signed { min, max, .. }, DecodedFieldValue::Signed(value)) => json!({
            "accepted_for_write": (min..=max).contains(&value),
            "kind": "signed",
            "value": value,
        }),
        (FieldCodec::FixedString { .. }, DecodedFieldValue::Text(value)) => json!({
            "kind": "text",
            "value": value,
        }),
        (FieldCodec::Bytes { .. }, DecodedFieldValue::Bytes(value)) => json!({
            "hex": hex_bytes(&value),
            "kind": "bytes",
            "length": value.len(),
        }),
        (codec, value) => json!({
            "kind": "decode_error",
            "message": format!(
                "decoder mismatch: codec={} value={value:?}",
                codec.value_kind()
            ),
        }),
    };
    if let Some(validation_error) = validation_error
        && let Some(object) = decoded.as_object_mut()
    {
        drop(object.insert(
            "validation_error".to_owned(),
            Value::String(validation_error),
        ));
    }
    decoded
}

fn unsigned_json(field: &MenuField, raw: u64, min: u64, max: u64) -> Value {
    let mut value = json!({
        "accepted_for_write": (min..=max).contains(&raw) && raw_is_accepted(field, raw),
        "kind": "unsigned",
        "rendered": render_unsigned(field, raw, min, max),
        "value": raw,
    });
    if let Some(option) = field.option(raw)
        && let Some(object) = value.as_object_mut()
    {
        drop(object.insert(
            "option".to_owned(),
            json!({
                "label": option.label,
                "member": option.member,
                "resource_key": option.resource_key,
            }),
        ));
    }
    value
}

fn json_field(field: &MenuField, image: &[u8]) -> Value {
    let raw_hex = raw_field_bytes(field, image)
        .map(hex_bytes)
        .map_or(Value::Null, Value::String);
    json!({
        "decoded": decoded_json(field, image),
        "id": field.descriptor.name,
        "offset": field.descriptor.offset,
        "offset_hex": format!("0x{:05X}", field.descriptor.offset),
        "raw_hex": raw_hex,
    })
}

fn json_snapshot(
    fields: &[&MenuField],
    pages: usize,
    image: &[u8],
    model: &str,
    firmware: &str,
) -> Value {
    let mut ordered_fields = fields.to_vec();
    ordered_fields.sort_unstable_by_key(|field| field.descriptor.name);
    let records = ordered_fields
        .into_iter()
        .map(|field| json_field(field, image))
        .collect::<Vec<_>>();
    json!({
        "fields": records,
        "radio": {
            "firmware": firmware,
            "model": model,
        },
        "schema": {
            "source_sha256": MCP_D75_SOURCE_SHA256,
            "version": MCP_D75_SCHEMA_VERSION,
        },
        "snapshot": {
            "field_count": fields.len(),
            "page_count": pages,
        },
    })
}

async fn read_sparse_with_interrupt_recovery<T: Transport>(
    radio: &mut Radio<T>,
    pages: &[u16],
) -> Result<Vec<(u16, [u8; programming::PAGE_SIZE])>> {
    read_sparse_with_interrupt(radio, pages, termination_signal()).await
}

#[cfg(unix)]
async fn termination_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
        _ = hangup.recv() => Ok(()),
    }
}

#[cfg(windows)]
async fn termination_signal() -> io::Result<()> {
    use tokio::signal::windows;

    let mut ctrl_break = windows::ctrl_break()?;
    let mut ctrl_close = windows::ctrl_close()?;
    let mut ctrl_logoff = windows::ctrl_logoff()?;
    let mut ctrl_shutdown = windows::ctrl_shutdown()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = ctrl_break.recv() => Ok(()),
        _ = ctrl_close.recv() => Ok(()),
        _ = ctrl_logoff.recv() => Ok(()),
        _ = ctrl_shutdown.recv() => Ok(()),
    }
}

#[cfg(not(any(unix, windows)))]
async fn termination_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}

async fn read_sparse_with_interrupt<T, I>(
    radio: &mut Radio<T>,
    pages: &[u16],
    interrupt: I,
) -> Result<Vec<(u16, [u8; programming::PAGE_SIZE])>>
where
    T: Transport,
    I: Future<Output = io::Result<()>>,
{
    let pages: Vec<McpPage> = pages
        .iter()
        .copied()
        .map(McpPage::new)
        .collect::<std::result::Result<_, _>>()?;
    let interrupt_result = {
        let read = radio.read_sparse_memory_pages(&pages);
        tokio::pin!(read);
        tokio::pin!(interrupt);
        tokio::select! {
            result = &mut read => {
                return Ok(result?
                    .into_iter()
                    .map(|(page, data)| (page.as_raw(), data))
                    .collect());
            }
            signal = &mut interrupt => signal,
        }
    };

    let interruption = interrupt_result.map_or_else(
        |error| {
            format!(
                "snapshot interrupt listener failed ({error}); the in-progress snapshot was \
                 cancelled"
            )
        },
        |()| "snapshot interrupted".to_owned(),
    );

    Err(recover_interrupted_mcp(radio, interruption).await)
}

async fn apply_patches_with_interrupt_recovery<T: Transport>(
    radio: &mut Radio<T>,
    patches: &PatchSet,
) -> Result<Vec<u16>> {
    apply_patches_with_interrupt(radio, patches, termination_signal()).await
}

async fn apply_patches_with_interrupt<T, I>(
    radio: &mut Radio<T>,
    patches: &PatchSet,
    interrupt: I,
) -> Result<Vec<u16>>
where
    T: Transport,
    I: Future<Output = io::Result<()>>,
{
    let interrupt_result = {
        let write = radio.apply_menu_patches_via_mcp(patches);
        tokio::pin!(write);
        tokio::pin!(interrupt);
        tokio::select! {
            result = &mut write => {
                return Ok(result?
                    .into_iter()
                    .map(kenwood_thd75::WritableMcpPage::as_raw)
                    .collect());
            }
            signal = &mut interrupt => signal,
        }
    };

    let interruption = interrupt_result.map_or_else(
        |error| {
            format!(
                "write interrupt listener failed ({error}); the in-progress write was cancelled; \
                 one or more earlier pages may already have changed"
            )
        },
        |()| "write interrupted; one or more earlier pages may already have changed".to_owned(),
    );

    Err(recover_interrupted_mcp(radio, interruption).await)
}

async fn recover_interrupted_mcp<T: Transport>(
    radio: &mut Radio<T>,
    interruption: String,
) -> BoxError {
    match radio.recover_from_interrupted_mcp().await {
        Ok(()) => invalid_input(format!(
            "{interruption}; MCP exit and normal CAT recovery completed"
        )),
        Err(recovery_error) => match radio.identify().await {
            Ok(info) if info.model == RadioModel::ThD75 => invalid_input(format!(
                "{interruption}; normal CAT is restored, but MCP exit reported: \
                 {recovery_error}"
            )),
            Ok(info) => invalid_input(format!(
                "{interruption}; recovery reconnected to unexpected model `{}` after: \
                 {recovery_error}",
                info.model
            )),
            Err(probe_error) => invalid_input(format!(
                "{interruption} and recovery was not proved: {recovery_error}; \
                 CAT probe also failed: {probe_error}; fully power-cycle the radio"
            )),
        },
    }
}

/// Whether `raw` is a value this field's finite domain (if any) accepts.
fn raw_is_accepted(field: &MenuField, raw: u64) -> bool {
    if !field.options.is_empty() {
        return field.option(raw).is_some();
    }
    if !field.allowed_values.is_empty() {
        return field.allowed_values.contains(&raw);
    }
    true
}

/// Resolve an unsigned value with a fixed precedence: `0x` hex first, then a
/// decimal raw value whenever the field accepts that raw, then an official
/// English label or decompiled member name.
///
/// Numeric input therefore always means the raw value when that raw is
/// valid; a numeric label of a different option can never capture it. A
/// number the field rejects as a raw still falls through to label matching,
/// so labels such as `500` (milliseconds) keep working where `500` is not a
/// valid raw.
fn parse_unsigned(field: &MenuField, text: &str) -> Result<u64> {
    if text.eq_ignore_ascii_case("off")
        && matches!(
            field.descriptor.name,
            "aprs.QsyLimit" | "aprs.FilterPositionLimit"
        )
    {
        return Ok(0);
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return Ok(u64::from_str_radix(hex, 16)?);
    }
    if let Ok(raw) = text.parse::<u64>()
        && raw_is_accepted(field, raw)
    {
        return Ok(raw);
    }
    if let Some(option) = field.options.iter().find(|option| {
        option.member.eq_ignore_ascii_case(text)
            || option
                .label
                .is_some_and(|label| label.eq_ignore_ascii_case(text))
    }) {
        return Ok(option.raw);
    }
    Ok(text.parse()?)
}

fn parse_signed(text: &str) -> Result<i64> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return Ok(i64::from_str_radix(hex, 16)?);
    }
    if let Some(hex) = text
        .strip_prefix("-0x")
        .or_else(|| text.strip_prefix("-0X"))
    {
        return Ok(-i64::from_str_radix(hex, 16)?);
    }
    Ok(text.parse()?)
}

fn parse_bool(field: &MenuField, text: &str) -> Result<bool> {
    match text.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "enabled" => Ok(true),
        "0" | "false" | "off" | "no" | "disabled" => Ok(false),
        _ => Err(invalid_input(format!(
            "field {} expects on/off or true/false, received `{text}`",
            field.descriptor.name
        ))),
    }
}

fn parse_hex_bytes(text: &str) -> Result<Vec<u8>> {
    let compact: String = text
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != ':')
        .collect();
    if !compact.len().is_multiple_of(2) {
        return Err(invalid_input(
            "hex byte input must contain complete byte pairs",
        ));
    }
    compact
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(pair, 16)?)
        })
        .collect()
}

fn parse_bytes(text: &str) -> Result<Vec<u8>> {
    if let Some(path) = text.strip_prefix('@') {
        return Ok(std::fs::read(path)?);
    }
    let hex = text
        .strip_prefix("hex:")
        .ok_or_else(|| invalid_input("byte arrays require hex:... or @FILE"))?;
    parse_hex_bytes(hex)
}

fn add_assignment(planner: &mut PatchPlanner, assignment: &str) -> Result {
    let (name, value) = assignment
        .split_once('=')
        .ok_or_else(|| invalid_input(format!("assignment `{assignment}` is missing `=`")))?;
    let field = menu_field(name)
        .ok_or_else(|| invalid_input(format!("unknown MCP-D75 menu field `{name}`")))?;

    match field.descriptor.codec {
        FieldCodec::Byte { .. } | FieldCodec::BitField { .. } | FieldCodec::Unsigned { .. } => {
            let raw = parse_unsigned(field, value)?;
            field.plan_value(planner, FieldValue::Unsigned(raw))?;
        }
        FieldCodec::Bool | FieldCodec::BitBool { .. } => {
            let boolean = parse_bool(field, value)?;
            field.plan_value(planner, FieldValue::Bool(boolean))?;
        }
        FieldCodec::FixedString { .. } => {
            field.plan_value(planner, FieldValue::Text(value))?;
        }
        FieldCodec::Signed { .. } => {
            let signed = parse_signed(value)?;
            field.plan_value(planner, FieldValue::Signed(signed))?;
        }
        FieldCodec::Bytes { .. } => {
            let bytes = parse_bytes(value)?;
            field.plan_value(planner, FieldValue::Bytes(&bytes))?;
        }
    }
    println!("  {name} = {value}");
    Ok(())
}

fn print_patch_summary(patches: &PatchSet) {
    let pages: Vec<u16> = patches
        .pages()
        .map(kenwood_thd75::WritableMcpPage::as_raw)
        .collect();
    let byte_count: usize = patches
        .page_patches()
        .iter()
        .map(|page| page.bytes().len())
        .sum();
    println!(
        "Patch plan: {byte_count} byte update(s) across {} page(s).",
        pages.len()
    );
    if pages.len() <= 16 {
        let formatted = pages
            .iter()
            .map(|page| format!("0x{page:04X}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Pages: {formatted}");
    } else if let (Some(first), Some(last)) = (pages.first(), pages.last()) {
        println!("Pages: 0x{first:04X}..0x{last:04X} (sparse)");
    }
}

fn open_transport(endpoint: &Endpoint) -> Result<EitherTransport> {
    match endpoint {
        Endpoint::Usb(port) => Ok(EitherTransport::Serial(SerialTransport::open(port)?)),
        Endpoint::Bluetooth(device_name) => open_bluetooth_transport(device_name),
    }
}

#[cfg(target_os = "macos")]
fn open_bluetooth_transport(device_name: &str) -> Result<EitherTransport> {
    Ok(EitherTransport::Bluetooth(BluetoothTransport::open(Some(
        device_name,
    ))?))
}

#[cfg(not(target_os = "macos"))]
fn open_bluetooth_transport(_device_name: &str) -> Result<EitherTransport> {
    Err(invalid_input(
        "--bluetooth uses native RFCOMM and is supported only on macOS",
    ))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args
        .first()
        .is_some_and(|argument| argument == "--list")
    {
        list_fields(raw_args.get(1).map(String::as_str));
        return Ok(());
    }

    let arguments = parse_arguments(raw_args).inspect_err(|_| print_usage())?;
    let Arguments {
        endpoint,
        operation,
    } = arguments;
    match operation {
        Operation::Read { filter, json } => {
            let fields = select_fields(filter.as_deref())?;
            let pages = required_pages(&fields)?;

            let privacy = "Privacy: snapshot output can contain callsigns, coordinates, messages, \
                           and Bluetooth names; send it only to a trusted destination.";
            let connecting = format!(
                "Connecting to {} for a read-only {}-page MCP snapshot...",
                endpoint.description(),
                pages.len()
            );
            if json {
                eprintln!("{privacy}");
                eprintln!("{connecting}");
            } else {
                println!("{privacy}");
                println!("{connecting}");
            }
            let transport = open_transport(&endpoint)?;
            let mut radio = Radio::new(transport);
            let info = radio.identify().await?;
            let firmware = radio.get_firmware_version().await?;
            if let Err(error) = validate_schema_target(info.model, &firmware) {
                drop(radio.disconnect().await);
                return Err(error);
            }

            let page_data = read_sparse_with_interrupt_recovery(&mut radio, &pages).await?;
            radio.disconnect().await?;
            let image = assemble_sparse_image(&page_data)?;
            if json {
                let snapshot = json_snapshot(
                    &fields,
                    pages.len(),
                    &image,
                    info.model.as_str(),
                    firmware.as_str(),
                );
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else {
                println!("Radio: {} firmware {firmware}", info.model);
                render_snapshot(&fields, pages.len(), &image);
            }
            Ok(())
        }
        Operation::Patch { write, assignments } => {
            println!("Validated assignments:");
            let mut planner = PatchPlanner::new();
            for assignment in &assignments {
                add_assignment(&mut planner, assignment)?;
            }
            let patches = planner.finish()?;
            print_patch_summary(&patches);

            if !write {
                println!("Dry run only; pass --write to apply this plan to the radio.");
                return Ok(());
            }

            println!("Connecting to {}...", endpoint.description());
            let transport = open_transport(&endpoint)?;
            let mut radio = Radio::new(transport);
            let info = radio.identify().await?;
            let firmware = radio.get_firmware_version().await?;
            if let Err(error) = validate_schema_target(info.model, &firmware) {
                drop(radio.disconnect().await);
                return Err(error);
            }

            let changed = apply_patches_with_interrupt_recovery(&mut radio, &patches).await?;
            println!("Verified {} changed page(s).", changed.len());
            radio.disconnect().await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_USB_PORT, Endpoint, MCP_D75_SCHEMA_FIRMWARE_IDENTITIES, Operation, Result,
        add_assignment, apply_patches_with_interrupt, field_len, json_snapshot, parse_arguments,
        read_sparse_with_interrupt, render_field, required_pages, select_fields,
        validate_schema_target,
    };
    use kenwood_thd75::Radio;
    use kenwood_thd75::memory::{
        FieldCodec, FieldDescriptor, MenuField, MenuOption, PatchPlanner, StringEncoding,
        menu_field,
    };
    use kenwood_thd75::protocol::programming;
    use kenwood_thd75::transport::MockTransport;
    use kenwood_thd75::types::{FirmwareIdentity, RadioModel};

    const NO_OPTIONS: &[MenuOption] = &[];
    const ENUM_OPTIONS: &[MenuOption] = &[MenuOption {
        raw: 0,
        member: "zero",
        label: Some("Zero"),
        resource_key: None,
    }];

    fn require_error<T>(
        result: Result<T>,
        message: &'static str,
    ) -> Result<Box<dyn std::error::Error>> {
        match result {
            Ok(_) => Err(super::invalid_input(message)),
            Err(error) => Ok(error),
        }
    }

    fn image_with_byte(field: &MenuField, raw: u8) -> Result<Vec<u8>> {
        let mut image = vec![0; field.descriptor.offset + 1];
        let stored = image
            .get_mut(field.descriptor.offset)
            .ok_or_else(|| super::invalid_input("synthetic field offset is out of bounds"))?;
        *stored = raw;
        Ok(image)
    }

    fn json_at<'a>(value: &'a serde_json::Value, path: &[&str]) -> Result<&'a serde_json::Value> {
        let mut current = value;
        for component in path {
            current = current.get(*component).ok_or_else(|| {
                super::invalid_input(format!("JSON path `{}` is missing", path.join(".")))
            })?;
        }
        Ok(current)
    }

    fn assert_json(
        value: &serde_json::Value,
        path: &[&str],
        expected: &serde_json::Value,
    ) -> Result {
        assert_eq!(json_at(value, path)?, expected);
        Ok(())
    }

    #[test]
    fn schema_target_accepts_only_qualified_exact_firmware_identities() -> Result {
        for firmware in MCP_D75_SCHEMA_FIRMWARE_IDENTITIES {
            let firmware = FirmwareIdentity::new(firmware)?;
            validate_schema_target(RadioModel::ThD75, &firmware)?;
        }

        for firmware in ["1.02", "1.03.001", "1.04"] {
            let firmware = FirmwareIdentity::new(firmware)?;
            let error = require_error(
                validate_schema_target(RadioModel::ThD75, &firmware),
                "unsupported schema target unexpectedly accepted",
            )?;
            let message = error.to_string();
            assert!(
                message.contains(firmware.as_str())
                    && message.contains("TH-D75")
                    && message.contains("1.03"),
                "schema-target refusal lost its qualification: {message}"
            );
        }
        Ok(())
    }

    fn synthetic_field(
        name: &'static str,
        offset: usize,
        codec: FieldCodec,
        options: &'static [MenuOption],
        is_blob: bool,
    ) -> MenuField {
        MenuField {
            menu: "test",
            enum_type: None,
            descriptor: FieldDescriptor::new(name, offset, codec),
            options,
            allowed_values: &[],
            storage_transform: None,
            is_blob,
        }
    }

    #[test]
    fn read_arguments_accept_one_filter_and_reject_write() -> Result {
        let arguments = parse_arguments(vec![
            "--read".to_owned(),
            "interface".to_owned(),
            "--port".to_owned(),
            "/dev/test".to_owned(),
        ])?;
        assert_eq!(arguments.endpoint, Endpoint::Usb("/dev/test".to_owned()));
        assert!(
            matches!(
                arguments.operation,
                Operation::Read {
                    filter: Some(ref filter),
                    json: false,
                } if filter == "interface"
            ),
            "read filter was not preserved: {:?}",
            arguments.operation
        );

        let conflict = parse_arguments(vec!["--read".to_owned(), "--write".to_owned()]);
        assert!(conflict.is_err(), "read/write conflict unexpectedly parsed");
        Ok(())
    }

    #[test]
    fn endpoint_and_json_arguments_are_unambiguous() -> Result {
        let defaults = parse_arguments(vec!["--read".to_owned()])?;
        assert_eq!(
            defaults.endpoint,
            Endpoint::Usb(DEFAULT_USB_PORT.to_owned()),
            "the historical implicit USB endpoint changed"
        );

        let bluetooth = parse_arguments(vec![
            "--read".to_owned(),
            "--json".to_owned(),
            "--bluetooth".to_owned(),
            "TH-D75".to_owned(),
            "beep".to_owned(),
        ])?;
        assert_eq!(bluetooth.endpoint, Endpoint::Bluetooth("TH-D75".to_owned()));
        assert!(matches!(
            bluetooth.operation,
            Operation::Read {
                filter: Some(ref filter),
                json: true,
            } if filter == "beep"
        ));

        let endpoint_conflict = require_error(
            parse_arguments(vec![
                "--read".to_owned(),
                "--port".to_owned(),
                "/dev/test".to_owned(),
                "--bluetooth".to_owned(),
                "TH-D75".to_owned(),
            ]),
            "USB and Bluetooth endpoints unexpectedly parsed together",
        )?;
        assert!(endpoint_conflict.to_string().contains("mutually exclusive"));

        let write_json = require_error(
            parse_arguments(vec!["--json".to_owned(), "radio.Beep=on".to_owned()]),
            "machine output unexpectedly accepted on the patch path",
        )?;
        assert!(write_json.to_string().contains("only with --read"));
        Ok(())
    }

    #[test]
    fn no_match_fails_before_a_page_plan_exists() {
        let result = select_fields(Some("definitely-not-a-real-field"));
        assert!(result.is_err(), "impossible filter unexpectedly matched");
    }

    #[test]
    fn generated_registry_has_expected_sparse_page_counts() -> Result {
        let all = select_fields(None)?;
        assert_eq!(all.len(), 400);
        assert_eq!(required_pages(&all)?.len(), 350);

        let scalar = all
            .into_iter()
            .filter(|field| !field.is_blob)
            .collect::<Vec<_>>();
        assert_eq!(scalar.len(), 399);
        assert_eq!(required_pages(&scalar)?.len(), 12);
        Ok(())
    }

    #[test]
    fn cross_page_field_includes_every_spanned_page() -> Result {
        let field = synthetic_field(
            "test.cross_page",
            programming::PAGE_SIZE - 1,
            FieldCodec::Bytes { len: 2 },
            NO_OPTIONS,
            false,
        );
        assert_eq!(field_len(&field)?, 2);
        assert_eq!(required_pages(&[&field])?, vec![0, 1]);
        Ok(())
    }

    #[test]
    fn renderer_flags_noncanonical_and_unknown_values() {
        let boolean = synthetic_field("test.bool", 0, FieldCodec::Bool, NO_OPTIONS, false);
        let rendered = render_field(&boolean, &[2]);
        assert!(
            rendered.contains("noncanonical raw=2"),
            "noncanonical boolean was hidden: {rendered}"
        );

        let enumeration = synthetic_field(
            "test.enum",
            0,
            FieldCodec::Byte { min: 0, max: 7 },
            ENUM_OPTIONS,
            false,
        );
        let rendered = render_field(&enumeration, &[7]);
        assert!(
            rendered.contains("unknown enum raw value"),
            "unknown enum was hidden: {rendered}"
        );
    }

    #[test]
    fn renderer_explains_live_known_special_values() -> Result {
        let qsy = menu_field("aprs.QsyLimit")
            .ok_or_else(|| super::invalid_input("QSY limit field is missing"))?;
        assert_eq!(
            render_field(qsy, &image_with_byte(qsy, 0)?),
            "aprs.QsyLimit = \"Off\" (raw=0)"
        );
        for (raw, displayed) in [(1, 10), (250, 2500)] {
            assert_eq!(
                render_field(qsy, &image_with_byte(qsy, raw)?),
                format!("aprs.QsyLimit = {displayed} (raw={raw})")
            );
        }

        let pf1 = menu_field("radio.Pf1PfKey")
            .ok_or_else(|| super::invalid_input("PF1 field is missing"))?;
        let rendered = render_field(pf1, &image_with_byte(pf1, 31)?);
        assert!(
            rendered.contains("\"Screen Capture\" (raw=31)")
                && rendered.contains("official Mic-PF enum")
                && rendered.contains("observed by hardware probe")
                && rendered.contains("generic write rejected"),
            "known off-menu PF1 value was not explained: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn aprs_distance_off_is_writeable_but_off_menu_pf1_is_not() -> Result {
        let mut off = PatchPlanner::new();
        add_assignment(&mut off, "aprs.QsyLimit=off")?;
        assert_eq!(off.finish()?.pages().count(), 1);

        let mut maximum = PatchPlanner::new();
        add_assignment(&mut maximum, "aprs.QsyLimit=250")?;
        assert_eq!(maximum.finish()?.pages().count(), 1);

        let mut above_maximum = PatchPlanner::new();
        assert!(
            add_assignment(&mut above_maximum, "aprs.QsyLimit=251").is_err(),
            "APRS distance above the validated 0..=250 domain was accepted"
        );

        let mut pf1 = PatchPlanner::new();
        let result = add_assignment(&mut pf1, "radio.Pf1PfKey=31");
        assert!(
            result.is_err(),
            "generic schema writer accepted the off-menu PF1 assignment"
        );
        Ok(())
    }

    #[test]
    fn renderer_escapes_text_and_bounds_blobs() {
        let text = synthetic_field(
            "test.text",
            0,
            FieldCodec::FixedString {
                len: 2,
                encoding: StringEncoding::Utf8,
                padding: 0,
            },
            NO_OPTIONS,
            false,
        );
        assert_eq!(render_field(&text, b"\n\0"), "test.text = \"\\n\"");

        let invalid_text = synthetic_field(
            "test.invalid_text",
            0,
            FieldCodec::FixedString {
                len: 2,
                encoding: StringEncoding::MemoryMap,
                padding: 0,
            },
            NO_OPTIONS,
            false,
        );
        let rendered = render_field(&invalid_text, &[0xFF, 0]);
        assert!(
            rendered.contains("hex:FF00") && rendered.contains("decode error"),
            "invalid text did not use exact hex fallback: {rendered}"
        );

        let blob = synthetic_field(
            "test.blob",
            0,
            FieldCodec::Bytes { len: 4 },
            NO_OPTIONS,
            true,
        );
        let rendered = render_field(&blob, &[1, 2, 3, 4]);
        assert!(
            rendered.contains("length=4") && rendered.contains("fnv1a64="),
            "blob metadata is missing: {rendered}"
        );
        assert!(
            !rendered.contains("01020304"),
            "blob contents leaked into output: {rendered}"
        );
    }

    #[test]
    fn json_snapshot_is_complete_exact_and_deterministically_ordered() -> Result {
        let enumeration = synthetic_field(
            "test.a_enum",
            0,
            FieldCodec::Byte { min: 0, max: 7 },
            ENUM_OPTIONS,
            false,
        );
        let boolean = synthetic_field("test.b_bool", 1, FieldCodec::Bool, NO_OPTIONS, false);
        let invalid_text = synthetic_field(
            "test.c_text",
            2,
            FieldCodec::FixedString {
                len: 2,
                encoding: StringEncoding::MemoryMap,
                padding: 0,
            },
            NO_OPTIONS,
            false,
        );
        let blob = synthetic_field(
            "test.d_blob",
            4,
            FieldCodec::Bytes { len: 2 },
            NO_OPTIONS,
            true,
        );
        let image = [0, 2, 0xFF, 0, 0xAB, 0xCD];
        let forward = [&enumeration, &boolean, &invalid_text, &blob];
        let reverse = [&blob, &invalid_text, &boolean, &enumeration];

        let snapshot = json_snapshot(&reverse, 3, &image, "TH-D75", "1.03");
        assert_eq!(
            serde_json::to_string(&snapshot)?,
            serde_json::to_string(&json_snapshot(&forward, 3, &image, "TH-D75", "1.03"))?,
            "caller order changed machine output"
        );
        assert_json(
            &snapshot,
            &["snapshot", "field_count"],
            &serde_json::json!(4),
        )?;
        assert_json(
            &snapshot,
            &["snapshot", "page_count"],
            &serde_json::json!(3),
        )?;
        assert_json(&snapshot, &["radio", "model"], &serde_json::json!("TH-D75"))?;

        let fields = json_at(&snapshot, &["fields"])?
            .as_array()
            .ok_or_else(|| super::invalid_input("JSON fields are not an array"))?;
        assert_eq!(fields.len(), 4, "a requested field was omitted");
        let enumeration = fields
            .first()
            .ok_or_else(|| super::invalid_input("enum JSON record is missing"))?;
        assert_json(enumeration, &["id"], &serde_json::json!("test.a_enum"))?;
        assert_json(enumeration, &["offset"], &serde_json::json!(0))?;
        assert_json(enumeration, &["raw_hex"], &serde_json::json!("00"))?;
        assert_json(enumeration, &["decoded", "value"], &serde_json::json!(0))?;
        assert_json(
            enumeration,
            &["decoded", "option", "label"],
            &serde_json::json!("Zero"),
        )?;

        let boolean = fields
            .get(1)
            .ok_or_else(|| super::invalid_input("boolean JSON record is missing"))?;
        assert_json(boolean, &["id"], &serde_json::json!("test.b_bool"))?;
        assert_json(boolean, &["decoded", "value"], &serde_json::json!(true))?;
        assert_json(
            boolean,
            &["decoded", "canonical_raw"],
            &serde_json::json!(false),
        )?;
        let boolean_decoded = json_at(boolean, &["decoded"])?;
        assert!(
            json_at(boolean_decoded, &["validation_error"])?
                .as_str()
                .is_some_and(|error| error.contains("outside 0..=1")),
            "noncanonical JSON lost the strict schema error: {boolean_decoded}"
        );

        let invalid_text = fields
            .get(2)
            .ok_or_else(|| super::invalid_input("invalid-text JSON record is missing"))?;
        assert_json(invalid_text, &["id"], &serde_json::json!("test.c_text"))?;
        assert_json(invalid_text, &["raw_hex"], &serde_json::json!("FF00"))?;
        assert_json(
            invalid_text,
            &["decoded", "kind"],
            &serde_json::json!("decode_error"),
        )?;

        let blob = fields
            .get(3)
            .ok_or_else(|| super::invalid_input("blob JSON record is missing"))?;
        assert_json(blob, &["id"], &serde_json::json!("test.d_blob"))?;
        assert_json(blob, &["offset_hex"], &serde_json::json!("0x00004"))?;
        assert_json(blob, &["decoded", "kind"], &serde_json::json!("bytes"))?;
        assert_json(blob, &["decoded", "hex"], &serde_json::json!("ABCD"))?;
        assert_json(blob, &["decoded", "length"], &serde_json::json!(2))?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn signal_listener_failure_still_recovers_the_interrupted_snapshot() -> Result {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(programming::McpPage::new(page)?);
        mock.expect_hang(&read);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        // A second CAT probe proves the helper returned a usable radio.
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let result = read_sparse_with_interrupt(&mut radio, &[page], async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Err(std::io::Error::other("signal backend unavailable"))
        })
        .await;

        let error = require_error(result, "listener failure unexpectedly returned a snapshot")?;
        let message = error.to_string();
        assert!(
            message.contains("interrupt listener failed")
                && message.contains("signal backend unavailable")
                && message.contains("normal CAT recovery completed"),
            "listener error or recovery outcome was lost: {message}"
        );

        let info = radio.identify().await?;
        assert_eq!(info.model, RadioModel::ThD75);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn interrupted_write_recovers_and_warns_about_partial_changes() -> Result {
        let mut planner = PatchPlanner::new();
        add_assignment(&mut planner, "radio.Beep=on")?;
        let patches = planner.finish()?;

        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03.000\r");
        mock.expect(programming::ENTER_PROGRAMMING, b"0M\r");

        let page = 0x0010;
        let read = programming::build_read_command(programming::McpPage::new(page)?);
        let original = [0u8; programming::PAGE_SIZE];
        let mut read_response = Vec::with_capacity(programming::W_RESPONSE_SIZE);
        let [page_hi, page_lo] = page.to_be_bytes();
        read_response.extend_from_slice(&[b'W', page_hi, page_lo, 0, 0]);
        read_response.extend_from_slice(&original);
        mock.expect(&read, &read_response);
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let mut modified = original;
        modified[0x71] = 1;
        let write =
            programming::build_write_command(programming::WritableMcpPage::new(page)?, &modified);
        mock.expect_hang(&write);

        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        // A final probe proves the helper returned a CAT-capable radio.
        mock.expect(b"ID\r", b"ID TH-D75\r");

        let mut radio = Radio::new(mock);
        let result = apply_patches_with_interrupt(&mut radio, &patches, async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(())
        })
        .await;

        let error = require_error(result, "interrupted write unexpectedly completed")?;
        let message = error.to_string();
        assert!(
            message.contains("one or more earlier pages may already have changed")
                && message.contains("normal CAT recovery completed"),
            "partial-write warning or recovery outcome was lost: {message}"
        );

        let info = radio.identify().await?;
        assert_eq!(info.model, RadioModel::ThD75);
        Ok(())
    }
}
