//! Structural discovery of the obfuscated anchors (spec D4).
//!
//! Every function here yields exactly one candidate or fails naming the
//! class, method, and line, so a changed decompilation shape stops
//! extraction instead of silently changing the manifest.

use std::collections::BTreeSet;
use std::path::Path;

use crate::address::SlotSymbol;
use crate::class_index::{ClassIndex, ClassInfo};
use crate::csharp::{Patterns, fancy_captures, find_balanced_body};
use crate::error::{Result, extract_error};
use crate::model::ModelSpec;
use crate::sources::{Sources, parse_types, read_sources};

/// A located method with its brace-balanced body.
#[derive(Debug, Clone)]
pub(crate) struct MethodRef {
    /// Dotted class name.
    pub(crate) class: String,
    /// Method name.
    pub(crate) method: String,
    /// Type of the `A_0` parameter.
    pub(crate) parameter_type: String,
    /// Manifest signature text, e.g. `a6(n7 A_0)` or `b(n7 A_0, int A_1)`.
    pub(crate) signature: String,
    /// 1-based line of the opening brace.
    pub(crate) line: usize,
    /// Body text between the braces.
    pub(crate) body: String,
}

/// A statement-form call into another serializer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NestedCall {
    /// Target expression as written.
    pub(crate) target: String,
    /// Method name.
    pub(crate) method: String,
    /// Index argument, when present.
    pub(crate) index_expression: Option<String>,
}

impl NestedCall {
    /// `(field, Some(index))` for `field[idx]`, `(field, None)` for `field`; `this.` removed.
    #[must_use]
    pub(crate) fn split_target(&self) -> (String, Option<String>) {
        let bare = self.target.strip_prefix("this.").unwrap_or(&self.target);
        match bare.split_once('[') {
            Some((field, rest)) => (
                field.to_owned(),
                Some(rest.trim_end_matches(']').to_owned()),
            ),
            None => (bare.to_owned(), None),
        }
    }
}

/// A per-slot detail class attached to a menu serializer.
#[derive(Debug, Clone)]
pub(crate) struct DetailInfo {
    /// Dotted class name.
    pub(crate) class: String,
    /// The detail's writer method.
    pub(crate) write: MethodRef,
    /// The serializer's `List<T>` field holding the details.
    pub(crate) list_field: String,
    /// Slot symbols the detail defines, one per anchor it declares.
    pub(crate) slots: Vec<SlotSymbol>,
}

/// One discovered menu serializer.
#[derive(Debug, Clone)]
pub(crate) struct MenuInfo {
    /// Spec key.
    pub(crate) key: &'static str,
    /// Spec container property.
    pub(crate) property: &'static str,
    /// Serializer class.
    pub(crate) class: String,
    /// Serializer writer.
    pub(crate) write: MethodRef,
    /// Per-slot detail, when the serializer loops over one.
    pub(crate) detail: Option<DetailInfo>,
}

/// Everything discovered from the sources for one spec.
#[derive(Debug, Clone)]
pub(crate) struct Discovered {
    /// Memory-map container class.
    pub(crate) container: String,
    /// Memory writer class (the `A_0` parameter type).
    pub(crate) writer_class: String,
    /// Language resource singleton class.
    pub(crate) resource_class: String,
    /// Menus in spec order.
    pub(crate) menus: Vec<MenuInfo>,
}

/// Every statement-form nested call in a writer body, in source order.
pub(crate) fn nested_calls(patterns: &Patterns, body: &str) -> Result<Vec<NestedCall>> {
    let mut calls = Vec::new();
    for capture in fancy_captures(&patterns.nested_call_line_re, body)? {
        calls.push(NestedCall {
            target: capture
                .get(1)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_owned(),
            method: capture
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_owned(),
            index_expression: capture
                .get(3)
                .map(|m| m.as_str().trim().to_owned())
                .filter(|text| !text.is_empty()),
        });
    }
    Ok(calls)
}

/// Declared type of a private field, if the class declares it.
pub(crate) fn field_type(patterns: &Patterns, class: &ClassInfo, field: &str) -> Option<String> {
    patterns
        .private_field_decl_re
        .captures_iter(&class.own_text)
        .find(|capture| capture.get(2).map(|m| m.as_str()) == Some(field))
        .and_then(|capture| capture.get(1).map(|m| m.as_str().trim().to_owned()))
}

/// `T` of a `List<T>` type text.
#[must_use]
pub(crate) fn list_element(type_text: &str) -> Option<&str> {
    type_text
        .strip_prefix("List<")
        .and_then(|rest| rest.strip_suffix('>'))
}

fn single_param_methods(patterns: &Patterns, class: &ClassInfo) -> Result<Vec<MethodRef>> {
    let mut methods = Vec::new();
    for capture in patterns
        .single_param_method_re
        .captures_iter(&class.own_text)
    {
        let method = capture
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let parameter_type = capture
            .get(2)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let signature_pattern = format!(
            r"^\s*public\s+(?:override\s+)?void\s+{}\s*\(\s*{}\s+A_0\s*\)",
            regex::escape(&method),
            regex::escape(&parameter_type)
        );
        let (body, line) = find_balanced_body(&class.own_text, &signature_pattern)?;
        methods.push(MethodRef {
            class: class.name.clone(),
            method: method.clone(),
            signature: format!("{method}({parameter_type} A_0)"),
            parameter_type,
            line,
            body,
        });
    }
    Ok(methods)
}

fn is_writer(
    index: &ClassIndex,
    class: &ClassInfo,
    method: &MethodRef,
    patterns: &Patterns,
    depth: usize,
) -> Result<bool> {
    if patterns.direct_statement_re.is_match(&method.body) {
        return Ok(true);
    }
    if depth > 8 {
        return Err(extract_error!(
            "writer discovery recursed too deep at {}.{}",
            class.name,
            method.method
        ));
    }
    let calls: Vec<NestedCall> = nested_calls(patterns, &method.body)?
        .into_iter()
        .filter(|call| call.index_expression.is_none())
        .collect();
    if calls.is_empty() {
        return Ok(false);
    }
    for call in calls {
        let (field, _) = call.split_target();
        let Some(declared) = field_type(patterns, class, &field) else {
            return Ok(false);
        };
        let element = list_element(&declared).unwrap_or(&declared).to_owned();
        let Some(target) = index.resolve(class, &element) else {
            return Ok(false);
        };
        let Some(candidate) = single_param_methods(patterns, target)?
            .into_iter()
            .find(|candidate| candidate.method == call.method)
        else {
            return Ok(false);
        };
        if !is_writer(index, target, &candidate, patterns, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Rule D4.2: the unique single-parameter writer method of a class.
pub(crate) fn find_writer(
    index: &ClassIndex,
    class: &ClassInfo,
    patterns: &Patterns,
) -> Result<MethodRef> {
    let mut writers = Vec::new();
    for method in single_param_methods(patterns, class)? {
        if is_writer(index, class, &method, patterns, 0)? {
            writers.push(method);
        }
    }
    match writers.len() {
        1 => writers
            .pop()
            .ok_or_else(|| extract_error!("writer candidate vanished in {}", class.name)),
        0 => Err(extract_error!(
            "no writer method found in class {} ({})",
            class.name,
            class.label
        )),
        _ => Err(extract_error!(
            "ambiguous writer methods in class {}: {}",
            class.name,
            writers
                .iter()
                .map(|writer| format!("{} at line {}", writer.signature, writer.line))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Rule D4.5 helper: the `(W A_0, int A_1)` child writer named `method`.
pub(crate) fn child_writer(
    patterns: &Patterns,
    class: &ClassInfo,
    method: &str,
    writer_class: &str,
) -> Result<MethodRef> {
    let _ = patterns;
    let signature_pattern = format!(
        r"^\s*public\s+(?:override\s+)?void\s+{}\s*\(\s*{}\s+A_0\s*,\s*int\s+A_1\s*\)",
        regex::escape(method),
        regex::escape(writer_class)
    );
    let (body, line) = find_balanced_body(&class.own_text, &signature_pattern)
        .map_err(|error| extract_error!("{error} in class {}", class.name))?;
    Ok(MethodRef {
        class: class.name.clone(),
        method: method.to_owned(),
        parameter_type: writer_class.to_owned(),
        signature: format!("{method}({writer_class} A_0, int A_1)"),
        line,
        body,
    })
}

/// Rule D4.2: verify the writer class allocates the spec image length.
fn verify_image_length(patterns: &Patterns, writer: &ClassInfo, expected: u64) -> Result<()> {
    let found = patterns
        .byte_array_alloc_re
        .captures_iter(&writer.own_text)
        .filter_map(|capture| capture.get(1)?.as_str().parse::<u64>().ok())
        .any(|length| length == expected);
    if found {
        Ok(())
    } else {
        Err(extract_error!(
            "writer class {} never allocates new byte[{expected}]",
            writer.name
        ))
    }
}

/// Rule D4.3: the unique `X` in `DisplayMember = X.Instance.`.
fn resource_class(patterns: &Patterns, sources: &Sources) -> Result<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for (_, source) in sources {
        for capture in patterns.display_member_instance_re.captures_iter(source) {
            let _fresh = names.insert(
                capture
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
    }
    let mut names: Vec<String> = names.into_iter().collect();
    match names.len() {
        1 => names
            .pop()
            .ok_or_else(|| extract_error!("resource class vanished")),
        0 => Err(extract_error!(
            "no DisplayMember = X.Instance. reference found; cannot discover the resource class"
        )),
        _ => Err(extract_error!(
            "ambiguous resource classes: {}",
            names.join(", ")
        )),
    }
}

/// The field a public `int` property's setter assigns from `value`, if the
/// class declares that property.
pub(crate) fn setter_symbol(
    patterns: &Patterns,
    class: &ClassInfo,
    property: &str,
) -> Result<Option<String>> {
    let property_pattern = format!(r"^\s*public\s+int\s+{}\s*$", regex::escape(property));
    let header = regex::Regex::new(&format!("(?m){property_pattern}"))
        .map_err(|error| extract_error!("property pattern failed to compile: {error}"))?;
    if !header.is_match(&class.own_text) {
        return Ok(None);
    }
    let (body, _) = find_balanced_body(&class.own_text, &property_pattern)?;
    let (setter, _) = find_balanced_body(&body, r"^\s*set\s*$")
        .map_err(|_| extract_error!("property {property} in {} has no setter", class.name))?;
    let assigned: Vec<String> = patterns
        .setter_assign_re
        .captures_iter(&setter)
        .filter_map(|capture| capture.get(1).map(|m| m.as_str().to_owned()))
        .collect();
    match assigned.as_slice() {
        [symbol] => Ok(Some(symbol.clone())),
        _ => Err(extract_error!(
            "property {property} in {} must assign exactly one field from value, found {assigned:?}",
            class.name
        )),
    }
}

/// Rule D4.4: slot symbols for every anchor property a class declares.
pub(crate) fn slot_symbols(
    patterns: &Patterns,
    class: &ClassInfo,
    spec: &ModelSpec,
) -> Result<Vec<SlotSymbol>> {
    let mut symbols = Vec::new();
    for dimension in spec.dimensions {
        for anchor in dimension.anchors {
            if let Some(symbol) = setter_symbol(patterns, class, anchor.property)? {
                symbols.push(SlotSymbol {
                    symbol,
                    anchor: anchor.property.to_owned(),
                    dimension: dimension.name.to_owned(),
                    stride: anchor.stride,
                });
            }
        }
    }
    Ok(symbols)
}

/// Verify `list[i].<anchor> = <stride> * i;` appears in the owner.
pub(crate) fn verify_anchor_assignment(
    owner: &ClassInfo,
    list_field: &str,
    anchor: &str,
    stride: u64,
) -> Result<()> {
    let pattern = regex::Regex::new(&format!(
        r"(?:this\.)?{}\[([@\w]+)\]\.{}\s*=\s*(\d+)\s*\*\s*([@\w]+)\s*;",
        regex::escape(list_field),
        regex::escape(anchor)
    ))
    .map_err(|error| extract_error!("anchor assignment pattern failed to compile: {error}"))?;
    for capture in pattern.captures_iter(&owner.own_text) {
        let index = capture.get(1).map(|m| m.as_str());
        let multiplier = capture.get(3).map(|m| m.as_str());
        let found: Option<u64> = capture.get(2).and_then(|m| m.as_str().parse().ok());
        if index == multiplier {
            return match found {
                Some(value) if value == stride => Ok(()),
                other => Err(extract_error!(
                    "{} assigns {anchor} with stride {other:?}, spec expects {stride}",
                    owner.name
                )),
            };
        }
    }
    Err(extract_error!(
        "{} never assigns {list_field}[i].{anchor} = {stride} * i",
        owner.name
    ))
}

/// Verify `target[i].<anchor> = <owner_symbol>;` appears in the owner.
pub(crate) fn verify_anchor_passthrough(
    owner: &ClassInfo,
    target: &str,
    anchor: &str,
    owner_symbol: &str,
) -> Result<()> {
    let pattern = regex::Regex::new(&format!(
        r"(?:this\.)?{}(?:\[[@\w]+\])?\.{}\s*=\s*(?:this\.)?{}\s*;",
        regex::escape(target),
        regex::escape(anchor),
        regex::escape(owner_symbol)
    ))
    .map_err(|error| extract_error!("anchor passthrough pattern failed to compile: {error}"))?;
    if pattern.is_match(&owner.own_text) {
        Ok(())
    } else {
        Err(extract_error!(
            "{} never assigns {target}.{anchor} = {owner_symbol}",
            owner.name
        ))
    }
}

/// The literal the owner assigns to `target[i].<property>`.
pub(crate) fn find_base_override(owner: &ClassInfo, target: &str, property: &str) -> Result<u64> {
    let pattern = regex::Regex::new(&format!(
        r"(?:this\.)?{}(?:\[[@\w]+\])?\.{}\s*=\s*(\d+)\s*;",
        regex::escape(target),
        regex::escape(property)
    ))
    .map_err(|error| extract_error!("base override pattern failed to compile: {error}"))?;
    let values: BTreeSet<u64> = pattern
        .captures_iter(&owner.own_text)
        .filter_map(|capture| capture.get(1)?.as_str().parse().ok())
        .collect();
    let mut values: Vec<u64> = values.into_iter().collect();
    match values.len() {
        1 => values
            .pop()
            .ok_or_else(|| extract_error!("base override vanished")),
        0 => Err(extract_error!(
            "{} never assigns {target}.{property} = <literal>",
            owner.name
        )),
        _ => Err(extract_error!(
            "{} assigns {target}.{property} several literals: {values:?}",
            owner.name
        )),
    }
}

/// A resolved record-list target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListTarget {
    /// Public list property.
    pub(crate) property: String,
    /// Element class as written in the `List<T>` type.
    pub(crate) element_class: String,
    /// Private backing field the property's getter returns; owners assign
    /// anchors and base overrides through this field.
    pub(crate) field: String,
}

/// The single distinct `return <field>;` of a property's getter body.
fn getter_field(
    patterns: &Patterns,
    class: &ClassInfo,
    body: &str,
    property: &str,
) -> Result<String> {
    let returns: BTreeSet<&str> = patterns
        .return_field_re
        .captures_iter(body)
        .filter_map(|capture| capture.get(1).map(|m| m.as_str()))
        .collect();
    let mut returns: Vec<&str> = returns.into_iter().collect();
    match returns.len() {
        1 => Ok(returns.pop().unwrap_or_default().to_owned()),
        _ => Err(extract_error!(
            "{}.{property} getter must return exactly one field, found {returns:?}",
            class.name
        )),
    }
}

/// Rule D4.5: map `X[i]` to its public list property, element class, and
/// backing field, whether `X` is the property or the field.
pub(crate) fn resolve_list_target(
    patterns: &Patterns,
    class: &ClassInfo,
    target: &str,
) -> Result<ListTarget> {
    let bare = target.strip_prefix("this.").unwrap_or(target);
    let name = bare.split_once('[').map_or(bare, |(field, _)| field);
    let types = parse_types(patterns, &class.name, &class.own_text)?;
    if let Some(declared) = types.properties.get(name) {
        let element = list_element(declared)
            .ok_or_else(|| extract_error!("{}.{name} is not a List<T> property", class.name))?;
        let header = format!(
            r"^\s*public\s+List<{}>\s+{}\s*$",
            regex::escape(element),
            regex::escape(name)
        );
        let (body, _) = find_balanced_body(&class.own_text, &header)?;
        return Ok(ListTarget {
            property: name.to_owned(),
            element_class: element.to_owned(),
            field: getter_field(patterns, class, &body, name)?,
        });
    }
    let mut matches = Vec::new();
    for capture in patterns.list_property_re.captures_iter(&class.own_text) {
        let element = capture
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let property = capture
            .get(2)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let header = format!(
            r"^\s*public\s+List<{}>\s+{}\s*$",
            regex::escape(&element),
            regex::escape(&property)
        );
        let (body, _) = find_balanced_body(&class.own_text, &header)?;
        if getter_field(patterns, class, &body, &property).is_ok_and(|returned| returned == name) {
            matches.push(ListTarget {
                property,
                element_class: element,
                field: name.to_owned(),
            });
        }
    }
    match matches.len() {
        1 => matches
            .pop()
            .ok_or_else(|| extract_error!("list target vanished")),
        0 => Err(extract_error!(
            "{}: no public List<T> property returns field {name}",
            class.name
        )),
        _ => Err(extract_error!(
            "{}: several public List<T> properties return field {name}: {}",
            class.name,
            matches
                .iter()
                .map(|m| m.property.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn find_serializer(index: &ClassIndex, property: &str) -> Result<(String, String)> {
    let pattern = regex::Regex::new(&format!(
        r"(?m)^\s*public\s+([@\w]+)\s+{}\s*$",
        regex::escape(property)
    ))
    .map_err(|error| extract_error!("menu property pattern failed to compile: {error}"))?;
    let mut hits: Vec<(String, String)> = Vec::new();
    for class in index.iter() {
        for capture in pattern.captures_iter(&class.own_text) {
            hits.push((
                class.name.clone(),
                capture
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            ));
        }
    }
    match hits.len() {
        1 => hits
            .pop()
            .ok_or_else(|| extract_error!("menu property vanished")),
        0 => Err(extract_error!(
            "cannot map {property} to its decompiled class; the source directory must include the MemoryMap container"
        )),
        _ => Err(extract_error!(
            "menu property {property} is declared by several classes: {}",
            hits.iter()
                .map(|(class, _)| class.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A per-slot detail candidate found in a serializer's writer body.
struct DetailCandidate<'a> {
    field: String,
    method: String,
    target: &'a ClassInfo,
    slots: Vec<SlotSymbol>,
}

fn detail_candidates<'a>(
    index: &'a ClassIndex,
    serializer: &ClassInfo,
    write: &MethodRef,
    spec: &ModelSpec,
    patterns: &Patterns,
) -> Result<Vec<DetailCandidate<'a>>> {
    let mut candidates = Vec::new();
    for call in nested_calls(patterns, &write.body)? {
        let (field, index_expression) = call.split_target();
        if index_expression.is_none() || call.index_expression.is_some() {
            continue;
        }
        let Some(declared) = field_type(patterns, serializer, &field) else {
            continue;
        };
        let Some(element) = list_element(&declared) else {
            continue;
        };
        let target = index.resolve(serializer, element).ok_or_else(|| {
            extract_error!("detail class {element} of {} not found", serializer.name)
        })?;
        let slots = slot_symbols(patterns, target, spec)?;
        if slots.is_empty() {
            continue;
        }
        candidates.push(DetailCandidate {
            field,
            method: call.method,
            target,
            slots,
        });
    }
    Ok(candidates)
}

fn discover_detail(
    index: &ClassIndex,
    serializer: &ClassInfo,
    write: &MethodRef,
    spec: &ModelSpec,
    patterns: &Patterns,
) -> Result<Option<DetailInfo>> {
    let mut candidates = detail_candidates(index, serializer, write, spec, patterns)?;
    let Some(candidate) = candidates.pop() else {
        return Ok(None);
    };
    if !candidates.is_empty() {
        return Err(extract_error!(
            "{} loops over several per-slot lists",
            serializer.name
        ));
    }
    let write = find_writer(index, candidate.target, patterns)?;
    if write.method != candidate.method {
        return Err(extract_error!(
            "{} calls {}.{} but that class's writer is {}",
            serializer.name,
            candidate.target.name,
            candidate.method,
            write.method
        ));
    }
    let first_anchor = spec
        .dimensions
        .first()
        .and_then(|dimension| dimension.anchors.first())
        .ok_or_else(|| extract_error!("detail class found but the spec declares no dimension"))?;
    if !candidate
        .slots
        .iter()
        .any(|slot| slot.anchor == first_anchor.property)
    {
        return Err(extract_error!(
            "detail class {} does not define the mandatory anchor {}",
            candidate.target.name,
            first_anchor.property
        ));
    }
    for slot in &candidate.slots {
        verify_anchor_assignment(serializer, &candidate.field, &slot.anchor, slot.stride)?;
    }
    Ok(Some(DetailInfo {
        class: candidate.target.name.clone(),
        write,
        list_field: candidate.field,
        slots: candidate.slots,
    }))
}

/// Discover the container, serializers, writers, resource class, and details.
pub(crate) fn discover(
    index: &ClassIndex,
    sources: &Sources,
    spec: &ModelSpec,
    patterns: &Patterns,
) -> Result<Discovered> {
    let mut container: Option<String> = None;
    let mut menus = Vec::new();
    let mut writer_class: Option<String> = None;
    for menu in spec.menus {
        let (owner, class_name) = find_serializer(index, menu.property)?;
        match &container {
            Some(existing) if *existing != owner => {
                return Err(extract_error!(
                    "menu properties are split across containers {existing} and {owner}"
                ));
            }
            Some(_) => {}
            None => container = Some(owner),
        }
        let class = index.get(&class_name).ok_or_else(|| {
            extract_error!(
                "source for class {class_name} ({}) not found",
                menu.property
            )
        })?;
        let write = find_writer(index, class, patterns)?;
        match &writer_class {
            Some(existing) if *existing != write.parameter_type => {
                return Err(extract_error!(
                    "serializers disagree on the writer class: {existing} versus {}",
                    write.parameter_type
                ));
            }
            Some(_) => {}
            None => writer_class = Some(write.parameter_type.clone()),
        }
        let detail = discover_detail(index, class, &write, spec, patterns)?;
        menus.push(MenuInfo {
            key: menu.key,
            property: menu.property,
            class: class_name,
            write,
            detail,
        });
    }
    let writer_class = writer_class.ok_or_else(|| extract_error!("spec declares no menus"))?;
    let writer = index
        .get(&writer_class)
        .ok_or_else(|| extract_error!("writer class {writer_class} has no source"))?;
    verify_image_length(patterns, writer, spec.image_length)?;
    Ok(Discovered {
        container: container.ok_or_else(|| extract_error!("no container discovered"))?,
        writer_class,
        resource_class: resource_class(patterns, sources)?,
        menus,
    })
}

/// One discovered menu in a [`DiscoveredSummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMenu {
    /// Spec menu key.
    pub key: String,
    /// Public container property.
    pub property: String,
    /// Serializer class.
    pub class: String,
    /// Serializer writer signature.
    pub write_method: String,
    /// Line of the writer's opening brace.
    pub write_line: usize,
    /// Per-slot detail class, when the serializer loops over one.
    pub detail_class: Option<String>,
    /// Detail writer signature.
    pub detail_write_method: Option<String>,
    /// `symbol=anchor*stride` for each slot symbol the detail defines.
    pub slot_symbols: Vec<String>,
    /// Public record list properties the writer (or its detail) iterates.
    pub record_lists: Vec<String>,
}

/// Public list properties targeted by indexed nested calls in `body`.
fn record_lists(patterns: &Patterns, owner: &ClassInfo, body: &str) -> Result<Vec<String>> {
    let mut lists = Vec::new();
    for call in nested_calls(patterns, body)? {
        if call.index_expression.is_none() {
            continue;
        }
        if let Ok(target) = resolve_list_target(patterns, owner, &call.target) {
            lists.push(target.property);
        }
    }
    Ok(lists)
}

/// Obfuscated anchors discovered for one model in an `ILSpy` project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSummary {
    /// Memory-map container class.
    pub container: String,
    /// Memory writer class.
    pub writer_class: String,
    /// Language resource singleton class.
    pub resource_class: String,
    /// Menus in spec order.
    pub menus: Vec<DiscoveredMenu>,
}

/// Discover a model's anchors in an `ILSpy` project directory.
///
/// # Errors
///
/// Returns an error when the sources cannot be read or when any discovery
/// rule finds zero or several candidates.
pub fn discover_project(source_dir: &Path, spec: &ModelSpec) -> Result<DiscoveredSummary> {
    let patterns = Patterns::new()?;
    let sources = read_sources(source_dir)?;
    let index = ClassIndex::build(&patterns, &sources, source_dir)?;
    let discovered = discover(&index, &sources, spec, &patterns)?;
    let mut menus = Vec::new();
    for menu in &discovered.menus {
        let owner = index
            .get(&menu.write.class)
            .ok_or_else(|| extract_error!("writer class {} vanished", menu.write.class))?;
        let mut lists = record_lists(&patterns, owner, &menu.write.body)?;
        if let Some(detail) = &menu.detail {
            let detail_class = index
                .get(&detail.write.class)
                .ok_or_else(|| extract_error!("detail class {} vanished", detail.write.class))?;
            lists.extend(record_lists(&patterns, detail_class, &detail.write.body)?);
        }
        menus.push(DiscoveredMenu {
            key: menu.key.to_owned(),
            property: menu.property.to_owned(),
            class: menu.class.clone(),
            write_method: menu.write.signature.clone(),
            write_line: menu.write.line,
            detail_class: menu.detail.as_ref().map(|detail| detail.class.clone()),
            detail_write_method: menu
                .detail
                .as_ref()
                .map(|detail| detail.write.signature.clone()),
            slot_symbols: menu.detail.as_ref().map_or_else(Vec::new, |detail| {
                detail
                    .slots
                    .iter()
                    .map(|slot| format!("{}={}*{}", slot.symbol, slot.anchor, slot.stride))
                    .collect()
            }),
            record_lists: lists,
        });
    }
    Ok(DiscoveredSummary {
        container: discovered.container,
        writer_class: discovered.writer_class,
        resource_class: discovered.resource_class,
        menus,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::model::THD75;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    const CONTAINER: &str = "public class m7\n{\n\tpublic m9 RadioMenuData\n\t{\n\t\tget { return null; }\n\t}\n\tpublic m1 GpsMenuData\n\t{\n\t\tget { return null; }\n\t}\n\tpublic l4 AprsMenuData\n\t{\n\t\tget { return null; }\n\t}\n\tpublic mu DvMenuData\n\t{\n\t\tget { return null; }\n\t}\n}\n";
    const WRITER: &str = "public class m6\n{\n\tprivate byte[] m_a;\n\tpublic m6()\n\t{\n\t\tthis.m_a = new byte[500480];\n\t}\n\tpublic void a(byte A_0, int A_1)\n\t{\n\t}\n}\n";
    const RADIO: &str = "public class m9\n{\n\tpublic bool TxInhibit\n\t{\n\t\tget { return false; }\n\t}\n\tpublic void a0(m6 A_0)\n\t{\n\t\tA_0.a(TxInhibit, 4097);\n\t}\n\tpublic void a1(m6 A_0)\n\t{\n\t\tTxInhibit = A_0.a(4097) != 0;\n\t}\n}\n";
    const SIMPLE: &str = "public class {name}\n{\n\tpublic byte X\n\t{\n\t\tget { return 0; }\n\t}\n\tpublic void a0(m6 A_0)\n\t{\n\t\tA_0.a(X, 5000);\n\t}\n}\n";
    const COMBO: &str = "public class combo\n{\n\tpublic void Build()\n\t{\n\t\tvar x = new gd { Value = m9.a.a, DisplayMember = kb.Instance.Key };\n\t}\n}\n";

    fn sources() -> Sources {
        vec![
            (PathBuf::from("m7.cs"), CONTAINER.to_owned()),
            (PathBuf::from("m6.cs"), WRITER.to_owned()),
            (PathBuf::from("m9.cs"), RADIO.to_owned()),
            (PathBuf::from("m1.cs"), SIMPLE.replace("{name}", "m1")),
            (PathBuf::from("l4.cs"), SIMPLE.replace("{name}", "l4")),
            (PathBuf::from("mu.cs"), SIMPLE.replace("{name}", "mu")),
            (PathBuf::from("combo.cs"), COMBO.to_owned()),
        ]
    }

    #[test]
    fn discovers_thd75_shape_without_pinned_names() -> TestResult {
        let patterns = Patterns::new()?;
        let sources = sources();
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let discovered = discover(&index, &sources, &THD75, &patterns)?;
        assert_eq!(discovered.container, "m7");
        assert_eq!(discovered.writer_class, "m6");
        assert_eq!(discovered.resource_class, "kb");
        let radio = discovered.menus.first().ok_or("no menus")?;
        assert_eq!(radio.class, "m9");
        assert_eq!(radio.write.method, "a0");
        assert_eq!(radio.write.signature, "a0(m6 A_0)");
        assert!(radio.detail.is_none());
        Ok(())
    }

    #[test]
    fn discover_project_reads_a_directory() -> TestResult {
        let temporary = tempfile::tempdir()?;
        for (path, source) in sources() {
            std::fs::write(temporary.path().join(path), source)?;
        }
        let summary = discover_project(temporary.path(), &THD75)?;
        assert_eq!(summary.writer_class, "m6");
        assert_eq!(summary.menus.len(), 4);
        let radio = summary.menus.first().ok_or("no menus")?;
        assert_eq!(radio.write_method, "a0(m6 A_0)");
        assert!(radio.detail_class.is_none());
        assert!(radio.slot_symbols.is_empty());
        assert!(radio.record_lists.is_empty());
        Ok(())
    }

    #[test]
    fn writer_discovery_rejects_ambiguity_and_absence() -> TestResult {
        let patterns = Patterns::new()?;
        let two_writers = "public class zz\n{\n\tpublic byte X\n\t{\n\t\tget { return 0; }\n\t}\n\tpublic void a0(m6 A_0)\n\t{\n\t\tA_0.a(X, 1);\n\t}\n\tpublic void a2(m6 A_0)\n\t{\n\t\tA_0.a(X, 2);\n\t}\n}\n";
        let sources: Sources = vec![(PathBuf::from("zz.cs"), two_writers.to_owned())];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let class = index.get("zz").ok_or("zz missing")?;
        let result = find_writer(&index, class, &patterns);
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("ambiguous writer")),
            "expected ambiguity error, got {result:?}"
        );
        let reader_only =
            "public class yy\n{\n\tpublic void a1(m6 A_0)\n\t{\n\t\tX = A_0.a(1);\n\t}\n}\n";
        let sources: Sources = vec![(PathBuf::from("yy.cs"), reader_only.to_owned())];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let class = index.get("yy").ok_or("yy missing")?;
        let result = find_writer(&index, class, &patterns);
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("no writer method")),
            "expected absence error, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn composition_only_writers_resolve_through_their_details() -> TestResult {
        let patterns = Patterns::new()?;
        let serializer = "public class mv\n{\n\tprivate List<ms> m_j;\n\tpublic void a6(n7 A_0)\n\t{\n\t\tthis.m_j[num3].a6(A_0);\n\t}\n\tpublic void a7(n7 A_0)\n\t{\n\t\tthis.m_j[num3].a7(A_0);\n\t}\n}\n";
        let detail = "public class ms\n{\n\tprivate int r;\n\tpublic int OffsetProgrammableMemoryAddress\n\t{\n\t\tset\n\t\t{\n\t\t\tr = value;\n\t\t}\n\t}\n\tpublic bool QsyInStatus\n\t{\n\t\tget { return false; }\n\t}\n\tpublic void a6(n7 A_0)\n\t{\n\t\tA_0.a(QsyInStatus, 329825 + r);\n\t}\n\tpublic void a7(n7 A_0)\n\t{\n\t\tQsyInStatus = A_0.a(329825 + r) != 0;\n\t}\n}\n";
        let sources: Sources = vec![
            (PathBuf::from("mv.cs"), serializer.to_owned()),
            (PathBuf::from("ms.cs"), detail.to_owned()),
        ];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let class = index.get("mv").ok_or("mv missing")?;
        let writer = find_writer(&index, class, &patterns)?;
        assert_eq!(writer.method, "a6");
        Ok(())
    }

    #[test]
    fn nested_calls_capture_targets_and_indices() -> TestResult {
        let patterns = Patterns::new()?;
        let body = "\t\tthis.m_a.b(A_0, 0);\n\t\tu[num3].ax(A_0, num3);\n\t\td.b(A_0);\n\t\tA_0.a(X, 1);\n\t\tthis.m_l[num3].a6(A_0);\n";
        let calls = nested_calls(&patterns, body)?;
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls.get(1).map(NestedCall::split_target),
            Some(("u".to_owned(), Some("num3".to_owned())))
        );
        assert_eq!(
            calls.get(2).map(|call| call.index_expression.clone()),
            Some(None)
        );
        assert_eq!(
            calls.get(3).map(NestedCall::split_target),
            Some(("m_l".to_owned(), Some("num3".to_owned())))
        );
        Ok(())
    }

    #[test]
    fn resolves_list_targets_through_property_or_getter() -> TestResult {
        let patterns = Patterns::new()?;
        let source = "public class m1\n{\n\tprivate List<MyPositionData> u;\n\tpublic List<MyPositionData> MyPositionList\n\t{\n\t\tget\n\t\t{\n\t\t\treturn u;\n\t\t}\n\t}\n\tpublic List<ObjectData> ObjectList\n\t{\n\t\tget\n\t\t{\n\t\t\treturn v;\n\t\t}\n\t}\n}\n";
        let sources: Sources = vec![(PathBuf::from("m1.cs"), source.to_owned())];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let class = index.get("m1").ok_or("m1 missing")?;
        assert_eq!(
            resolve_list_target(&patterns, class, "u[num3]")?,
            ListTarget {
                property: "MyPositionList".to_owned(),
                element_class: "MyPositionData".to_owned(),
                field: "u".to_owned()
            }
        );
        assert_eq!(
            resolve_list_target(&patterns, class, "ObjectList[num6]")?,
            ListTarget {
                property: "ObjectList".to_owned(),
                element_class: "ObjectData".to_owned(),
                field: "v".to_owned()
            }
        );
        assert!(resolve_list_target(&patterns, class, "w[num3]").is_err());
        Ok(())
    }

    #[test]
    fn slot_symbols_and_anchor_verification() -> TestResult {
        let patterns = Patterns::new()?;
        let owner = "public class oa\n{\n\tprivate List<nl> m_l;\n\tpublic void ai()\n\t{\n\t\tthis.m_l[num3].OffsetProgrammableMemoryAddress = 8192 * num3;\n\t\tthis.m_l[num3].OffsetProgrammableMemoryBitmapAddress = 256000 * num3;\n\t}\n}\n";
        let detail = "public class nl\n{\n\tprivate int f;\n\tprivate int g;\n\tprivate c d = new c();\n\tpublic int OffsetProgrammableMemoryAddress\n\t{\n\t\tset\n\t\t{\n\t\t\tf = value;\n\t\t\td.OffsetProgrammableMemoryAddress = f;\n\t\t}\n\t}\n\tpublic int OffsetProgrammableMemoryBitmapAddress\n\t{\n\t\tset\n\t\t{\n\t\t\tg = value;\n\t\t}\n\t}\n}\n";
        let sources: Sources = vec![
            (PathBuf::from("oa.cs"), owner.to_owned()),
            (PathBuf::from("nl.cs"), detail.to_owned()),
        ];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let spec = ModelSpec {
            dimensions: &[crate::model::DimensionSpec {
                name: "pm_slot",
                count: 6,
                anchors: &[
                    crate::model::AnchorSpec {
                        property: "OffsetProgrammableMemoryAddress",
                        stride: 8192,
                    },
                    crate::model::AnchorSpec {
                        property: "OffsetProgrammableMemoryBitmapAddress",
                        stride: 256_000,
                    },
                ],
            }],
            ..THD75
        };
        let detail_class = index.get("nl").ok_or("nl missing")?;
        let slots = slot_symbols(&patterns, detail_class, &spec)?;
        let summary: Vec<(&str, &str, u64)> = slots
            .iter()
            .map(|slot| (slot.symbol.as_str(), slot.anchor.as_str(), slot.stride))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("f", "OffsetProgrammableMemoryAddress", 8192),
                ("g", "OffsetProgrammableMemoryBitmapAddress", 256_000)
            ]
        );
        let owner_class = index.get("oa").ok_or("oa missing")?;
        verify_anchor_assignment(owner_class, "m_l", "OffsetProgrammableMemoryAddress", 8192)?;
        verify_anchor_assignment(
            owner_class,
            "m_l",
            "OffsetProgrammableMemoryBitmapAddress",
            256_000,
        )?;
        assert!(
            verify_anchor_assignment(owner_class, "m_l", "OffsetProgrammableMemoryAddress", 4096)
                .is_err()
        );
        Ok(())
    }
}
