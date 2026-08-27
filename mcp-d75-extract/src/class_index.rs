//! Nesting-aware index of every class in an `ILSpy` project.
//!
//! A nested class (`private class bd` inside `oa`) is a distinct class named
//! `oa.bd`. Each class carries `own_text`: the whole file with everything
//! outside the class's own region, and every nested class region, replaced
//! by spaces (newlines kept). Line-oriented patterns and brace matching run
//! on `own_text` unchanged and still report file line numbers, while a
//! nested class's members never count as members of the enclosing class.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::csharp::Patterns;
use crate::error::{Result, extract_error};
use crate::sources::{ClassTypes, Sources, parse_types, source_label};

/// One class (top level or nested) with the text it owns.
#[derive(Debug, Clone)]
pub(crate) struct ClassInfo {
    /// Dotted name: `oa` or `oa.be`.
    pub(crate) name: String,
    /// Source path.
    pub(crate) path: PathBuf,
    /// Manifest path label.
    pub(crate) label: String,
    /// The file with everything outside this class's own region blanked.
    pub(crate) own_text: String,
    /// 1-based line of the `class` keyword.
    pub(crate) declaration_line: usize,
}

struct RawClass {
    name: String,
    declaration_start: usize,
    body_end: usize,
    declaration_line: usize,
}

fn matching_brace(source: &str, opening: usize) -> Result<usize> {
    let mut depth = 0i64;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (offset, character) in source.get(opening..).unwrap_or_default().char_indices() {
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
                    return Ok(opening + offset);
                }
            }
            _ => {}
        }
    }
    Err(extract_error!("unterminated class body"))
}

fn raw_classes(patterns: &Patterns, source: &str) -> Result<Vec<RawClass>> {
    let mut raw = Vec::new();
    for capture in patterns.class_decl_re.captures_iter(source) {
        let whole = capture
            .get(0)
            .ok_or_else(|| extract_error!("class match vanished"))?;
        let name = capture
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let after = source.get(whole.end()..).unwrap_or_default();
        let opening = whole.end()
            + after
                .find('{')
                .ok_or_else(|| extract_error!("class {name} has no opening brace"))?;
        let body_end = matching_brace(source, opening)?;
        let declaration_line = source
            .get(..whole.start())
            .unwrap_or_default()
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        raw.push(RawClass {
            name,
            declaration_start: whole.start(),
            body_end,
            declaration_line,
        });
    }
    Ok(raw)
}

fn blank_outside(source: &str, keep: (usize, usize), blank: &[(usize, usize)]) -> String {
    source
        .char_indices()
        .map(|(index, character)| {
            let outside = index < keep.0 || index > keep.1;
            let nested = blank
                .iter()
                .any(|(start, end)| index >= *start && index <= *end);
            if character == '\n' || !(outside || nested) {
                character
            } else {
                ' '
            }
        })
        .collect()
}

/// Index every class of one source file, naming nested classes `Outer.Inner`.
pub(crate) fn index_classes(
    patterns: &Patterns,
    path: &Path,
    source_dir: &Path,
    source: &str,
) -> Result<Vec<ClassInfo>> {
    let raw = raw_classes(patterns, source)?;
    let mut classes = Vec::new();
    for (position, class) in raw.iter().enumerate() {
        let mut parents: Vec<&RawClass> = raw
            .iter()
            .filter(|candidate| {
                candidate.declaration_start < class.declaration_start
                    && candidate.body_end > class.body_end
            })
            .collect();
        parents.sort_by_key(|parent| parent.declaration_start);
        let name = parents
            .iter()
            .map(|parent| parent.name.as_str())
            .chain(std::iter::once(class.name.as_str()))
            .collect::<Vec<_>>()
            .join(".");
        let nested: Vec<(usize, usize)> = raw
            .iter()
            .enumerate()
            .filter(|(index, candidate)| {
                *index != position
                    && candidate.declaration_start > class.declaration_start
                    && candidate.body_end < class.body_end
            })
            .map(|(_, candidate)| (candidate.declaration_start, candidate.body_end))
            .collect();
        classes.push(ClassInfo {
            name,
            path: path.to_path_buf(),
            label: source_label(path, source_dir),
            own_text: blank_outside(source, (class.declaration_start, class.body_end), &nested),
            declaration_line: class.declaration_line,
        });
    }
    Ok(classes)
}

/// Every class of the project by dotted name.
#[derive(Debug)]
pub(crate) struct ClassIndex {
    classes: Vec<ClassInfo>,
    by_name: HashMap<String, usize>,
}

impl ClassIndex {
    /// Index every class in every source; duplicate names are an error.
    pub(crate) fn build(patterns: &Patterns, sources: &Sources, source_dir: &Path) -> Result<Self> {
        let mut classes: Vec<ClassInfo> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for (path, source) in sources {
            for class in index_classes(patterns, path, source_dir, source)? {
                if let Some(existing) = by_name.get(&class.name) {
                    let previous = classes
                        .get(*existing)
                        .ok_or_else(|| extract_error!("class index corrupted"))?;
                    return Err(extract_error!(
                        "duplicate source class {}: {} line {} and {} line {}",
                        class.name,
                        previous.path.display(),
                        previous.declaration_line,
                        class.path.display(),
                        class.declaration_line
                    ));
                }
                let _previous = by_name.insert(class.name.clone(), classes.len());
                classes.push(class);
            }
        }
        Ok(Self { classes, by_name })
    }

    /// Class by dotted name.
    pub(crate) fn get(&self, name: &str) -> Option<&ClassInfo> {
        self.by_name
            .get(name)
            .and_then(|index| self.classes.get(*index))
    }

    /// Every class in file order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &ClassInfo> {
        self.classes.iter()
    }

    /// Resolve a type name as written inside `owner`: a class nested in
    /// `owner` (or in `owner`'s ancestors), then a top-level or dotted class.
    pub(crate) fn resolve(&self, owner: &ClassInfo, type_name: &str) -> Option<&ClassInfo> {
        let mut scope: Vec<&str> = owner.name.split('.').collect();
        while !scope.is_empty() {
            let candidate = format!("{}.{type_name}", scope.join("."));
            if let Some(class) = self.get(&candidate) {
                return Some(class);
            }
            let _popped = scope.pop();
        }
        self.get(type_name)
    }

    /// Resolve an enum type as written inside `owner` to its qualified
    /// `<declaring class>.<enum>` name.
    ///
    /// A qualified type (`oa.ad`) names its declaring class, resolved like
    /// any class name; an unqualified type is looked up in `owner` and then
    /// in each enclosing class.
    pub(crate) fn resolve_enum(
        &self,
        patterns: &Patterns,
        owner: &ClassInfo,
        type_name: &str,
    ) -> Option<String> {
        let declares = |class: &ClassInfo, enum_name: &str| {
            patterns
                .enum_re
                .captures_iter(&class.own_text)
                .any(|capture| {
                    capture
                        .get(1)
                        .is_some_and(|name| name.as_str() == enum_name)
                })
        };
        if let Some((class_part, enum_name)) = type_name.rsplit_once('.') {
            return self
                .resolve(owner, class_part)
                .filter(|class| declares(class, enum_name))
                .map(|class| format!("{}.{enum_name}", class.name));
        }
        let mut scope: Vec<&str> = owner.name.split('.').collect();
        while !scope.is_empty() {
            let candidate = scope.join(".");
            if let Some(class) = self.get(&candidate)
                && declares(class, type_name)
            {
                return Some(format!("{candidate}.{type_name}"));
            }
            let _popped = scope.pop();
        }
        None
    }

    /// The member types of `class`, with every enum-typed member resolved
    /// to its declaring class (nested enums and enums declared in other
    /// classes alike).
    pub(crate) fn types(&self, patterns: &Patterns, class: &ClassInfo) -> Result<ClassTypes> {
        let mut types = parse_types(patterns, &class.name, &class.own_text)?;
        let candidates: Vec<String> = types
            .properties
            .values()
            .chain(types.private_fields.values())
            .filter(|kind| !types.enums.contains_key(kind.as_str()))
            .filter(|kind| !kind.contains(['<', '[']))
            .cloned()
            .collect();
        for kind in candidates {
            if let Some(qualified) = self.resolve_enum(patterns, class, &kind) {
                drop(types.enums.insert(kind, qualified));
            }
        }
        Ok(types)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    const NESTED: &str = "public class oa : n5\n{\n\tprivate class bd\n\t{\n\t\tprivate int m_a;\n\n\t\tpublic void b(n7 A_0, int A_1)\n\t\t{\n\t\t\tint num3 = 848 + 16 * A_1;\n\t\t\tA_0.b(this.m_a, 2, num3);\n\t\t}\n\t}\n\n\tprivate bd m_a = new bd();\n\n\tpublic byte Language\n\t{\n\t\tget { return 0; }\n\t}\n\n\tpublic void a6(n7 A_0)\n\t{\n\t\tA_0.a((byte)Language, 4242);\n\t\tthis.m_a.b(A_0, 0);\n\t}\n}\n";

    fn index() -> std::result::Result<ClassIndex, Box<dyn std::error::Error>> {
        let patterns = Patterns::new()?;
        let sources: Sources = vec![(PathBuf::from("oa.cs"), NESTED.to_owned())];
        Ok(ClassIndex::build(&patterns, &sources, Path::new(""))?)
    }

    #[test]
    fn names_nested_classes_with_their_parent() -> TestResult {
        let index = index()?;
        let names: Vec<&str> = index.iter().map(|class| class.name.as_str()).collect();
        assert_eq!(names, vec!["oa", "oa.bd"]);
        Ok(())
    }

    #[test]
    fn own_text_excludes_nested_members_and_keeps_line_numbers() -> TestResult {
        let index = index()?;
        let outer = index.get("oa").ok_or("oa missing")?;
        assert!(outer.own_text.contains("public void a6(n7 A_0)"));
        assert!(
            !outer.own_text.contains("int num3 = 848"),
            "nested body must be blanked"
        );
        assert_eq!(outer.own_text.lines().count(), NESTED.lines().count());
        let inner = index.get("oa.bd").ok_or("oa.bd missing")?;
        assert!(inner.own_text.contains("int num3 = 848"));
        assert!(!inner.own_text.contains("A_0.a((byte)Language"));
        assert_eq!(inner.declaration_line, 3);
        Ok(())
    }

    #[test]
    fn resolves_types_relative_to_the_owner() -> TestResult {
        let index = index()?;
        let outer = index.get("oa").ok_or("oa missing")?;
        assert_eq!(
            index.resolve(outer, "bd").map(|class| class.name.as_str()),
            Some("oa.bd")
        );
        assert_eq!(
            index.resolve(outer, "oa").map(|class| class.name.as_str()),
            Some("oa")
        );
        assert!(index.resolve(outer, "zz").is_none());
        Ok(())
    }
}
