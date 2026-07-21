//! Narrow C# text primitives for `ILSpy`'s one-line serializer call shape.

use crate::error::{ExtractError, Result, extract_error};

/// Compiled patterns mirroring the extractor's module-level regex constants.
///
/// Two patterns (`property_re`, `nested_call_re`) require negative lookahead
/// and therefore use the backtracking `fancy_regex` engine; the rest use the
/// linear-time `regex` engine.
#[derive(Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is a compiled pattern; the _re suffix mirrors the reference extractor's constant names, which the doc comments cite"
)]
pub(crate) struct Patterns {
    /// `^\s*(?:public|internal)\s+class\s+([@\w]+)` (multiline).
    pub(crate) class_re: regex::Regex,
    /// Public `RadioMenuData`/`GpsMenuData`/`AprsMenuData`/`DvMenuData` property.
    pub(crate) menu_property_re: regex::Regex,
    /// Public property declaration, excluding class/enum/void lines.
    pub(crate) property_re: fancy_regex::Regex,
    /// Private field declaration.
    pub(crate) field_re: regex::Regex,
    /// Public (possibly `new`) enum header.
    pub(crate) enum_re: regex::Regex,
    /// Public enum header with its brace-enclosed body.
    pub(crate) enum_body_re: regex::Regex,
    /// One-line `A_0.<method>(...)` direct writer call.
    pub(crate) direct_call_re: regex::Regex,
    /// One-line nested serializer call `<target>.<method>(A_0[, index])`.
    pub(crate) nested_call_re: fancy_regex::Regex,
    /// Decimal or hex C# integer literal.
    pub(crate) integer_re: regex::Regex,
    /// Bare (optionally `this.`-qualified) identifier.
    pub(crate) identifier_re: regex::Regex,
    /// `kb.Instance.<ResourceKey>` language-resource reference.
    pub(crate) resource_key_re: regex::Regex,
    /// Numeric `<n>.ToString(...)` display label.
    pub(crate) number_label_re: regex::Regex,
    /// `string.Format(kb.Instance.<Key>, ...)` display label.
    pub(crate) format_label_re: regex::Regex,
    /// Static integer constant declaration site.
    pub(crate) static_int_decl_re: regex::Regex,
    /// Constant assignment `name = <integer>;`.
    pub(crate) constant_assign_re: regex::Regex,
    /// `int <var> = <expr containing A_1>;` base-offset assignment.
    pub(crate) base_assign_re: regex::Regex,
    /// Compacted linear base expression `<base>+<stride>*A_1`.
    pub(crate) linear_base_re: regex::Regex,
    /// Compacted piecewise base expression `A_1==<i>?<b>:<base>+<stride>*A_1`.
    pub(crate) piecewise_base_re: regex::Regex,
    /// Enum member entry `Name` or `Name = <integer>`.
    pub(crate) enum_member_re: regex::Regex,
    /// `Convert.ToByte(<inner>)` helper wrapper.
    pub(crate) convert_to_byte_re: regex::Regex,
    /// `(<type>) <expr>` explicit cast.
    pub(crate) cast_re: regex::Regex,
    /// `string.Format` positional placeholder `{0}` (with format/align parts).
    pub(crate) format_placeholder_re: regex::Regex,
    /// C# line/block comment spans inside an enum body.
    pub(crate) enum_comment_re: regex::Regex,
    /// A whole-string double-quoted literal.
    pub(crate) quoted_string_re: regex::Regex,
    /// `\bA_0\.` mention counter.
    pub(crate) a0_mention_re: regex::Regex,
}

fn compile(pattern: &str) -> Result<regex::Regex> {
    regex::Regex::new(pattern)
        .map_err(|error| extract_error!("internal pattern failed to compile: {error}"))
}

fn compile_fancy(pattern: &str) -> Result<fancy_regex::Regex> {
    fancy_regex::Regex::new(pattern)
        .map_err(|error| extract_error!("internal pattern failed to compile: {error}"))
}

impl Patterns {
    /// Compile the full pattern set.
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            class_re: compile(r"(?m)^\s*(?:public|internal)\s+class\s+([@\w]+)")?,
            menu_property_re: compile(
                r"(?m)^\s*public\s+([@\w]+)\s+(RadioMenuData|GpsMenuData|AprsMenuData|DvMenuData)\s*$",
            )?,
            property_re: compile_fancy(
                r"(?m)^\s*public\s+(?!class\b|enum\b|void\b)([@\w.<>\[\], ]+)\s+([@\w]+)\s*$",
            )?,
            field_re: compile(r"(?m)^\s*private\s+([@\w.<>\[\], ]+)\s+([@\w]+)(?:\s*=.*)?;\s*$")?,
            enum_re: compile(r"(?m)^\s*public\s+(?:new\s+)?enum\s+([@\w]+)(?:\s*:\s*([@\w]+))?")?,
            enum_body_re: compile(
                r"(?ms)^\s*public\s+(?:new\s+)?enum\s+([@\w]+)(?:\s*:\s*([@\w]+))?\s*\{([^}]*)\}",
            )?,
            direct_call_re: compile(r"(?m)^\s*A_0\.([a-zA-Z0-9_@]+)\((.*)\);\s*$")?,
            nested_call_re: compile_fancy(
                r"(?m)^\s*(?!A_0\.)([^;]+?)\.([a-zA-Z0-9_@]+)\(A_0(?:\s*,\s*([^)]*))?\);\s*$",
            )?,
            integer_re: compile(r"^-?(?:0[xX][0-9a-fA-F]+|\d+)$")?,
            identifier_re: compile(r"^(?:this\.)?([@A-Za-z_][@A-Za-z0-9_]*)$")?,
            resource_key_re: compile(r"kb\.Instance\.([@\w]+)")?,
            number_label_re: compile(r"^(-?\d+(?:\.\d+)?)\.ToString(?:\(.*\))?$")?,
            format_label_re: compile(
                r"(?s)^string\.Format\(\s*kb\.Instance\.([@\w]+)\s*,\s*(.*)\)$",
            )?,
            static_int_decl_re: compile(r"(?m)^\s*public\s+static\s+int\s+([@\w]+)\s*;")?,
            constant_assign_re: compile(
                r"(?m)^\s*([@\w]+)\s*=\s*(-?(?:0[xX][0-9a-fA-F]+|\d+))\s*;",
            )?,
            base_assign_re: compile(r"\bint\s+([@\w]+)\s*=\s*([^;]*\bA_1\b[^;]*);")?,
            linear_base_re: compile(r"^(\d+)\+(\d+)\*A_1$")?,
            piecewise_base_re: compile(r"^A_1==(\d+)\?(\d+):(\d+)\+(\d+)\*A_1$")?,
            enum_member_re: compile(r"^([@\w]+)(?:\s*=\s*(-?(?:0[xX][0-9a-fA-F]+|\d+)))?$")?,
            convert_to_byte_re: compile(r"^Convert\.ToByte\(([^()]+)\)$")?,
            cast_re: compile(r"^\(([@\w.]+)\)\s*(.+)$")?,
            format_placeholder_re: compile(r"\{(\d+)(?:,[^}:]+)?(?::[^}]+)?\}")?,
            enum_comment_re: compile(r"(?ms)//.*?$|/\*.*?\*/")?,
            quoted_string_re: compile(r#"^"(.*)"$"#)?,
            a0_mention_re: compile(r"\bA_0\.")?,
        })
    }
}

/// Run a fancy-regex match fallibly, mapping engine errors to extraction errors.
pub(crate) fn fancy_captures<'t>(
    pattern: &fancy_regex::Regex,
    text: &'t str,
) -> Result<Vec<fancy_regex::Captures<'t>>> {
    pattern
        .captures_iter(text)
        .map(|capture| capture.map_err(|error| extract_error!("pattern match failed: {error}")))
        .collect()
}

/// Split a C# argument list while preserving nested calls and strings.
pub(crate) fn split_arguments(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i64;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                result.push(text.get(start..index).unwrap_or_default().trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    result.push(text.get(start..).unwrap_or_default().trim().to_owned());
    result
}

/// Parse a decimal or hex C# integer literal, else return `None`.
pub(crate) fn parse_integer(patterns: &Patterns, text: &str) -> Option<i64> {
    let text = text.trim();
    if !patterns.integer_re.is_match(text) {
        return None;
    }
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let magnitude = if let Some(hex) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok()?
    } else {
        unsigned.parse::<i64>().ok()?
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// Locate a method by signature; return its brace-balanced body and line.
pub(crate) fn find_balanced_body(source: &str, signature_pattern: &str) -> Result<(String, usize)> {
    let signature = compile(&format!("(?m){signature_pattern}"))?;
    let matched = signature
        .find(source)
        .ok_or_else(|| extract_error!("method not found: {signature_pattern}"))?;
    let after = source.get(matched.end()..).unwrap_or_default();
    let opening_offset = after
        .find('{')
        .ok_or_else(|| ExtractError::new("method body has no opening brace"))?;
    let opening = matched.end() + opening_offset;
    let mut depth = 0i64;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let tail = source.get(opening..).unwrap_or_default();
    for (offset, character) in tail.char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let body = source
                        .get(opening + 1..opening + offset)
                        .unwrap_or_default()
                        .to_owned();
                    let first_line = source
                        .get(..opening)
                        .unwrap_or_default()
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1;
                    return Ok((body, first_line));
                }
            }
            _ => {}
        }
    }
    Err(ExtractError::new("unterminated method body"))
}

/// Collapse all whitespace runs to single spaces, matching `" ".join(s.split())`.
pub(crate) fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip whitespace and parentheses, matching `re.sub(r"[\s()]", "", s)`.
pub(crate) fn compact_expression(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect()
}
