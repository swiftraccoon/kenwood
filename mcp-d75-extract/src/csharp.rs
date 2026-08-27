//! Narrow C# text primitives for `ILSpy`'s one-line serializer call shape.

use crate::error::{ExtractError, Result, extract_error};

/// Compiled patterns shared by the extraction stages.
///
/// Two patterns (`property_re`, `nested_call_line_re`) require negative
/// lookahead and therefore use the backtracking `fancy_regex` engine; the
/// rest use the linear-time `regex` engine.
#[derive(Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is a compiled pattern; the _re suffix names the pattern's role"
)]
pub(crate) struct Patterns {
    /// `^\s*(?:public|internal)\s+class\s+([@\w]+)` (multiline).
    pub(crate) class_re: regex::Regex,
    /// Class declaration with any modifier list (top level or nested).
    pub(crate) class_decl_re: regex::Regex,
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
    /// Decimal or hex C# integer literal.
    pub(crate) integer_re: regex::Regex,
    /// Bare (optionally `this.`-qualified) identifier.
    pub(crate) identifier_re: regex::Regex,
    /// Numeric `<n>.ToString(...)` display label.
    pub(crate) number_label_re: regex::Regex,
    /// Static integer constant declaration site.
    pub(crate) static_int_decl_re: regex::Regex,
    /// Constant assignment `name = <integer>;`.
    pub(crate) constant_assign_re: regex::Regex,
    /// `int <var> = <expr containing A_1>;` base-offset assignment.
    pub(crate) base_assign_re: regex::Regex,
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
    /// Public single-parameter void method (writers and readers).
    pub(crate) single_param_method_re: regex::Regex,
    /// Statement-form direct write `A_0.x(...);` (one per line).
    pub(crate) direct_statement_re: regex::Regex,
    /// Nested serializer call on one line: `[this.]ident[[idx]].method(A_0[, idx]);`.
    pub(crate) nested_call_line_re: fancy_regex::Regex,
    /// Private field declaration with optional `= new T()` initializer.
    pub(crate) private_field_decl_re: regex::Regex,
    /// `DisplayMember = X.Instance.` resource singleton reference.
    pub(crate) display_member_instance_re: regex::Regex,
    /// `new byte[<n>]` allocation.
    pub(crate) byte_array_alloc_re: regex::Regex,
    /// Public list property header: `public List<T> Name`.
    pub(crate) list_property_re: regex::Regex,
    /// `return <field>;`.
    pub(crate) return_field_re: regex::Regex,
    /// `<sym> = value;` inside a setter.
    pub(crate) setter_assign_re: regex::Regex,
    /// `[assembly: AssemblyProduct("...")]`.
    pub(crate) assembly_product_re: regex::Regex,
    /// `[assembly: AssemblyVersion("...")]`.
    pub(crate) assembly_version_re: regex::Regex,
    /// `helper(identifier)` where helper is a private method of the writing class.
    pub(crate) helper_call_re: regex::Regex,
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
            class_decl_re: compile(
                r"(?m)^\s*(?:(?:public|internal|private|protected|static|sealed|abstract|partial)\s+)*class\s+([@\w]+)",
            )?,
            property_re: compile_fancy(
                r"(?m)^[ \t]*public\s+(?!class\b|enum\b|void\b)([@\w.<>\[\], ]+)\s+([@\w]+)[ \t]*$",
            )?,
            field_re: compile(r"(?m)^\s*private\s+([@\w.<>\[\], ]+)\s+([@\w]+)(?:\s*=.*)?;\s*$")?,
            enum_re: compile(r"(?m)^\s*public\s+(?:new\s+)?enum\s+([@\w]+)(?:\s*:\s*([@\w]+))?")?,
            enum_body_re: compile(
                r"(?ms)^\s*public\s+(?:new\s+)?enum\s+([@\w]+)(?:\s*:\s*([@\w]+))?\s*\{([^}]*)\}",
            )?,
            direct_call_re: compile(r"(?m)^\s*A_0\.([a-zA-Z0-9_@]+)\((.*)\);\s*$")?,
            integer_re: compile(r"^-?(?:0[xX][0-9a-fA-F]+|\d+)$")?,
            identifier_re: compile(r"^(?:this\.)?([@A-Za-z_][@A-Za-z0-9_]*)$")?,
            number_label_re: compile(r"^(-?\d+(?:\.\d+)?)\.ToString(?:\(.*\))?$")?,
            static_int_decl_re: compile(r"(?m)^\s*public\s+static\s+int\s+([@\w]+)\s*;")?,
            constant_assign_re: compile(
                r"(?m)^\s*([@\w]+)\s*=\s*(-?(?:0[xX][0-9a-fA-F]+|\d+))\s*;",
            )?,
            base_assign_re: compile(r"\bint\s+([@\w]+)\s*=\s*([^;]*\bA_1\b[^;]*);")?,
            enum_member_re: compile(r"^([@\w]+)(?:\s*=\s*(-?(?:0[xX][0-9a-fA-F]+|\d+)))?$")?,
            convert_to_byte_re: compile(r"^Convert\.ToByte\(([^()]+)\)$")?,
            cast_re: compile(r"^\(([@\w.]+)\)\s*(.+)$")?,
            format_placeholder_re: compile(r"\{(\d+)(?:,[^}:]+)?(?::[^}]+)?\}")?,
            enum_comment_re: compile(r"(?ms)//.*?$|/\*.*?\*/")?,
            quoted_string_re: compile(r#"^"(.*)"$"#)?,
            a0_mention_re: compile(r"\bA_0\.")?,
            single_param_method_re: compile(
                r"(?m)^\s*public\s+(?:override\s+)?void\s+([@\w]+)\s*\(\s*([@\w.]+)\s+A_0\s*\)",
            )?,
            direct_statement_re: compile(r"(?m)^\s*A_0\.[a-zA-Z0-9_@]+\(.*\);\s*$")?,
            nested_call_line_re: compile_fancy(
                r"(?m)^[ \t]*(?!A_0\.)((?:this\.)?[@\w]+(?:\[[@\w]+\])?)\.([a-zA-Z0-9_@]+)\(A_0(?:[ \t]*,[ \t]*([^)]*))?\);[ \t]*$",
            )?,
            private_field_decl_re: compile(
                r"(?m)^\s*private\s+([@\w.<>\[\], ]+?)\s+([@\w]+)\s*(?:=\s*new\s+[@\w.<>]+\(\)\s*)?;\s*$",
            )?,
            display_member_instance_re: compile(r"DisplayMember\s*=\s*([@\w]+)\.Instance\.")?,
            byte_array_alloc_re: compile(r"new\s+byte\[(\d+)\]")?,
            list_property_re: compile(r"(?m)^\s*public\s+List<([@\w.]+)>\s+([@\w]+)\s*$")?,
            return_field_re: compile(r"(?m)^\s*return\s+(?:this\.)?([@\w]+)\s*;\s*$")?,
            setter_assign_re: compile(r"(?m)^\s*(?:this\.)?([@\w]+)\s*=\s*value\s*;\s*$")?,
            assembly_product_re: compile(r#"\[assembly:\s*AssemblyProduct\("([^"]*)"\)\]"#)?,
            assembly_version_re: compile(r#"\[assembly:\s*AssemblyVersion\("([^"]*)"\)\]"#)?,
            helper_call_re: compile(r"^([@A-Za-z_][@\w]*)\(((?:this\.)?[@A-Za-z_][@\w]*)\)$")?,
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

/// Collapse all whitespace runs to single spaces.
pub(crate) fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip whitespace and parentheses from an expression.
pub(crate) fn compact_expression(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect()
}
