//! Batch-write MCP-D75 menu fields without transferring a full snapshot.
//!
//! The generated registry comes from the official MCP-D75 serializers. By
//! default this command only validates and displays a patch plan. Pass
//! `--write` to enter MCP mode once, read only the touched pages, apply every
//! assignment, write changed pages, and verify them by read-back.
//!
//! ```text
//! cargo run -p kenwood-thd75 --example mcp_menu -- --list beep
//! cargo run -p kenwood-thd75 --example mcp_menu -- radio.Beep=on radio.BluetoothOnOff=off
//! cargo run -p kenwood-thd75 --example mcp_menu -- --write --port /dev/cu.usbmodem1234 \
//!     radio.Beep=on radio.BluetoothOnOff=off
//! ```

// Deps visible to every `kenwood-thd75` example target but unused here.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::io;

use kenwood_thd75::Radio;
use kenwood_thd75::memory::{
    FieldCodec, FieldValue, MCP_D75_MENU_FIELDS, MenuField, PatchPlanner, PatchSet, menu_field,
};
use kenwood_thd75::transport::SerialTransport;

type BoxError = Box<dyn std::error::Error>;
type Result<T = ()> = std::result::Result<T, BoxError>;

#[derive(Debug)]
struct Arguments {
    write: bool,
    port: String,
    assignments: Vec<String>,
}

fn invalid_input(message: impl Into<String>) -> BoxError {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

fn print_usage() {
    eprintln!(
        "Usage:\n  mcp_menu --list [filter]\n  mcp_menu [--write] [--port DEVICE] menu.Field=value [...]\n\nValues accept official English option labels, raw decimal/0x numbers, on/off booleans, text, or hex:.../@FILE for byte arrays.\nNumbers resolve as 0x hex first, then as the decimal raw value whenever the field accepts that raw, then as an option label."
    );
}

fn parse_arguments(args: Vec<String>) -> Result<Arguments> {
    let mut write = false;
    let mut port = "/dev/cu.usbmodem1234".to_owned();
    let mut assignments = Vec::new();
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--write" => write = true,
            "--port" => {
                port = args
                    .next()
                    .ok_or_else(|| invalid_input("--port requires a device path"))?;
            }
            "--help" | "-h" => {
                print_usage();
                return Err(invalid_input("help requested"));
            }
            _ if argument.starts_with('-') => {
                return Err(invalid_input(format!("unknown option `{argument}`")));
            }
            _ => assignments.push(argument),
        }
    }

    if assignments.is_empty() {
        return Err(invalid_input(
            "at least one menu.Field=value assignment is required",
        ));
    }
    Ok(Arguments {
        write,
        port,
        assignments,
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
    let pages: Vec<u16> = patches.pages().collect();
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

#[tokio::main]
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
    println!("Validated assignments:");
    let mut planner = PatchPlanner::new();
    for assignment in &arguments.assignments {
        add_assignment(&mut planner, assignment)?;
    }
    let patches = planner.finish()?;
    print_patch_summary(&patches);

    if !arguments.write {
        println!("Dry run only; pass --write to apply this plan to the radio.");
        return Ok(());
    }

    println!("Connecting to {}...", arguments.port);
    let transport = SerialTransport::open(&arguments.port, SerialTransport::DEFAULT_BAUD)?;
    let mut radio = Radio::connect(transport).await?;
    let info = radio.identify().await?;
    if info.model != "TH-D75" {
        return Err(invalid_input(format!(
            "refusing MCP-D75 schema write to unexpected model `{}`",
            info.model
        )));
    }

    let changed = radio.apply_menu_patches(&patches).await?;
    println!("Verified {} changed page(s).", changed.len());
    radio.disconnect().await?;
    Ok(())
}
