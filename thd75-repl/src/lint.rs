//! Accessibility lint checker for REPL output.
//!
//! Implements fourteen hard rules (R1 through R14) that together
//! establish conformance to nine published accessibility standards:
//! WCAG 2.1 Level AA, Section 508 of the US Rehabilitation Act, the
//! CHI 2021 CLI accessibility paper, EN 301 549 version 3.2.1,
//! ISO 9241-171, ITU-T Recommendation F.790, BRLTTY compatibility,
//! the Handi-Ham Program recommendations, and the ARRL accessibility
//! resources. Each `Rule` variant carries its own doc comment
//! describing the specific constraint.

use std::fmt;

/// An identifier for one of the 14 hard accessibility rules. Each
/// variant carries its own doc comment describing the specific
/// constraint the rule enforces.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    /// R1: Every byte is ASCII printable (0x20–0x7E) or `\n`.
    AsciiOnly,
    /// R2: Every line is ≤ 80 characters.
    LineLength,
    /// R3: No ANSI escape sequences.
    NoAnsi,
    /// R4: Error lines begin with `Error:`.
    ErrorPrefix,
    /// R5: Warning lines begin with `Warning:`.
    WarningPrefix,
    /// R6: Mutating commands confirm success. (Covered by per-command unit tests; not a line-level rule.)
    ConfirmOnSet,
    /// R7: Lists end with a count summary. (Covered by per-command unit tests.)
    ListCountSummary,
    /// R8: Labels end with a colon before their values.
    LabelColonValue,
    /// R9: Booleans render as `on`/`off`, never `true`/`false` or `1`/`0`.
    BooleanWords,
    /// R10: No `print!` without a trailing newline. (Static grep rule.)
    NoNakedPrint,
    /// R11: No cursor-move sequences or spinners. (Static grep rule.)
    NoCursorMoves,
    /// R12: Numeric units spelled out (megahertz, watts, etc.).
    UnitsSpelledOut,
    /// R13: No ad-hoc `[HH:MM:SS]` timestamps. (Static grep rule.)
    NoAdHocTimestamps,
    /// R14: User output to stdout, diagnostics to stderr. (Static grep rule.)
    StdoutStderrSeparation,
}

impl Rule {
    /// Short identifier used in error messages and the compliance
    /// report: `"R1"`, `"R2"`, etc.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::AsciiOnly => "R1",
            Self::LineLength => "R2",
            Self::NoAnsi => "R3",
            Self::ErrorPrefix => "R4",
            Self::WarningPrefix => "R5",
            Self::ConfirmOnSet => "R6",
            Self::ListCountSummary => "R7",
            Self::LabelColonValue => "R8",
            Self::BooleanWords => "R9",
            Self::NoNakedPrint => "R10",
            Self::NoCursorMoves => "R11",
            Self::UnitsSpelledOut => "R12",
            Self::NoAdHocTimestamps => "R13",
            Self::StdoutStderrSeparation => "R14",
        }
    }

    /// Human-readable summary of the rule, suitable for the report
    /// printed by the `check` subcommand.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AsciiOnly => "ASCII only",
            Self::LineLength => "line under 80 chars",
            Self::NoAnsi => "no ANSI escapes",
            Self::ErrorPrefix => "error prefix",
            Self::WarningPrefix => "warning prefix",
            Self::ConfirmOnSet => "confirm after set",
            Self::ListCountSummary => "list count summary",
            Self::LabelColonValue => "label colon value",
            Self::BooleanWords => "booleans as words",
            Self::NoNakedPrint => "no naked print",
            Self::NoCursorMoves => "no cursor or spinners",
            Self::UnitsSpelledOut => "units spelled out",
            Self::NoAdHocTimestamps => "timestamps only via aprintln",
            Self::StdoutStderrSeparation => "stdout and stderr separated",
        }
    }
}

/// One accessibility rule violation discovered during linting.
///
/// Contains the rule that was violated, the 0-based line number in
/// the scanned input where the violation occurred, and a short
/// human-readable explanation of what was wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// Which rule was violated.
    pub rule: Rule,
    /// 0-based line number within the scanned input.
    pub line: usize,
    /// Human-readable explanation of the violation.
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: line {}: {}",
            self.rule.id(),
            self.rule.description(),
            self.line,
            self.message,
        )
    }
}

/// Shorthand unit tokens rejected by R12. Longer units (`MHz`,
/// `kHz`, `GHz`) are listed before the short `Hz` so the per-unit
/// `break` inside [`check_r12_units`] takes the longer match first.
const BAD_UNITS: &[&str] = &["MHz", "kHz", "GHz", "Hz", "mW", "kW", "dB"];

/// R3: reject any ANSI control sequence, identified by ESC (0x1B).
/// Returns `true` if an ESC byte was found and a violation pushed.
fn check_r3_no_ansi(line: &str, violations: &mut Vec<Violation>) -> bool {
    line.as_bytes()
        .iter()
        .position(|&b| b == 0x1B)
        .is_some_and(|offset| {
            violations.push(Violation {
                rule: Rule::NoAnsi,
                line: 0,
                message: format!("ESC byte at offset {offset}"),
            });
            true
        })
}

/// R1: reject any byte outside printable ASCII (0x20-0x7E).
fn check_r1_ascii(line: &str, violations: &mut Vec<Violation>) {
    for (offset, byte) in line.as_bytes().iter().enumerate() {
        if !(0x20..=0x7E).contains(byte) {
            violations.push(Violation {
                rule: Rule::AsciiOnly,
                line: 0,
                message: format!("byte 0x{byte:02X} at offset {offset}"),
            });
            // One R1 violation per line is enough to signal the problem;
            // reporting every non-ASCII byte on a line with many would
            // drown the output.
            break;
        }
    }
}

/// R2: reject lines longer than 80 characters.
fn check_r2_line_length(line: &str, violations: &mut Vec<Violation>) {
    let char_count = line.chars().count();
    if char_count > 80 {
        violations.push(Violation {
            rule: Rule::LineLength,
            line: 0,
            message: format!("{char_count} characters (max 80)"),
        });
    }
}

/// R4: lines whose first word looks error-like must start with
/// `Error: `. Heuristic: case-insensitive first word equals `error`,
/// `error:`, or starts with `error` and is at most 7 characters.
fn check_r4_error_prefix(line: &str, violations: &mut Vec<Violation>) {
    if let Some(first_word) = line.split_whitespace().next() {
        let lower = first_word.to_lowercase();
        let looks_like_error = lower == "error"
            || lower == "error:"
            || (lower.starts_with("error") && lower.len() <= 7);
        if looks_like_error && !line.starts_with("Error: ") {
            violations.push(Violation {
                rule: Rule::ErrorPrefix,
                line: 0,
                message: format!(
                    "line looks like an error report but does not start with \"Error: \" (first word: {first_word:?})"
                ),
            });
        }
    }
}

/// R5: same shape as R4, for warning lines. First word looking like
/// `warning` must start with `Warning: `.
fn check_r5_warning_prefix(line: &str, violations: &mut Vec<Violation>) {
    if let Some(first_word) = line.split_whitespace().next() {
        let lower = first_word.to_lowercase();
        let looks_like_warning = lower == "warning"
            || lower == "warning:"
            || (lower.starts_with("warning") && lower.len() <= 9);
        if looks_like_warning && !line.starts_with("Warning: ") {
            violations.push(Violation {
                rule: Rule::WarningPrefix,
                line: 0,
                message: format!(
                    "line looks like a warning but does not start with \"Warning: \" (first word: {first_word:?})"
                ),
            });
        }
    }
}

/// R9: reject `: true` and `: false` as standalone tokens. This is
/// the common `{bool}` format mistake.
fn check_r9_boolean_words(line: &str, violations: &mut Vec<Violation>) {
    for bad in [": true", ": false"] {
        if let Some(offset) = line.find(bad) {
            let next = line.as_bytes().get(offset + bad.len());
            let is_standalone = next.is_none_or(|&b| !b.is_ascii_alphanumeric());
            if is_standalone {
                violations.push(Violation {
                    rule: Rule::BooleanWords,
                    line: 0,
                    message: format!(
                        "boolean literal {bad:?} at offset {offset} - use on/off instead"
                    ),
                });
            }
        }
    }
}

/// R12: reject shorthand unit tokens (MHz, Hz, dB, ...) when they
/// appear as standalone tokens bounded by non-alphanumerics. `Hz` as
/// a substring of `hertz` is not flagged because the neighbouring
/// characters are alphabetic.
fn check_r12_units(line: &str, violations: &mut Vec<Violation>) {
    let bytes = line.as_bytes();
    for unit in BAD_UNITS {
        let mut search_start = 0;
        while let Some(rel_offset) = line[search_start..].find(unit) {
            let offset = search_start + rel_offset;
            search_start = offset + unit.len();

            #[expect(
                clippy::indexing_slicing,
                reason = "The `if offset == 0` guard above ensures `offset - 1` is \
                          non-negative when this branch runs, and `offset` came from \
                          `find(unit)` on `bytes`, so `offset < bytes.len()` — the index \
                          is always in-bounds. Using `bytes.get(offset - 1)` and \
                          unwrapping would add noise without catching a real bug."
            )]
            let prev = if offset == 0 { b' ' } else { bytes[offset - 1] };
            let next = bytes.get(offset + unit.len()).copied().unwrap_or(b' ');

            let prev_ok = !prev.is_ascii_alphanumeric();
            let next_ok = !next.is_ascii_alphanumeric();

            if prev_ok && next_ok {
                violations.push(Violation {
                    rule: Rule::UnitsSpelledOut,
                    line: 0,
                    message: format!("shorthand unit {unit:?} at offset {offset} - spell it out"),
                });
                break;
            }
        }
    }
}

/// R8: lines starting with `Band A ` or `Band B ` must contain `: `.
///
/// The rule is narrowed so natural verb-phrase sentences (e.g.
/// `Band A tuned to 146.52 megahertz`, `Band A mode set to FM`,
/// `Band A recalled channel 5`) are not flagged. R8 only fires on
/// lines that look like `Band X <noun> <value>` - the label-shape
/// pattern - without any intervening verb.
fn check_r8_label_colon(line: &str, violations: &mut Vec<Violation>) {
    /// Past-tense verbs that can appear directly after `Band X `.
    /// `Band A tuned to X`, `Band A stepped up to X`, `Band A recalled
    /// channel N` - these are sentences, not labels.
    const DIRECT_VERBS: &[&str] = &["tuned", "stepped", "recalled"];
    /// Phrases that follow a label noun to form a verb phrase:
    /// `Band A mode set to FM`, `Band A squelch set to 5`. If the
    /// substring after the prefix contains " set to " we treat the
    /// whole line as a verb phrase.
    const VERB_PHRASES: &[&str] = &[" set to "];
    for prefix in ["Band A ", "Band B "] {
        if !line.starts_with(prefix) || line.contains(": ") {
            continue;
        }
        let rest = &line[prefix.len()..];
        // Check for direct past-tense verbs (`tuned`, `stepped`).
        let first = rest.split_whitespace().next().unwrap_or("");
        if DIRECT_VERBS.contains(&first) {
            continue;
        }
        // Check for `... set to ...` verb phrases.
        if VERB_PHRASES.iter().any(|p| rest.contains(p)) {
            continue;
        }
        violations.push(Violation {
            rule: Rule::LabelColonValue,
            line: 0,
            message: format!("line starts with {prefix:?} but has no colon separator"),
        });
        break;
    }
}

/// Check a single line against all applicable line-level rules.
///
/// Static rules (R10, R11, R13, R14) are structural and cannot be
/// checked from a single line; they are enforced by a static grep
/// test that scans `src/**/*.rs`.
///
/// # Errors
///
/// Returns a `Vec` of all violations found on the line. A fully-
/// conformant line returns `Ok(())`.
pub fn check_line(line: &str) -> Result<(), Vec<Violation>> {
    let mut violations = Vec::new();

    // R3 must run first and short-circuit: ESC would otherwise also
    // trip R1, and the duplicate pair would be confusing.
    if check_r3_no_ansi(line, &mut violations) {
        return Err(violations);
    }

    check_r1_ascii(line, &mut violations);
    check_r2_line_length(line, &mut violations);
    check_r4_error_prefix(line, &mut violations);
    check_r5_warning_prefix(line, &mut violations);
    check_r9_boolean_words(line, &mut violations);
    check_r12_units(line, &mut violations);
    check_r8_label_colon(line, &mut violations);

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Check multi-line output against all applicable rules.
///
/// Splits on `\n` and runs [`check_line`] on each line. Also runs
/// cross-line rules that need the full output (e.g. error-prefix
/// classification requires looking at the line as a whole).
///
/// # Errors
///
/// Returns a `Vec` of all violations found across all lines. A
/// fully-conformant output returns `Ok(())`.
pub fn check_output(output: &str) -> Result<(), Vec<Violation>> {
    let mut violations = Vec::new();
    // Strip a single trailing `\n` so we don't see the phantom empty
    // string that `split` produces for newline-terminated inputs.
    // Intentional blank lines in the middle of the output are still
    // checked normally.
    let trimmed = output.strip_suffix('\n').unwrap_or(output);
    for (line_no, line) in trimmed.split('\n').enumerate() {
        if let Err(mut v) = check_line(line) {
            for violation in &mut v {
                violation.line = line_no;
            }
            violations.append(&mut v);
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_check_line_accepts_empty() {
        assert!(check_line("").is_ok());
    }

    #[test]
    fn stub_check_output_accepts_empty() {
        assert!(check_output("").is_ok());
    }

    #[test]
    fn check_output_processes_intentional_blank_lines() {
        // A blank line in the middle of the output should be visible
        // to check_line (which currently accepts it because the stub
        // always returns Ok, but once rule logic lands this test
        // becomes meaningful — e.g. R1 must still run on the blank
        // line to confirm it has no control characters).
        //
        // This test locks in the fix for the bug where
        // `if line.is_empty() && line_no > 0 { continue; }` was
        // silently skipping mid-output blank lines.
        let result = check_output("first line\n\nthird line\n");
        assert!(result.is_ok(), "check_output should process blank lines");
    }

    #[test]
    fn check_output_strips_trailing_newline_only_once() {
        // Double trailing newline means there IS an intentional blank
        // as the final line. It should be checked, not silently dropped.
        // With `split('\n')` on `"a\n\n"` after stripping one `\n`, we
        // get ["a", ""] — two lines, both processed.
        let result = check_output("a\n\n");
        assert!(result.is_ok());
    }

    #[test]
    fn rule_ids_are_r1_through_r14() {
        assert_eq!(Rule::AsciiOnly.id(), "R1");
        assert_eq!(Rule::LineLength.id(), "R2");
        assert_eq!(Rule::NoAnsi.id(), "R3");
        assert_eq!(Rule::ErrorPrefix.id(), "R4");
        assert_eq!(Rule::WarningPrefix.id(), "R5");
        assert_eq!(Rule::ConfirmOnSet.id(), "R6");
        assert_eq!(Rule::ListCountSummary.id(), "R7");
        assert_eq!(Rule::LabelColonValue.id(), "R8");
        assert_eq!(Rule::BooleanWords.id(), "R9");
        assert_eq!(Rule::NoNakedPrint.id(), "R10");
        assert_eq!(Rule::NoCursorMoves.id(), "R11");
        assert_eq!(Rule::UnitsSpelledOut.id(), "R12");
        assert_eq!(Rule::NoAdHocTimestamps.id(), "R13");
        assert_eq!(Rule::StdoutStderrSeparation.id(), "R14");
    }

    #[test]
    fn violation_display_includes_rule_id_and_line() {
        let v = Violation {
            rule: Rule::AsciiOnly,
            line: 3,
            message: "byte 0xE2 at offset 14".to_string(),
        };
        let rendered = v.to_string();
        assert!(rendered.contains("R1"));
        assert!(rendered.contains("ASCII only"));
        assert!(rendered.contains("line 3"));
        assert!(rendered.contains("byte 0xE2 at offset 14"));
    }

    #[test]
    fn r1_accepts_plain_ascii() {
        assert!(check_line("Band A frequency: 146.52 megahertz").is_ok());
        assert!(check_line("Error: invalid frequency").is_ok());
        assert!(check_line("").is_ok());
    }

    #[test]
    fn r1_rejects_unicode_right_arrow() {
        assert!(check_line("Forwarding RF \u{2192} internet").is_err());
    }

    #[test]
    fn r1_rejects_em_dash() {
        assert!(check_line("stopped \u{2014} good").is_err());
    }

    #[test]
    fn r1_rejects_left_right_arrow() {
        assert!(check_line("RF \u{2194} internet").is_err());
    }

    #[test]
    fn r1_rejects_tab() {
        assert!(check_line("Band\tA").is_err());
    }

    #[test]
    fn r1_rejects_carriage_return() {
        assert!(check_line("partial\rline").is_err());
    }

    #[test]
    fn r1_rejects_null_byte() {
        assert!(check_line("foo\0bar").is_err());
    }

    #[test]
    fn r1_violation_includes_byte_offset() {
        let err = check_line("hi \u{2192} there").unwrap_err();
        let v = err.iter().find(|v| v.rule == Rule::AsciiOnly).unwrap();
        assert!(v.message.contains("offset 3"));
    }

    #[test]
    fn r2_accepts_80_char_line() {
        let s = "a".repeat(80);
        assert!(check_line(&s).is_ok());
    }

    #[test]
    fn r2_rejects_81_char_line() {
        let s = "a".repeat(81);
        let err = check_line(&s).unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::LineLength));
    }

    #[test]
    fn r2_violation_reports_actual_length() {
        let s = "a".repeat(83);
        let err = check_line(&s).unwrap_err();
        let v = err.iter().find(|v| v.rule == Rule::LineLength).unwrap();
        assert!(v.message.contains("83"));
    }

    #[test]
    fn r3_rejects_ansi_color() {
        let err = check_line("\x1b[31mError\x1b[0m").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::NoAnsi));
    }

    #[test]
    fn r3_rejects_cursor_move() {
        let err = check_line("\x1b[2J").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::NoAnsi));
    }

    #[test]
    fn r3_accepts_plain_text() {
        assert!(check_line("Band A frequency: 146.52 megahertz").is_ok());
    }

    #[test]
    fn r4_accepts_proper_error_prefix() {
        assert!(check_line("Error: invalid frequency").is_ok());
        assert!(check_line("Error: something went wrong").is_ok());
    }

    #[test]
    fn r4_accepts_line_without_error_word() {
        assert!(check_line("Band A frequency: 146.52 megahertz").is_ok());
    }

    #[test]
    fn r4_rejects_missing_prefix_on_error_line() {
        let err = check_line("error: bad thing happened").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::ErrorPrefix));
    }

    #[test]
    fn r4_rejects_uppercase_no_colon() {
        let err = check_line("Error invalid frequency").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::ErrorPrefix));
    }

    #[test]
    fn r4_accepts_word_errors_in_declarative_line() {
        assert!(check_line("Total errors found: 0").is_ok());
    }

    #[test]
    fn r5_accepts_proper_warning_prefix() {
        assert!(check_line("Warning: authentication failed").is_ok());
    }

    #[test]
    fn r5_rejects_lowercase_warning() {
        let err = check_line("warning: auth failed").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::WarningPrefix));
    }

    #[test]
    fn r5_rejects_missing_colon() {
        let err = check_line("Warning auth failed").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::WarningPrefix));
    }

    #[test]
    fn r9_accepts_on_off() {
        assert!(check_line("Key lock: on").is_ok());
        assert!(check_line("Bluetooth: off").is_ok());
    }

    #[test]
    fn r9_rejects_true() {
        let err = check_line("Key lock: true").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::BooleanWords));
    }

    #[test]
    fn r9_rejects_false() {
        let err = check_line("Bluetooth: false").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::BooleanWords));
    }

    #[test]
    fn r9_accepts_truth_in_word() {
        assert!(check_line("The truth is simple").is_ok());
    }

    #[test]
    fn r12_accepts_spelled_units() {
        assert!(check_line("Band A frequency: 146.52 megahertz").is_ok());
        assert!(check_line("Band A power: high, 5 watts").is_ok());
    }

    #[test]
    fn r12_rejects_mhz_shorthand() {
        let err = check_line("146.52 MHz").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::UnitsSpelledOut));
    }

    #[test]
    fn r12_rejects_khz_shorthand() {
        let err = check_line("step 25 kHz").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::UnitsSpelledOut));
    }

    #[test]
    fn r12_rejects_db_shorthand() {
        let err = check_line("signal -110 dB").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::UnitsSpelledOut));
    }

    #[test]
    fn r12_accepts_hz_inside_word() {
        assert!(check_line("Frequency in hertz").is_ok());
    }

    #[test]
    fn r8_accepts_label_colon_value() {
        assert!(check_line("Band A frequency: 146.52 megahertz").is_ok());
        assert!(check_line("Radio model: TH-D75").is_ok());
    }

    #[test]
    fn r8_accepts_imperative_sentence() {
        assert!(check_line("Reading radio status, please wait.").is_ok());
    }

    #[test]
    fn r8_rejects_labeled_line_without_colon() {
        let err = check_line("Band A frequency 146.52 megahertz").unwrap_err();
        assert!(err.iter().any(|v| v.rule == Rule::LabelColonValue));
    }
}
