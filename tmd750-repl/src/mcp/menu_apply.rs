//! Menu updates over one MCP connection followed by a fresh CAT check.
//!
//! The update runs as a [`PagePlan`] of the shared page workflow: the first
//! connection compares the whole page, writes it, reads it back and exits
//! MCP; the fresh check reads the identity tuple and Gateway Off. Its journal
//! records the field assignments the plan was prepared from.

use kenwood_tmd750::MenuUpdatePlan;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_transport::bluetooth::BluetoothAddress;
use serde::Serialize;
use serde_json::{Map, Value};

use super::link::NativeLink;
use super::menu::{ApplyRequest, Value as AssignmentValue};
use super::page_apply::{self, PagePlan, PlanKind};
use crate::{AppResult, CommandError, native};

#[cfg(test)]
use super::link::WriteLink;
#[cfg(test)]
use super::page_apply::{Captures, WorkflowResult};
#[cfg(test)]
use crate::capture::Recorder;
#[cfg(test)]
use std::fs::File;
#[cfg(test)]
use std::sync::atomic::AtomicBool;

#[cfg(test)]
mod tests;

#[derive(Serialize)]
struct AssignmentEvidence {
    field: &'static str,
    slot: Option<u8>,
    value: AssignmentValue,
}

/// The journal description of `plan`: its field assignments.
fn description(plan: &MenuUpdatePlan) -> AppResult<Map<String, Value>> {
    let assignments = plan
        .assignments()
        .iter()
        .map(|assignment| AssignmentEvidence {
            field: assignment.field().descriptor.name,
            slot: assignment.slot().map(kenwood_tmd750::SlotIndex::index),
            value: AssignmentValue::from(assignment.value()),
        })
        .collect::<Vec<_>>();
    let mut description = Map::new();
    let _previous =
        description.insert("assignments".to_owned(), serde_json::to_value(assignments)?);
    Ok(description)
}

/// Reserve the capture files and journal, then run the update and its CAT check.
///
/// Returns `CommandError` on a non-Unix host, or when the endpoint is not a
/// TM-D750 main-unit or operation-panel USB endpoint at 9600 baud. Nothing is
/// opened until every capture file and the journal exist and their directories
/// are synchronized.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ApplyRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::Menu)?;
    let plan = request.prepare()?;
    page_apply::run_usb(
        endpoint,
        baud,
        &PagePlan::menu(&plan, description(&plan)?),
        request.output(),
        request.source_backup(),
    )
    .await
}

/// Apply a plan prepared from a native Bluetooth backup of the radio at
/// `address` over the native link to `endpoint`.
///
/// Returns `CommandError`, before any capture file is reserved or any
/// connection is opened, when `endpoint` is not that radio.
pub(super) async fn run_native(
    endpoint: &native::Endpoint,
    plan: &MenuUpdatePlan,
    address: &BluetoothAddress,
    request: &ApplyRequest,
) -> AppResult<()> {
    if endpoint.address != *address {
        return Err(CommandError(format!(
            "the selected radio {} is not {address}, the radio of the source backup; nothing was opened",
            endpoint.address
        ))
        .into());
    }
    let mut backend = native::SystemBackend;
    page_apply::run_prepared(
        &PagePlan::menu(plan, description(plan)?),
        &mut NativeLink::new(&mut backend, endpoint),
        request.output(),
        request.source_backup(),
    )
    .await
}

/// Run `plan` through the shared workflow, as the live paths do.
#[cfg(test)]
async fn run_workflow(
    link: &mut impl WriteLink,
    plan: &MenuUpdatePlan,
    captures: Captures,
    journal: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let description = description(plan).unwrap_or_default();
    page_apply::run_workflow(
        link,
        &PagePlan::menu(plan, description),
        captures,
        journal,
        cancelled,
    )
    .await
}
