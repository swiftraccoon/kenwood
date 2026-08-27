//! Affine write addresses: a constant base plus stride-scaled dimension indices.
//!
//! Decompiled offsets come in three shapes: a literal (`4242`), a literal plus
//! a per-slot symbol (`329828 + r`), and, inside repeated-record writers, a
//! base variable plus a literal (`num + 12`). Record base assignments add a
//! stride term over the loop index `A_1` (`329232 + av + 32 * A_1`). Every
//! shape is an affine expression over symbols; this module parses them and
//! resolves symbols through a [`SymbolScope`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::csharp::compact_expression;
use crate::error::{Result, extract_error};

/// One stride-scaled index of a declared dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    /// Dimension name declared in the manifest's `dimensions` array.
    pub dimension: String,
    /// Bytes added per index step.
    pub stride: u64,
}

/// A write address: `base + sum(term.stride * index(term.dimension))`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    /// Address when every dimension index is zero.
    pub base: u64,
    /// Stride-scaled dimension terms; empty for an absolute address.
    pub terms: Vec<Term>,
}

impl Address {
    /// An address without dimension terms.
    #[must_use]
    pub const fn absolute(base: u64) -> Self {
        Self {
            base,
            terms: Vec::new(),
        }
    }

    /// True when the address has no dimension terms.
    #[must_use]
    pub const fn is_absolute(&self) -> bool {
        self.terms.is_empty()
    }

    /// Concrete offset for the given `(dimension, index)` pairs.
    ///
    /// # Errors
    ///
    /// Returns an error when a term's dimension has no index or the sum
    /// overflows `u64`.
    pub fn offset(&self, indices: &[(&str, u64)]) -> Result<u64> {
        let mut offset = self.base;
        for term in &self.terms {
            let index = indices
                .iter()
                .find(|(name, _)| *name == term.dimension)
                .map(|(_, index)| *index)
                .ok_or_else(|| {
                    extract_error!("no index supplied for dimension {}", term.dimension)
                })?;
            offset = term
                .stride
                .checked_mul(index)
                .and_then(|scaled| offset.checked_add(scaled))
                .ok_or_else(|| {
                    extract_error!("address overflow in dimension {}", term.dimension)
                })?;
        }
        Ok(offset)
    }
}

/// A parsed affine expression over the loop index `A_1` and named symbols.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Affine {
    /// Sum of the integer literals.
    pub constant: i64,
    /// Coefficient of `A_1`, when the expression mentions it.
    pub index_stride: Option<i64>,
    /// Bare or `this.`-qualified identifiers, in source order, `this.` removed.
    pub symbols: Vec<String>,
}

fn parse_literal(text: &str) -> Option<i64> {
    let (negative, digits) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let magnitude = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok()?
    } else {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<i64>().ok()?
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first == '@' || first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| {
            character == '@'
                || character == '_'
                || character == '.'
                || character.is_ascii_alphanumeric()
        })
}

/// Parse an affine expression such as `329232 + av + 32 * A_1`.
///
/// Whitespace and parentheses are ignored. Accepted terms: integer literals
/// (decimal or hex), `<n>*A_1`, `A_1`, and identifiers (optionally prefixed
/// with `this.`, which is removed; qualified names such as `oc.bb` are kept
/// whole for constant resolution).
///
/// # Errors
///
/// Returns an error for an empty expression, a subtraction, or any term
/// that is not one of the accepted shapes.
pub fn parse_affine(expression: &str) -> Result<Affine> {
    let compact = compact_expression(expression);
    if compact.is_empty() {
        return Err(extract_error!("empty offset expression"));
    }
    if compact.contains('-') {
        return Err(extract_error!(
            "unsupported subtraction in offset expression: {expression}"
        ));
    }
    let mut affine = Affine::default();
    for term in compact.split('+') {
        if term == "A_1" {
            affine.index_stride = Some(affine.index_stride.unwrap_or(0) + 1);
        } else if let Some(coefficient) = term.strip_suffix("*A_1") {
            let stride = parse_literal(coefficient).ok_or_else(|| {
                extract_error!("unparsable stride in offset expression: {expression}")
            })?;
            affine.index_stride = Some(affine.index_stride.unwrap_or(0) + stride);
        } else if let Some(literal) = parse_literal(term) {
            affine.constant = affine
                .constant
                .checked_add(literal)
                .ok_or_else(|| extract_error!("offset expression overflows: {expression}"))?;
        } else {
            let symbol = term.strip_prefix("this.").unwrap_or(term);
            if !is_identifier(symbol) {
                return Err(extract_error!(
                    "unsupported offset term {term:?} in {expression}"
                ));
            }
            affine.symbols.push(symbol.to_owned());
        }
    }
    Ok(affine)
}

/// A per-instance slot symbol: the field written by an anchor property's
/// setter, and the dimension term it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSymbol {
    /// Field name assigned by the anchor setter (`r`, `av`, `this.m_b` without `this.`).
    pub symbol: String,
    /// Anchor property whose setter assigns the symbol (`OffsetProgrammableMemoryAddress`).
    pub anchor: String,
    /// Dimension the symbol indexes.
    pub dimension: String,
    /// Stride the owning anchor assigns per index.
    pub stride: u64,
}

/// Everything an offset expression may reference.
#[derive(Debug, Clone, Copy)]
pub struct SymbolScope<'a> {
    /// `Class.member` static integer constants from the whole project.
    pub constants: &'a HashMap<String, i64>,
    /// Slot symbols of the class whose method is being read.
    pub slots: &'a [SlotSymbol],
    /// Pinned literal values of base-override symbols (`StartAddress`).
    pub overrides: &'a [(String, u64)],
}

impl SymbolScope<'_> {
    fn resolve_symbol(&self, symbol: &str, address: &mut Address, expression: &str) -> Result<()> {
        if let Some(slot) = self.slots.iter().find(|slot| slot.symbol == symbol) {
            if address
                .terms
                .iter()
                .any(|term| term.dimension == slot.dimension)
            {
                return Err(extract_error!(
                    "offset expression uses dimension {} twice: {expression}",
                    slot.dimension
                ));
            }
            address.terms.push(Term {
                dimension: slot.dimension.clone(),
                stride: slot.stride,
            });
            return Ok(());
        }
        if let Some((_, value)) = self.overrides.iter().find(|(name, _)| name == symbol) {
            address.base = address
                .base
                .checked_add(*value)
                .ok_or_else(|| extract_error!("offset expression overflows: {expression}"))?;
            return Ok(());
        }
        if let Some(value) = self.constants.get(symbol) {
            let value = u64::try_from(*value).map_err(|_| {
                extract_error!("negative constant {symbol} in offset expression: {expression}")
            })?;
            address.base = address
                .base
                .checked_add(value)
                .ok_or_else(|| extract_error!("offset expression overflows: {expression}"))?;
            return Ok(());
        }
        Err(extract_error!(
            "unresolved symbol {symbol:?} in offset expression: {expression}"
        ))
    }
}

/// Resolve a direct-write offset expression (no `A_1`) to an address.
///
/// # Errors
///
/// Returns an error when the expression mentions `A_1`, a negative
/// constant, or a symbol that is neither a slot symbol, a pinned override,
/// nor a project constant.
pub fn resolve_offset(expression: &str, scope: &SymbolScope<'_>) -> Result<Address> {
    let affine = parse_affine(expression)?;
    if affine.index_stride.is_some() {
        return Err(extract_error!(
            "record index A_1 is not allowed in a direct write offset: {expression}"
        ));
    }
    let base = u64::try_from(affine.constant)
        .map_err(|_| extract_error!("negative offset expression: {expression}"))?;
    let mut address = Address::absolute(base);
    for symbol in &affine.symbols {
        scope.resolve_symbol(symbol, &mut address, expression)?;
    }
    Ok(address)
}

/// A resolved repeated-record base formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBase {
    /// Slot-zero base of record 0.
    pub base: u64,
    /// Bytes between consecutive records.
    pub stride: u64,
    /// `(index, base)` pairs replacing the linear formula for single records.
    pub overrides: Vec<(u64, u64)>,
    /// Inherited dimension terms (slot symbols) shared by every record.
    pub terms: Vec<Term>,
}

impl RecordBase {
    /// Slot-zero base address of each record in index order.
    ///
    /// # Errors
    ///
    /// Returns an error when an override index is outside `count` or the
    /// linear formula overflows.
    pub fn bases(&self, count: u64) -> Result<Vec<u64>> {
        let mut bases = Vec::new();
        for index in 0..count {
            let base = self
                .stride
                .checked_mul(index)
                .and_then(|scaled| self.base.checked_add(scaled))
                .ok_or_else(|| extract_error!("record base overflows at index {index}"))?;
            bases.push(base);
        }
        for (index, base) in &self.overrides {
            let slot = usize::try_from(*index)
                .ok()
                .and_then(|index| bases.get_mut(index))
                .ok_or_else(|| {
                    extract_error!("record base override index {index} is outside count {count}")
                })?;
            *slot = *base;
        }
        Ok(bases)
    }
}

fn resolve_linear(
    affine: &Affine,
    scope: &SymbolScope<'_>,
    expression: &str,
) -> Result<(Address, u64)> {
    let stride = affine
        .index_stride
        .ok_or_else(|| extract_error!("record base formula has no A_1 stride: {expression}"))?;
    let stride = u64::try_from(stride)
        .map_err(|_| extract_error!("negative record stride: {expression}"))?;
    let base = u64::try_from(affine.constant)
        .map_err(|_| extract_error!("negative record base: {expression}"))?;
    let mut address = Address::absolute(base);
    for symbol in &affine.symbols {
        scope.resolve_symbol(symbol, &mut address, expression)?;
    }
    Ok((address, stride))
}

/// Split a compacted piecewise formula `A_1==k?<override>:<linear>` into its
/// override `(index, expression)` and its linear part.
fn split_piecewise(compact: &str, expression: &str) -> Result<(Option<(i64, String)>, String)> {
    let Some((condition, rest)) = compact.split_once('?') else {
        return Ok((None, compact.to_owned()));
    };
    let index = condition
        .strip_prefix("A_1==")
        .and_then(parse_literal)
        .ok_or_else(|| extract_error!("unsupported record base condition: {expression}"))?;
    let (override_expression, linear_expression) = rest
        .split_once(':')
        .ok_or_else(|| extract_error!("piecewise record base has no ':' branch: {expression}"))?;
    Ok((
        Some((index, override_expression.to_owned())),
        linear_expression.to_owned(),
    ))
}

/// Resolve a repeated-record base assignment expression.
///
/// Accepted shapes (after whitespace and parentheses removal):
/// `<affine with A_1>` (linear) and `A_1==<k>?<affine without A_1>:<affine with A_1>`
/// (linear with one override). Symbols resolve through `scope` exactly as
/// direct writes do; both branches of a piecewise formula must resolve to
/// the same dimension terms.
///
/// # Errors
///
/// Returns an error for any other shape or an unresolved symbol.
pub fn resolve_record_base(expression: &str, scope: &SymbolScope<'_>) -> Result<RecordBase> {
    let compact = compact_expression(expression);
    let (override_part, linear_part) = split_piecewise(&compact, expression)?;
    let linear = parse_affine(&linear_part)?;
    let (address, stride) = resolve_linear(&linear, scope, expression)?;
    let mut overrides = Vec::new();
    if let Some((index, override_expression)) = override_part {
        let index = u64::try_from(index)
            .map_err(|_| extract_error!("negative record override index: {expression}"))?;
        let override_affine = parse_affine(&override_expression)?;
        if override_affine.index_stride.is_some() {
            return Err(extract_error!(
                "record override branch mentions A_1: {expression}"
            ));
        }
        let override_address = resolve_offset(&override_expression, scope)?;
        if override_address.terms != address.terms {
            return Err(extract_error!(
                "record override branch resolves to different dimension terms: {expression}"
            ));
        }
        overrides.push((index, override_address.base));
    }
    Ok(RecordBase {
        base: address.base,
        stride,
        overrides,
        terms: address.terms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn slot(symbol: &str, stride: u64) -> SlotSymbol {
        SlotSymbol {
            symbol: symbol.to_owned(),
            anchor: "OffsetProgrammableMemoryAddress".to_owned(),
            dimension: "pm_slot".to_owned(),
            stride,
        }
    }

    #[test]
    fn parses_literal_and_slot_forms() -> TestResult {
        assert_eq!(
            parse_affine("4242")?,
            Affine {
                constant: 4242,
                index_stride: None,
                symbols: Vec::new()
            }
        );
        assert_eq!(
            parse_affine("329828 + r")?,
            Affine {
                constant: 329_828,
                index_stride: None,
                symbols: vec!["r".to_owned()]
            }
        );
        assert!(
            parse_affine("(A_1 == 4) ? 1 : 2").is_err(),
            "the '?' operator must be rejected by parse_affine"
        );
        assert_eq!(
            parse_affine("this.m_b + this.m_a + 96 * A_1")?,
            Affine {
                constant: 0,
                index_stride: Some(96),
                symbols: vec!["m_b".to_owned(), "m_a".to_owned()]
            }
        );
        Ok(())
    }

    #[test]
    fn resolves_direct_offsets_through_scope() -> TestResult {
        let constants = HashMap::from([("oc.bb".to_owned(), 12_i64)]);
        let slots = [slot("r", 8192), slot("g", 256_000)];
        let scope = SymbolScope {
            constants: &constants,
            slots: &slots,
            overrides: &[],
        };
        assert_eq!(resolve_offset("4242", &scope)?, Address::absolute(4242));
        assert_eq!(
            resolve_offset("329828 + r", &scope)?,
            Address {
                base: 329_828,
                terms: vec![Term {
                    dimension: "pm_slot".to_owned(),
                    stride: 8192
                }]
            }
        );
        assert_eq!(
            resolve_offset("393216 + g", &scope)?,
            Address {
                base: 393_216,
                terms: vec![Term {
                    dimension: "pm_slot".to_owned(),
                    stride: 256_000
                }]
            }
        );
        assert_eq!(resolve_offset("10 + oc.bb", &scope)?, Address::absolute(22));
        let unresolved = resolve_offset("10 + zz", &scope);
        assert!(
            unresolved.is_err(),
            "unknown symbols must fail: {unresolved:?}"
        );
        let doubled = resolve_offset("10 + r + g", &scope);
        assert!(doubled.is_err(), "two slot terms must fail: {doubled:?}");
        Ok(())
    }

    #[test]
    fn resolves_record_bases_with_slot_and_override() -> TestResult {
        let constants = HashMap::new();
        let slots = [slot("av", 8192)];
        let overrides = [("m_start".to_owned(), 334_081_u64)];
        let scope = SymbolScope {
            constants: &constants,
            slots: &slots,
            overrides: &overrides,
        };
        let linear = resolve_record_base("329232 + av + 32 * A_1", &scope)?;
        assert_eq!(linear.base, 329_232);
        assert_eq!(linear.stride, 32);
        assert_eq!(linear.terms.len(), 1);
        assert_eq!(
            linear.bases(5)?,
            vec![329_232, 329_264, 329_296, 329_328, 329_360]
        );
        let piecewise = resolve_record_base("((A_1 == 4) ? 4864 : (4656 + 48 * A_1))", &scope)?;
        assert_eq!(piecewise.overrides, vec![(4, 4864)]);
        assert_eq!(piecewise.bases(5)?, vec![4656, 4704, 4752, 4800, 4864]);
        let folded = resolve_record_base("m_start + av + 96 * A_1", &scope)?;
        assert_eq!(folded.base, 334_081);
        assert_eq!(folded.stride, 96);
        assert_eq!(folded.terms.len(), 1);
        Ok(())
    }

    #[test]
    fn concrete_offsets_apply_every_term() -> TestResult {
        let address = Address {
            base: 100,
            terms: vec![Term {
                dimension: "pm_slot".to_owned(),
                stride: 8192,
            }],
        };
        assert_eq!(address.offset(&[("pm_slot", 3)])?, 100 + 3 * 8192);
        assert!(address.offset(&[]).is_err(), "missing index must fail");
        Ok(())
    }
}
