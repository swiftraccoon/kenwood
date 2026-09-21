//! Tests for decoding format-3 native backup reports and for their rejection
//! by `Snapshot::load_for_usb_write`.

use serde_json::{Value, json};

use super::super::{Provenance, Snapshot, read_document};
use crate::AppResult;

type TestResult = AppResult<()>;

fn transcript(file: &str, events: u64) -> Value {
    json!({"file":file,"complete":true,"events":events,"error":null})
}

fn history(endpoint: &Value, file: &str, events: u64) -> Value {
    json!({
        "attempts":[{"number":1,"started":true,"resolved":endpoint,
            "error":null,"interruption":null}],
        "retry_error":null,"capture_error":null,"transcript":transcript(file, events)
    })
}

fn fixture() -> AppResult<Value> {
    let mut usb = super::super::tests::fixture();
    let backup = usb
        .as_object_mut()
        .ok_or("USB fixture is not an object")?
        .remove("backup")
        .ok_or("USB fixture has no backup")?;
    let endpoint = json!({"address":"01-23-45-67-89-AB","rfcomm_channel":11});
    let identity = backup.get("identity").ok_or("identity missing")?.clone();
    Ok(json!({
        "format_version":3,"operation":{"kind":"configuration_backup"},
        "transport":"native_bluetooth","requested_address":"01-23-45-67-89-AB",
        "helper_executable":null,"service":"serial_port_0x1101",
        "post_exit_service":"fixed_previously_opened_channel",
        "maximum_original_open_attempts":2,"maximum_post_exit_open_attempts":2,
        "post_exit_settle_milliseconds":5000,"open_retry_delay_milliseconds":1000,
        "open_budget_milliseconds":25000,"cat_exchange_timeout_milliseconds":1500,
        "close_budget_milliseconds":2000,
        "identity_assurance":"exact_bluetooth_address_and_cat_tuple_not_physical_unit_continuity",
        "started_at_utc":"2026-01-01T00:00:00Z","finished_at_utc":"2026-01-01T00:01:00Z",
        "signal_error":null,"cancelled":false,
        "workflow":{
            "original_endpoint":endpoint,
            "original_opening":history(&endpoint,"transcript.jsonl",2),
            "original":{"kind":"configuration_backup","backup":backup,
                "gateway_before":0,"close_error":null,
                "transcript":transcript("transcript.jsonl",8000)},
            "fresh_endpoint":endpoint,
            "fresh_opening":history(&endpoint,"post-exit-transcript.jsonl",4),
            "fresh_cat":{"scope":"gateway","identity":identity,"gateway":0,
                "band_a":null,"band_b":null,"operation_error":null,
                "close_error":null,"capture_error":null,"cancelled":false,
                "transcript":transcript("post-exit-transcript.jsonl",18)},
            "settle_error":null,
            "settle_transcript":transcript("post-exit-transcript.jsonl",2)
        }
    }))
}

fn replace(document: &mut Value, path: &str, replacement: Value) -> TestResult {
    *document.pointer_mut(path).ok_or("fixture path missing")? = replacement;
    Ok(())
}

fn remove(document: &mut Value, path: &str) -> TestResult {
    let (parent, field) = path.rsplit_once('/').ok_or("invalid fixture pointer")?;
    let _removed = document
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .ok_or("fixture parent missing")?
        .remove(field)
        .ok_or("fixture field missing")?;
    Ok(())
}

fn load(document: &Value) -> AppResult<Snapshot> {
    read_document(serde_json::to_vec(document)?.as_slice())?.into_snapshot()
}

#[test]
fn fresh_cat_failure_explains_why_complete_pages_are_not_a_usable_backup() -> TestResult {
    let mut document = fixture()?;
    replace(&mut document, "/workflow/fresh_cat/identity", Value::Null)?;
    replace(&mut document, "/workflow/fresh_cat/gateway", Value::Null)?;
    replace(
        &mut document,
        "/workflow/fresh_cat/operation_error",
        json!({"message":"ID timed out after 1500ms","causes":[]}),
    )?;
    let error = load(&document)
        .err()
        .ok_or("failed fresh CAT incorrectly admitted a complete-page backup")?;
    let message = error.to_string();
    assert!(message.contains("native backup is incomplete"), "{message}");
    assert!(
        message.contains("fresh CAT verification failed"),
        "{message}"
    );
    assert!(message.contains("ID timed out after 1500ms"), "{message}");
    Ok(())
}

#[test]
fn absent_fresh_identity_or_gateway_is_explained_and_never_inferred() -> TestResult {
    let source = fixture()?;
    for field in ["identity", "gateway"] {
        let path = format!("/workflow/fresh_cat/{field}");
        let mut absent = source.clone();
        remove(&mut absent, &path)?;
        let missing = load(&absent)
            .err()
            .ok_or("absent fresh reads were inferred")?;
        assert!(
            missing
                .to_string()
                .contains(&format!("missing field `{field}`")),
            "missing {field} must retain its exact schema error: {missing}"
        );
        let mut unobserved = source.clone();
        replace(&mut unobserved, &path, Value::Null)?;
        let error = load(&unobserved)
            .err()
            .ok_or("null fresh reads were inferred")?;
        assert!(
            error
                .to_string()
                .contains("fresh CAT identity and Gateway Off reads"),
            "unobserved {field} must identify the failed verification: {error}"
        );
    }
    Ok(())
}

#[test]
fn duplicate_source_and_nested_fields_retain_specific_rejection() -> TestResult {
    let encoded = serde_json::to_string(&fixture()?)?;
    let object = encoded.strip_prefix('{').ok_or("fixture object missing")?;
    for (field, value) in [
        ("transport", "\"native_bluetooth\""),
        ("format_version", "3"),
    ] {
        let duplicate = format!("{{\"{field}\":{value},{object}");
        let error = read_document(duplicate.as_bytes())
            .err()
            .ok_or("duplicate top-level field was normalized")?;
        assert!(
            error
                .to_string()
                .contains(&format!("duplicate field `{field}`")),
            "duplicate source field must retain its exact schema error: {error}"
        );
    }
    let nested = encoded.replacen(
        "\"operation_error\":null",
        "\"operation_error\":null,\"operation_error\":null",
        1,
    );
    assert_ne!(nested, encoded, "nested duplicate fixture must change");
    let error = read_document(nested.as_bytes())
        .err()
        .ok_or("duplicate nested failure field was normalized")?;
    assert!(
        error
            .to_string()
            .contains("duplicate field `operation_error`"),
        "nested duplicate must retain its exact schema error: {error}"
    );
    Ok(())
}

#[test]
fn native_snapshot_preserves_bytes_and_cannot_supply_usb_write_provenance() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("native.json");
    let bytes = serde_json::to_vec(&fixture()?)?;
    std::fs::write(&path, &bytes)?;
    let snapshot = Snapshot::load(&path)?;
    assert_eq!(snapshot.identity.firmware.as_str(), "1.02");
    assert!(matches!(
        &snapshot.provenance,
        Provenance::NativeBluetooth { address, channel }
            if address.as_str() == "01-23-45-67-89-AB" && channel.get() == 11
    ));
    assert_eq!(
        snapshot.captured_bytes(kenwood_tmd750::Region::new(8, 48)?)?,
        [0x42; 40]
    );
    assert!(snapshot.menu_snapshot().is_ok());
    assert!(snapshot.standard_configuration().is_ok());
    let error = Snapshot::load_for_usb_write(&path)
        .err()
        .ok_or("native evidence incorrectly authorized a USB write source")?;
    assert!(error.to_string().contains("offline inspection"), "{error}");
    assert_eq!(std::fs::read(&path)?, bytes);

    let usb = directory.path().join("usb.json");
    std::fs::write(&usb, serde_json::to_vec(&super::super::tests::fixture())?)?;
    assert!(matches!(
        Snapshot::load_for_usb_write(&usb)?.provenance,
        Provenance::Usb
    ));
    Ok(())
}

#[test]
fn native_format_and_lifecycle_policy_are_not_guessed() -> TestResult {
    let source = fixture()?;
    for (path, value) in [
        ("/format_version", json!(2)),
        ("/format_version", json!(4)),
        ("/operation/kind", json!("fixed_mcp")),
        ("/transport", json!("usb")),
        ("/service", json!("fixed_channel")),
        ("/post_exit_service", json!("serial_port_0x1101")),
        ("/maximum_original_open_attempts", json!(3)),
        ("/maximum_post_exit_open_attempts", json!(1)),
        ("/post_exit_settle_milliseconds", json!(2000)),
        ("/open_retry_delay_milliseconds", json!(0)),
        ("/open_budget_milliseconds", json!(25001)),
        ("/cat_exchange_timeout_milliseconds", json!(1501)),
        ("/close_budget_milliseconds", json!(2001)),
        ("/identity_assurance", json!("physical_unit_continuity")),
        ("/cancelled", json!(true)),
    ] {
        let mut document = source.clone();
        replace(&mut document, path, value)?;
        assert!(
            load(&document).is_err(),
            "admitted changed policy at {path}"
        );
    }
    Ok(())
}

#[test]
fn every_cleanup_capture_signal_and_interruption_requires_explicit_success() -> TestResult {
    let source = fixture()?;
    for path in [
        "/signal_error",
        "/workflow/settle_error",
        "/workflow/original/close_error",
        "/workflow/original/transcript/error",
        "/workflow/original_opening/retry_error",
        "/workflow/original_opening/capture_error",
        "/workflow/original_opening/transcript/error",
        "/workflow/original_opening/attempts/0/interruption",
        "/workflow/fresh_opening/retry_error",
        "/workflow/fresh_opening/capture_error",
        "/workflow/fresh_opening/transcript/error",
        "/workflow/fresh_opening/attempts/0/interruption",
        "/workflow/fresh_cat/operation_error",
        "/workflow/fresh_cat/close_error",
        "/workflow/fresh_cat/capture_error",
        "/workflow/fresh_cat/transcript/error",
        "/workflow/settle_transcript/error",
    ] {
        let mut failed = source.clone();
        replace(&mut failed, path, json!({"message":"failure","causes":[]}))?;
        assert!(load(&failed).is_err(), "admitted failure at {path}");
        let mut absent = source.clone();
        remove(&mut absent, path)?;
        assert!(load(&absent).is_err(), "inferred missing success at {path}");
    }
    Ok(())
}

#[test]
fn identity_gateway_and_selected_endpoint_must_match_every_phase() -> TestResult {
    let source = fixture()?;
    for (path, value) in [
        ("/requested_address", json!("TM-D750")),
        ("/requested_address", json!("01:23:45:67:89:AB")),
        (
            "/workflow/original_endpoint/address",
            json!("01-23-45-67-89-AC"),
        ),
        ("/workflow/original_endpoint/rfcomm_channel", json!(0)),
        ("/workflow/fresh_endpoint/rfcomm_channel", json!(12)),
        ("/workflow/original/gateway_before", json!(2)),
        ("/workflow/original/backup/identity/model", json!("TH-D75")),
        ("/workflow/original/backup/identity/firmware", json!("1.03")),
        ("/workflow/fresh_cat/identity/radio_type", json!("E,2,1")),
        ("/workflow/fresh_cat/gateway", json!(2)),
        ("/workflow/fresh_cat/cancelled", json!(true)),
        ("/workflow/fresh_cat/scope", json!("identity")),
        ("/workflow/fresh_cat/band_a", json!("FM")),
    ] {
        let mut document = source.clone();
        replace(&mut document, path, value)?;
        assert!(load(&document).is_err(), "admitted mismatch at {path}");
    }
    let mut unsupported = source;
    for path in [
        "/workflow/original/backup/identity/firmware",
        "/workflow/fresh_cat/identity/firmware",
    ] {
        replace(&mut unsupported, path, json!("1.03"))?;
    }
    assert!(
        load(&unsupported).is_err(),
        "matching tuples do not expand the native backup qualification target"
    );
    Ok(())
}

fn add_retry(document: &mut Value, phase: &str) -> TestResult {
    let attempts = document
        .pointer_mut(&format!("/workflow/{phase}/attempts"))
        .and_then(Value::as_array_mut)
        .ok_or("attempts missing")?;
    replace(
        attempts.first_mut().ok_or("final attempt missing")?,
        "/number",
        json!(2),
    )?;
    attempts.insert(
        0,
        json!({
            "number":1,"started":true,"resolved":null,"interruption":null,
            "error":{"error":{"message":"native opening deadline","causes":[]},
                "close_error":null,"retry_admission":"native_opening",
                "host_retirement_confirmed":true}
        }),
    );
    Ok(())
}

#[test]
fn complete_eligible_retry_histories_remain_visible_and_admissible() -> TestResult {
    let mut document = fixture()?;
    add_retry(&mut document, "original_opening")?;
    add_retry(&mut document, "fresh_opening")?;
    let snapshot = load(&document)?;
    assert!(matches!(
        snapshot.provenance,
        Provenance::NativeBluetooth { .. }
    ));
    for path in [
        "/workflow/original_endpoint/rfcomm_channel",
        "/workflow/original_opening/attempts/1/resolved/rfcomm_channel",
        "/workflow/fresh_endpoint/rfcomm_channel",
        "/workflow/fresh_opening/attempts/1/resolved/rfcomm_channel",
    ] {
        replace(&mut document, path, json!(19))?;
    }
    assert!(
        load(&document).is_ok(),
        "channel must be observed, not hardcoded"
    );
    Ok(())
}

#[test]
fn recovered_success_cannot_erase_ineligible_or_unretired_predecessors() -> TestResult {
    let mut source = fixture()?;
    add_retry(&mut source, "original_opening")?;
    for (suffix, value) in [
        ("number", json!(2)),
        ("started", json!(false)),
        (
            "resolved",
            json!({"address":"01-23-45-67-89-AB","rfcomm_channel":11}),
        ),
        ("error/retry_admission", json!("refused")),
        ("error/host_retirement_confirmed", json!(false)),
        (
            "error/close_error",
            json!({"message":"reap pending","causes":[]}),
        ),
        ("error/error/message", json!("")),
        ("error/error/causes", json!([""])),
        ("error", Value::Null),
    ] {
        let mut document = source.clone();
        let path = format!("/workflow/original_opening/attempts/0/{suffix}");
        replace(&mut document, &path, value)?;
        assert!(
            load(&document).is_err(),
            "admitted unsafe predecessor at {path}"
        );
    }
    let mut too_many = source;
    add_retry(&mut too_many, "original_opening")?;
    assert!(load(&too_many).is_err(), "third opening must be refused");
    Ok(())
}

#[test]
fn final_opening_needs_exact_returned_owner_and_required_nullable_fields() -> TestResult {
    let source = fixture()?;
    for phase in ["original_opening", "fresh_opening"] {
        for (suffix, value) in [
            ("number", json!(2)),
            ("started", json!(false)),
            ("resolved", Value::Null),
            ("resolved/address", json!("01-23-45-67-89-AC")),
            ("resolved/rfcomm_channel", json!(30)),
        ] {
            let mut document = source.clone();
            let path = format!("/workflow/{phase}/attempts/0/{suffix}");
            replace(&mut document, &path, value)?;
            assert!(
                load(&document).is_err(),
                "admitted final mismatch at {path}"
            );
        }
        for field in ["resolved", "error", "interruption"] {
            let mut document = source.clone();
            let path = format!("/workflow/{phase}/attempts/0/{field}");
            remove(&mut document, &path)?;
            assert!(
                load(&document).is_err(),
                "inferred absent final evidence at {path}"
            );
        }
    }
    Ok(())
}

#[test]
fn capture_prefixes_and_final_evidence_must_be_complete_and_ordered() -> TestResult {
    let source = fixture()?;
    for transcript_path in [
        "/workflow/original/transcript",
        "/workflow/original_opening/transcript",
        "/workflow/fresh_cat/transcript",
        "/workflow/fresh_opening/transcript",
        "/workflow/settle_transcript",
    ] {
        for (field, value) in [
            ("complete", json!(false)),
            ("events", json!(0)),
            ("file", json!("other.jsonl")),
        ] {
            let mut document = source.clone();
            let path = format!("{transcript_path}/{field}");
            replace(&mut document, &path, value)?;
            assert!(
                load(&document).is_err(),
                "admitted incomplete capture at {path}"
            );
        }
    }
    for (path, events) in [
        ("/workflow/original/transcript/events", 2),
        ("/workflow/fresh_cat/transcript/events", 4),
        ("/workflow/settle_transcript/events", 4),
    ] {
        let mut document = source.clone();
        replace(&mut document, path, json!(events))?;
        assert!(
            load(&document).is_err(),
            "admitted impossible prefix ordering at {path}"
        );
    }
    Ok(())
}

#[test]
fn native_backup_cannot_promote_partial_or_malformed_configuration_pages() -> TestResult {
    let source = fixture()?;
    for mutation in 0..5 {
        let mut document = source.clone();
        let segments = document
            .pointer_mut("/workflow/original/backup/segments")
            .and_then(Value::as_array_mut)
            .ok_or("segments missing")?;
        match mutation {
            0 => {
                let _removed = segments.pop();
            }
            1 => segments.swap(0, 1),
            2 => segments.push(segments.first().ok_or("first segment missing")?.clone()),
            3 => replace(
                segments.first_mut().ok_or("first segment missing")?,
                "/length",
                json!(41),
            )?,
            _ => replace(
                segments.first_mut().ok_or("first segment missing")?,
                "/data",
                json!([66]),
            )?,
        }
        assert!(
            load(&document).is_err(),
            "admitted invalid page mutation {mutation}"
        );
    }
    for (path, value) in [
        (
            "/workflow/original/backup/complete_configuration",
            json!(false),
        ),
        ("/workflow/original/backup/entry_reply", json!([48, 77, 13])),
        ("/workflow/original/backup/exit", json!("recovery_required")),
        (
            "/workflow/original/backup/outcome/status",
            json!("cancelled"),
        ),
    ] {
        let mut document = source.clone();
        replace(&mut document, path, value)?;
        assert!(load(&document).is_err(), "admitted failed backup at {path}");
    }
    Ok(())
}

#[test]
fn native_evidence_cannot_fall_through_into_the_usb_provenance_schema() -> TestResult {
    let mut usb = super::super::tests::fixture();
    let object = usb.as_object_mut().ok_or("USB object missing")?;
    let _previous = object.insert("transport".to_owned(), json!("native_bluetooth"));
    assert!(
        load(&usb).is_err(),
        "mixed native/USB report was admitted as USB"
    );

    let mut native = fixture()?;
    let _previous = native
        .as_object_mut()
        .ok_or("native object missing")?
        .insert("endpoint".to_owned(), json!({"path":"/dev/cu.invented"}));
    assert!(
        load(&native).is_err(),
        "mixed native/USB report was admitted"
    );
    Ok(())
}

#[test]
fn duplicate_fields_and_absent_native_policy_are_not_silently_normalized() -> TestResult {
    let source = fixture()?;
    let encoded = serde_json::to_string(&source)?;
    let duplicate = format!(
        "{{\"format_version\":3,{}",
        encoded.strip_prefix('{').ok_or("fixture object missing")?
    );
    assert!(
        read_document(duplicate.as_bytes()).is_err(),
        "duplicate transport-policy fields must be rejected before reconstruction"
    );
    for field in [
        "transport",
        "requested_address",
        "helper_executable",
        "service",
        "post_exit_service",
        "maximum_original_open_attempts",
        "maximum_post_exit_open_attempts",
        "post_exit_settle_milliseconds",
        "open_retry_delay_milliseconds",
        "open_budget_milliseconds",
        "cat_exchange_timeout_milliseconds",
        "close_budget_milliseconds",
        "identity_assurance",
        "cancelled",
    ] {
        let mut document = source.clone();
        remove(&mut document, &format!("/{field}"))?;
        assert!(
            load(&document).is_err(),
            "inferred absent native policy {field}"
        );
    }
    Ok(())
}
