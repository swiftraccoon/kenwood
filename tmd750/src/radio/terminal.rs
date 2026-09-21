//! Immutable, narrowly scoped plans for the persistent Reflector Terminal setting.
//!
//! Planning performs no I/O: a [`TerminalPlan`] is local data. A stored Gateway
//! setting is not modem readiness; connection roles, entering MCP while Gateway
//! is active, the recovery journal, shutdown, and fresh verification remain the
//! caller's.

use std::collections::BTreeMap;

use super::Identity;
use super::menu::{MenuFieldSnapshot, ScopedMenuField};
use super::programming::PageReplacement;
use crate::error::{Error, SchemaError};
use crate::memory::{
    DecodedFieldValue, FieldValue, PatchPlanner, TerminalGatewayRoute, menu_field,
};
use crate::types::{Address, DvGatewayMode, Page, RadioModel, SlotIndex};

const PM_FIELD: &str = "pm.PmSelect";
const USB_FIELD: &str = "radio.UsbFunction";
const ROUTE_FIELD: &str = "radio.DvGatewayInterface";
const GATEWAY_FIELD: &str = "dv.DvGatewayModeDvGateway";
const SUBTYPE_FIELD: &str = "dv.SelectTerminalMode";

/// Persistent Gateway selection, independent of the communication-route policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTarget {
    /// Disable Gateway while retaining the captured Terminal subtype.
    Off,
    /// Select Terminal Gateway and its Reflector subtype.
    ReflectorTerminal,
}

impl TerminalTarget {
    const fn gateway(self) -> DvGatewayMode {
        match self {
            Self::Off => DvGatewayMode::Off,
            Self::ReflectorTerminal => DvGatewayMode::Terminal,
        }
    }
}

/// Complete-page Terminal changes bound to an exact identity and captured guards.
///
/// Admission requires TM-D750 firmware 1.02, type K,2,1, format zero, valid active
/// PM, COM+AF USB function, a known Gateway route, and Gateway Off or Reflector
/// Terminal. Every control/routing/target page remains an immutable full-page
/// comparison, including no-ops. The route-preserving constructor changes only
/// Gateway and subtype; explicit route selection may additionally change only
/// the active PM's Gateway route. MY/RPT fields, PM selection, USB function, and
/// every unrelated byte are preserved exactly; no station identity is installed.
///
/// These checks cover the captured layout only. Hardware coverage is the
/// configuration in the crate README: firmware 1.02 in PM Off, Gateway routed
/// to panel USB, Bluetooth modem link; other slots, routes and targets are
/// untested. Apply the plan with
/// [`McpSession::compare_exchange_terminal`](super::programming::McpSession::compare_exchange_terminal),
/// which freshly compares every page before the first write. Multi-page
/// comparison is optimistic, not a radio lock or a firmware-atomic transaction:
/// the `before_write` record, exit, handle release, and the fresh CAT or modem
/// check remain the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPlan {
    identity: Identity,
    slot: SlotIndex,
    route: TerminalGatewayRoute,
    target_route: TerminalGatewayRoute,
    before: DvGatewayMode,
    target: TerminalTarget,
    replacements: Vec<PageReplacement>,
}

impl TerminalPlan {
    /// Prepare the requested change from complete captured canonical pages.
    ///
    /// The active PM is read from the supplied PM-control page, not selected by
    /// the caller. An Off-to-Reflector request changes only Gateway to raw 2 and
    /// subtype to raw 0; a direct Off request changes only Gateway to raw 0.
    /// Off may retain either known subtype, but an already active Terminal
    /// state must have Reflector subtype. The source snapshot is never changed.
    /// Requests already satisfied retain all compare-only guards without W.
    ///
    /// # Errors
    ///
    /// Rejects unsupported identity, absent canonical coverage, nonzero format,
    /// unsupported scalar values, active non-Reflector Terminal, missing registry
    /// metadata, and invalid field patches. Unknown values never become defaults.
    pub fn new(
        identity: &Identity,
        snapshot: &MenuFieldSnapshot,
        target: TerminalTarget,
    ) -> Result<Self, TerminalPlanError> {
        Self::prepare(identity, snapshot, target, RouteRequest::Preserve)
    }

    fn prepare(
        identity: &Identity,
        snapshot: &MenuFieldSnapshot,
        target: TerminalTarget,
        route: RouteRequest,
    ) -> Result<Self, TerminalPlanError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(TerminalPlanError::UnsupportedIdentity {
                actual: identity.clone(),
            });
        }
        let guards = Guards::read(snapshot)?;
        let mut planner = PatchPlanner::new();
        let _gateway = planner.set_menu(
            guards.gateway.field(),
            guards.gateway.slot(),
            FieldValue::Unsigned(u64::from(u8::from(target.gateway()))),
        )?;
        if target == TerminalTarget::ReflectorTerminal {
            let _subtype = planner.set_menu(
                guards.subtype.field(),
                guards.subtype.slot(),
                FieldValue::Unsigned(0),
            )?;
        }
        let target_route = match route {
            RouteRequest::Preserve => guards.route,
            RouteRequest::Select(route) => {
                let field = selection(ROUTE_FIELD, Some(guards.slot))?;
                let _route = planner.set_menu(
                    field.field(),
                    field.slot(),
                    FieldValue::Unsigned(route_value(route)),
                )?;
                route
            }
        };
        let mut replacements: BTreeMap<_, _> = snapshot
            .plan_exchanges(&planner.finish()?)?
            .into_iter()
            .map(|replacement| (replacement.page().address().as_u32(), replacement))
            .collect();
        for page in guards.pages {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                replacements.entry(page.address().as_u32())
            {
                let original = captured_page(snapshot, page, "Terminal guard")?;
                let guard = PageReplacement::new(page, original, original).map_err(Error::from)?;
                let _inserted = entry.insert(guard);
            }
        }
        Ok(Self {
            identity: identity.clone(),
            slot: guards.slot,
            route: guards.route,
            target_route,
            before: guards.before,
            target,
            replacements: replacements.into_values().collect(),
        })
    }

    /// Prepare Reflector Terminal entry on an explicitly selected Gateway route.
    ///
    /// This pure constructor changes only the active PM's Gateway mode, Reflector
    /// subtype, and Gateway route. Applying it requires fresh comparison of every
    /// original control/routing/target page before any write. A satisfied selection keeps
    /// its complete compare-only guards. Selecting a different route while
    /// already in Reflector Terminal creates the same explicit route intent.
    ///
    /// The caller retains a separate usable control path, closes any modem
    /// connection before changing that connection's route, and preserves the
    /// exact recovery images.
    ///
    /// # Errors
    ///
    /// Returns the identity, coverage, domain, or patch errors from [`Self::new`].
    pub fn for_route(
        identity: &Identity,
        snapshot: &MenuFieldSnapshot,
        route: TerminalGatewayRoute,
    ) -> Result<Self, TerminalPlanError> {
        Self::prepare(
            identity,
            snapshot,
            TerminalTarget::ReflectorTerminal,
            RouteRequest::Select(route),
        )
    }

    /// Canonical pages sufficient to plan against any supported active PM.
    ///
    /// Includes complete format and PM-control guards, plus USB function,
    /// Gateway route, Gateway mode, and Terminal subtype coverage for all six
    /// PM slots. Pages are unique and sorted by address. These are addresses to
    /// capture, not observed bytes, and they are not a complete configuration
    /// backup. Planning still validates the captured PM and all guards.
    ///
    /// # Errors
    ///
    /// Returns invalid registered descriptors, scopes, or canonical-page shapes.
    pub fn required_pages() -> Result<Vec<Page>, TerminalPlanError> {
        let format = format_page()?;
        let mut pages = BTreeMap::from([(format.address().as_u32(), format)]);
        for page in selection(PM_FIELD, None)?.pages()? {
            let _previous = pages.insert(page.address().as_u32(), page);
        }
        for slot in SlotIndex::all() {
            for name in [USB_FIELD, ROUTE_FIELD, GATEWAY_FIELD, SUBTYPE_FIELD] {
                for page in selection(name, Some(slot))?.pages()? {
                    let _previous = pages.insert(page.address().as_u32(), page);
                }
            }
        }
        Ok(pages.into_values().collect())
    }

    /// Complete identity bound to the original captured images.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Active PM captured in the immutable PM-control guard; zero is PM Off.
    #[must_use]
    pub const fn slot(&self) -> SlotIndex {
        self.slot
    }

    /// Gateway route in the captured before-images.
    #[must_use]
    pub const fn route(&self) -> TerminalGatewayRoute {
        self.route
    }

    /// Gateway route in this plan's replacement pages.
    #[must_use]
    pub const fn target_route(&self) -> TerminalGatewayRoute {
        self.target_route
    }

    /// Gateway value in this plan's expected pages.
    #[must_use]
    pub const fn before(&self) -> DvGatewayMode {
        self.before
    }

    /// Gateway selection this plan requests.
    #[must_use]
    pub const fn target(&self) -> TerminalTarget {
        self.target
    }

    /// Complete expected/replacement pages in address order, including no-op guards.
    #[must_use]
    pub fn replacements(&self) -> &[PageReplacement] {
        &self.replacements
    }

    /// Derive an exact reverse plan without assuming the forward change occurred.
    ///
    /// Reverse expected pages are synthetic copies of the forward destination,
    /// not newly observed bytes. Every reverse page must match a fresh whole-page
    /// read before any write; a conflict causes no rebase, partial merge, retry,
    /// or unconditional rollback. Retain the originals and recovery journal
    /// separately if either direction is interrupted.
    ///
    /// Reversal restores every original byte, including the original Gateway
    /// route and an original subtype of Repeater while Gateway was Off. This
    /// differs from direct [`TerminalTarget::Off`], which preserves its input
    /// route and subtype.
    /// Guards remain present in both directions. Deriving a reverse plan is
    /// pure: it sends no commands and performs no restoration.
    ///
    /// # Errors
    ///
    /// Returns coverage, guard-domain, or complete-page shape errors when the
    /// synthetic expected images cannot form another supported guarded plan.
    pub fn restoration(&self) -> Result<Self, TerminalPlanError> {
        let expected = MenuFieldSnapshot::from_pages(
            self.replacements
                .iter()
                .map(|replacement| (replacement.page(), replacement.replacement().to_vec()))
                .collect(),
        )?;
        let target = match self.before {
            DvGatewayMode::Off => TerminalTarget::Off,
            DvGatewayMode::Terminal => TerminalTarget::ReflectorTerminal,
            DvGatewayMode::Unqualified(raw) => {
                return Err(TerminalPlanError::GuardValue {
                    field: GATEWAY_FIELD,
                    actual: u64::from(raw),
                });
            }
        };
        let mut reverse = Self::new(&self.identity, &expected, target)?;
        reverse.target_route = self.route;
        reverse.replacements = self
            .replacements
            .iter()
            .map(|replacement| {
                PageReplacement::new(
                    replacement.page(),
                    replacement.replacement(),
                    replacement.expected(),
                )
            })
            .collect::<Result<_, _>>()
            .map_err(Error::from)?;
        Ok(reverse)
    }

    /// Recheck the exact session identity before the shared page engine runs.
    pub(crate) fn validate_identity(&self, actual: &Identity) -> Result<(), TerminalPlanError> {
        if actual == &self.identity {
            Ok(())
        } else {
            Err(TerminalPlanError::IdentityMismatch {
                expected: self.identity.clone(),
                actual: actual.clone(),
            })
        }
    }
}

/// Route preservation or deliberate selection, separate from the mode target.
#[derive(Clone, Copy)]
enum RouteRequest {
    Preserve,
    Select(TerminalGatewayRoute),
}

const fn route_value(route: TerminalGatewayRoute) -> u64 {
    match route {
        TerminalGatewayRoute::MainUnit => 0,
        TerminalGatewayRoute::ControlPanel => 1,
        TerminalGatewayRoute::Bluetooth => 2,
    }
}

fn format_page() -> Result<Page, Error> {
    Ok(Page::new(Address::new(8)?, 40)?)
}

/// Registry-selected fields and complete immutable guard pages.
struct Guards {
    slot: SlotIndex,
    route: TerminalGatewayRoute,
    before: DvGatewayMode,
    gateway: ScopedMenuField<'static>,
    subtype: ScopedMenuField<'static>,
    pages: Vec<Page>,
}

impl Guards {
    fn read(snapshot: &MenuFieldSnapshot) -> Result<Self, TerminalPlanError> {
        // Format is not a menu entry; its complete canonical fragment is guarded.
        let format_page = format_page()?;
        let format = *captured_page(snapshot, format_page, "memory format")?
            .get(2)
            .ok_or(TerminalPlanError::GuardDescriptor {
                field: "memory format",
            })?;
        if format != 0 {
            return Err(TerminalPlanError::MemoryFormat { actual: format });
        }
        let pm = selection(PM_FIELD, None)?;
        let raw = unsigned(snapshot, pm)?;
        let slot = u8::try_from(raw)
            .ok()
            .and_then(|value| SlotIndex::new(value).ok())
            .ok_or(TerminalPlanError::GuardValue {
                field: PM_FIELD,
                actual: raw,
            })?;
        let usb = selection(USB_FIELD, Some(slot))?;
        let _usb = require_value(snapshot, usb, &[0])?;
        let route_selection = selection(ROUTE_FIELD, Some(slot))?;
        let route = match require_value(snapshot, route_selection, &[0, 1, 2])? {
            0 => TerminalGatewayRoute::MainUnit,
            1 => TerminalGatewayRoute::ControlPanel,
            _ => TerminalGatewayRoute::Bluetooth,
        };
        let gateway = selection(GATEWAY_FIELD, Some(slot))?;
        let before = match require_value(snapshot, gateway, &[0, 2])? {
            0 => DvGatewayMode::Off,
            _ => DvGatewayMode::Terminal,
        };
        let subtype = selection(SUBTYPE_FIELD, Some(slot))?;
        let allowed_subtypes: &[u64] = if before == DvGatewayMode::Terminal {
            &[0]
        } else {
            &[0, 1]
        };
        let _subtype = require_value(snapshot, subtype, allowed_subtypes)?;
        let mut pages = vec![format_page];
        for field in [pm, usb, route_selection, gateway, subtype] {
            pages.extend(field.pages()?);
        }
        Ok(Self {
            slot,
            route,
            before,
            gateway,
            subtype,
            pages,
        })
    }
}

fn selection(
    name: &'static str,
    slot: Option<SlotIndex>,
) -> Result<ScopedMenuField<'static>, TerminalPlanError> {
    let field = menu_field(name).ok_or(TerminalPlanError::GuardDescriptor { field: name })?;
    Ok(ScopedMenuField::new(field, slot)?)
}

fn unsigned(
    snapshot: &MenuFieldSnapshot,
    field: ScopedMenuField<'_>,
) -> Result<u64, TerminalPlanError> {
    match snapshot.value(field)? {
        DecodedFieldValue::Unsigned(raw) => Ok(raw),
        _ => Err(TerminalPlanError::GuardDescriptor {
            field: field.field().descriptor.name,
        }),
    }
}

fn require_value(
    snapshot: &MenuFieldSnapshot,
    field: ScopedMenuField<'_>,
    allowed: &[u64],
) -> Result<u64, TerminalPlanError> {
    let actual = unsigned(snapshot, field)?;
    if allowed.contains(&actual) {
        Ok(actual)
    } else {
        Err(TerminalPlanError::GuardValue {
            field: field.field().descriptor.name,
            actual,
        })
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

/// Typed refusal of a Terminal plan, its reversal, or a guarded session operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TerminalPlanError {
    /// The source identity is outside the exact admitted software-layout tuple.
    #[error("Terminal plans require TM-D750 firmware 1.02 and type K,2,1")]
    UnsupportedIdentity {
        /// Supplied identity, without guessing the meanings of its components.
        actual: Identity,
    },
    /// The current session differs from the immutable plan identity.
    #[error("current Terminal session identity differs from the plan identity")]
    IdentityMismatch {
        /// Identity bound to the captured original pages.
        expected: Identity,
        /// Identity observed by the current session.
        actual: Identity,
    },
    /// A required registered field is absent or has an unsupported decoded type.
    #[error("Terminal guard {field} has an unsupported descriptor")]
    GuardDescriptor {
        /// Required guard name.
        field: &'static str,
    },
    /// The captured image format is unsupported.
    #[error("Terminal plans require memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Captured byte at address ten.
        actual: u8,
    },
    /// A captured scalar is unknown or excluded by this narrow lifecycle.
    #[error("Terminal lifecycle does not admit {field} value {actual}")]
    GuardValue {
        /// Registered guard whose supported values were not satisfied.
        field: &'static str,
        /// Exact captured value, never normalized or replaced with a default.
        actual: u64,
    },
    /// A descriptor, snapshot, or field patch failed structural validation.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// Page construction or a shared MCP lifecycle operation failed.
    #[error(transparent)]
    Operation(#[from] Error),
}

#[cfg(test)]
mod tests;
