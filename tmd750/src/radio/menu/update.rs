//! Immutable, registry-only assignments and guarded complete-page updates.

use std::collections::{BTreeMap, BTreeSet};

use super::{MenuFieldSnapshot, ScopedMenuField};
use crate::error::{Error, SchemaError};
use crate::memory::{
    DecodedFieldValue, MenuField, MenuValueError, MenuWritePolicy, MenuWritePolicyError,
    PatchPlanner, menu_field,
};
use crate::radio::Identity;
use crate::radio::programming::PageReplacement;
use crate::types::{Address, DvGatewayMode, Page, RadioModel, SlotIndex};

const PM_FIELD: &str = "pm.PmSelect";
const GATEWAY_FIELD: &str = "dv.DvGatewayModeDvGateway";

/// One immutable, typed assignment to an ordinary registered menu field.
///
/// Names resolve through the compiled registry. Scope and input values are
/// validated without normalization; no arbitrary descriptor or raw patch can
/// be substituted. Construction performs no I/O and grants no write authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuAssignment {
    selection: ScopedMenuField<'static>,
    value: DecodedFieldValue,
}

impl MenuAssignment {
    /// Resolve a field, validate its ordinary-update policy, and parse its value.
    ///
    /// Numeric input denotes stored values, not inferred display units. Global
    /// fields reject slots; per-PM fields require an explicit slot, including
    /// slot zero for PM Off. Text retains its exact encoding, case, and spaces.
    ///
    /// # Errors
    ///
    /// Rejects unknown names, unsupported policies, invalid scopes, and values
    /// outside supported syntax, semantic domains, or storage codecs. Errors
    /// never include the supplied text or an unrecognized input name.
    pub fn new(name: &str, slot: Option<SlotIndex>, text: &str) -> Result<Self, MenuUpdateError> {
        let field = menu_field(name).ok_or(MenuUpdateError::UnknownField)?;
        let policy = field.write_policy();
        if policy != MenuWritePolicy::Ordinary {
            return Err(MenuUpdateError::NotOrdinary {
                field: field.descriptor.name,
                policy,
            });
        }
        let selection = ScopedMenuField::new(field, slot)?;
        let value = field.parse_value(text)?;
        field.validate_ordinary_value(value.as_field_value())?;
        Ok(Self { selection, value })
    }

    /// Exact registered field and explicit scope of this assignment.
    #[must_use]
    pub const fn selection(&self) -> ScopedMenuField<'static> {
        self.selection
    }

    /// The immutable registered field metadata.
    #[must_use]
    pub const fn field(&self) -> &'static MenuField {
        self.selection.field()
    }

    /// Explicit PM slot, or `None` for a global field.
    #[must_use]
    pub const fn slot(&self) -> Option<SlotIndex> {
        self.selection.slot()
    }

    /// Exact desired stored value, owned by this assignment.
    #[must_use]
    pub const fn value(&self) -> &DecodedFieldValue {
        &self.value
    }
}

/// Registry-only changes bound to complete captured pages and operating guards.
///
/// The admitted software layout is exactly TM-D750 firmware 1.02, type K,2,1,
/// memory format zero. The captured active PM must be valid and its Gateway
/// must be Off. Complete format, PM-control, and active-Gateway pages are kept
/// alongside every assigned page, even when they are compare-only no-ops.
///
/// This does not widen arbitrary-page programming admission or establish
/// hardware qualification. Callers must independently obtain authorization,
/// match the current identity, compare every expected page before writing,
/// retain durable intent, and verify the full exit/reconnection lifecycle.
/// The plan is neither an atomic transaction nor a rollback instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuUpdatePlan {
    identity: Identity,
    assignments: Vec<MenuAssignment>,
    replacements: Vec<PageReplacement>,
}

impl MenuUpdatePlan {
    /// Prepare an immutable update from actual captured canonical pages.
    ///
    /// Every field-plus-slot assignment must be unique. Shared-byte patches
    /// merge without changing unrelated bits; all whole-page before-images
    /// remain exact. Source snapshots are never modified. Nonempty requests
    /// whose desired bytes already match produce compare-only plans, not writes.
    ///
    /// # Errors
    ///
    /// Rejects unsupported identity, empty or duplicate assignments, missing
    /// complete pages, invalid format/PM/Gateway guards, and patch conflicts.
    pub fn new(
        identity: &Identity,
        snapshot: &MenuFieldSnapshot,
        assignments: Vec<MenuAssignment>,
    ) -> Result<Self, MenuUpdateError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(MenuUpdateError::UnsupportedIdentity {
                actual: identity.clone(),
            });
        }
        if assignments.is_empty() {
            return Err(MenuUpdateError::EmptyAssignments);
        }
        let mut seen = BTreeSet::new();
        let mut planner = PatchPlanner::new();
        for assignment in &assignments {
            let field = assignment.field();
            let slot = assignment.slot();
            if !seen.insert((field.descriptor.name, slot.map(SlotIndex::index))) {
                return Err(MenuUpdateError::DuplicateAssignment {
                    field: field.descriptor.name,
                    slot,
                });
            }
            field.validate_ordinary_value(assignment.value.as_field_value())?;
            let _planned = planner.set_menu(field, slot, assignment.value.as_field_value())?;
        }
        let guards = guard_pages(snapshot)?;
        let mut replacements: BTreeMap<_, _> = snapshot
            .plan_exchanges(&planner.finish()?)?
            .into_iter()
            .map(|change| (change.page().address().as_u32(), change))
            .collect();
        for page in guards {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                replacements.entry(page.address().as_u32())
            {
                let original = captured_page(snapshot, page, "menu guard")?;
                let guard = PageReplacement::new(page, original, original).map_err(Error::from)?;
                let _inserted = entry.insert(guard);
            }
        }
        Ok(Self {
            identity: identity.clone(),
            assignments,
            replacements: replacements.into_values().collect(),
        })
    }

    /// Complete identity bound to the captured before-images.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Immutable assignments in the caller's original order.
    #[must_use]
    pub fn assignments(&self) -> &[MenuAssignment] {
        &self.assignments
    }

    /// Complete expected/replacement pages, including guards, in address order.
    ///
    /// Every page requires a fresh comparison, including no-ops. A guard page
    /// shared with an assignment retains that assignment's intended changes.
    #[must_use]
    pub fn replacements(&self) -> &[PageReplacement] {
        &self.replacements
    }

    /// Require the current session to match the complete captured identity.
    pub(crate) fn validate_identity(&self, actual: &Identity) -> Result<(), MenuUpdateError> {
        if actual == &self.identity {
            Ok(())
        } else {
            Err(MenuUpdateError::IdentityMismatch {
                expected: self.identity.clone(),
                actual: actual.clone(),
            })
        }
    }
}

fn guard_pages(snapshot: &MenuFieldSnapshot) -> Result<Vec<Page>, MenuUpdateError> {
    let format_page = Page::new(Address::new(8).map_err(Error::from)?, 40).map_err(Error::from)?;
    let format = *captured_page(snapshot, format_page, "memory format")?
        .get(2)
        .ok_or(MenuUpdateError::GuardDescriptor {
            field: "memory format",
        })?;
    if format != 0 {
        return Err(MenuUpdateError::MemoryFormat { actual: format });
    }
    let pm = guard_selection(PM_FIELD, None)?;
    let raw = unsigned_guard(snapshot, pm)?;
    let active = u8::try_from(raw)
        .ok()
        .and_then(|index| SlotIndex::new(index).ok())
        .ok_or(MenuUpdateError::PmSelection { actual: raw })?;
    let gateway = guard_selection(GATEWAY_FIELD, Some(active))?;
    let raw = unsigned_guard(snapshot, gateway)?;
    let actual =
        DvGatewayMode::from(
            u8::try_from(raw).map_err(|_| MenuUpdateError::GuardDescriptor {
                field: GATEWAY_FIELD,
            })?,
        );
    if actual != DvGatewayMode::Off {
        return Err(MenuUpdateError::GatewayMode {
            slot: active,
            actual,
        });
    }
    let mut pages = vec![format_page];
    pages.extend(pm.pages()?);
    pages.extend(gateway.pages()?);
    Ok(pages)
}

fn guard_selection(
    name: &'static str,
    slot: Option<SlotIndex>,
) -> Result<ScopedMenuField<'static>, MenuUpdateError> {
    let field = menu_field(name).ok_or(MenuUpdateError::GuardDescriptor { field: name })?;
    Ok(ScopedMenuField::new(field, slot)?)
}

fn unsigned_guard(
    snapshot: &MenuFieldSnapshot,
    selection: ScopedMenuField<'_>,
) -> Result<u64, MenuUpdateError> {
    match snapshot.value(selection)? {
        DecodedFieldValue::Unsigned(raw) => Ok(raw),
        _ => Err(MenuUpdateError::GuardDescriptor {
            field: selection.field().descriptor.name,
        }),
    }
}

fn captured_page<'a>(
    snapshot: &'a MenuFieldSnapshot,
    page: Page,
    field: &'static str,
) -> Result<&'a [u8], SchemaError> {
    snapshot
        .page(page)
        .ok_or_else(|| SchemaError::SnapshotPageMissing {
            field,
            address: page.address().as_u32(),
            len: page.len(),
        })
}

/// Typed rejection of a menu assignment, guarded plan, or session comparison.
///
/// Unrecognized input names and desired text are deliberately omitted from
/// errors; recognized field names and numeric guard observations remain useful.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MenuUpdateError {
    /// Input did not resolve to a compiled registry field.
    #[error("unknown menu field; select a registered field name")]
    UnknownField,
    /// A field requires a different update policy or unresolved value domain.
    #[error("{field} cannot use ordinary menu updates: {policy:?}")]
    NotOrdinary {
        /// Recognized registry field.
        field: &'static str,
        /// Policy excluding this assignment.
        policy: MenuWritePolicy,
    },
    /// No field assignment was provided.
    #[error("menu updates require at least one field assignment")]
    EmptyAssignments,
    /// A field and slot were assigned more than once, even with equal values.
    #[error("menu update repeats {field} in scope {slot:?}")]
    DuplicateAssignment {
        /// Canonical registry field name.
        field: &'static str,
        /// Repeated global or PM scope.
        slot: Option<SlotIndex>,
    },
    /// Captured identity is outside the exact admitted software-layout tuple.
    #[error("menu updates require TM-D750 firmware 1.02 and type K,2,1")]
    UnsupportedIdentity {
        /// Complete supplied identity, retained without inferred components.
        actual: Identity,
    },
    /// Current session identity differs from the immutable captured identity.
    #[error("current menu-update identity differs from the captured identity")]
    IdentityMismatch {
        /// Complete identity associated with the before-images.
        expected: Identity,
        /// Complete identity of the current session.
        actual: Identity,
    },
    /// Required guard metadata is missing or cannot be decoded as expected.
    #[error("menu guard {field} has an unsupported descriptor")]
    GuardDescriptor {
        /// Guard whose compiled storage descriptor cannot be used.
        field: &'static str,
    },
    /// Captured memory format at address ten is not supported.
    #[error("menu updates require memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Actual captured format byte.
        actual: u8,
    },
    /// Captured active PM does not resolve to slot zero through five.
    #[error("menu updates require a valid active PM, got selector {actual}")]
    PmSelection {
        /// Actual stored active-PM selector.
        actual: u64,
    },
    /// Gateway is not Off in the captured active PM slot.
    #[error("menu updates require Gateway Off in slot {slot:?}, got {actual}")]
    GatewayMode {
        /// Active slot selected by the complete PM-control page.
        slot: SlotIndex,
        /// Actual stored Gateway mode, including unnamed raw values.
        actual: DvGatewayMode,
    },
    /// Input syntax, a registry domain, or a codec rejected the desired value.
    #[error(transparent)]
    Value(#[from] MenuValueError),
    /// Supplemental ordinary-update policy or semantic validation failed.
    #[error(transparent)]
    Policy(#[from] MenuWritePolicyError),
    /// Scope, sparse coverage, or patch planning was invalid.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// Complete-page construction or an MCP operation failed.
    #[error(transparent)]
    Operation(#[from] Error),
}

#[cfg(test)]
mod tests;
