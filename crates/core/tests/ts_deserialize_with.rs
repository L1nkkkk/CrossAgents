use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

// Look inside both direct attributes and cfg_attr without confusing adjacent fields.
fn has_attribute(tokens: proc_macro2::TokenStream, name: &str, members: &[&str]) -> bool {
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            proc_macro2::TokenTree::Ident(ident) if ident == name => {
                if let Some(proc_macro2::TokenTree::Group(group)) = tokens.peek()
                    && group.stream().into_iter().any(|token| {
                        matches!(token, proc_macro2::TokenTree::Ident(ident) if members.iter().any(|member| ident == *member))
                    })
                {
                    return true;
                }
            }
            proc_macro2::TokenTree::Group(group) if has_attribute(group.stream(), name, members) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

#[derive(Default)]
struct Check {
    missing: Vec<String>,
}

impl<'ast> Visit<'ast> for Check {
    fn visit_field(&mut self, field: &'ast syn::Field) {
        let has = |name, members: &[&str]| {
            field
                .attrs
                .iter()
                .any(|attr| has_attribute(quote::quote!(#attr), name, members))
        };
        if has("serde", &["deserialize_with"]) && !has("ts", &["type", "skip"]) {
            self.missing.push(
                field
                    .ident
                    .as_ref()
                    .map_or_else(|| "tuple field".to_owned(), ToString::to_string),
            );
        }
        visit::visit_field(self, field);
    }
}

fn scan(path: &Path, failures: &mut Vec<String>) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, failures);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = std::fs::read_to_string(&path).unwrap();
            let syntax =
                syn::parse_file(&source).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let mut check = Check::default();
            check.visit_file(&syntax);
            failures.extend(
                check
                    .missing
                    .iter()
                    .map(|field| format!("{}: {field}", path.display())),
            );
        }
    }
}

#[test]
fn deserialize_with_fields_declare_ts_type_or_skip() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    scan(&root.join("src"), &mut failures);
    scan(&root.join("../api-types/src"), &mut failures);
    assert!(
        failures.is_empty(),
        "Fields with serde deserialize_with need #[ts(type = \"...\")] or #[ts(skip)] (use cfg_attr(feature = \"ts\", ...) in core):\n{}",
        failures.join("\n")
    );
}

#[test]
fn detects_missing_annotations_on_each_field() {
    let syntax = syn::parse_file(
        r#"
        struct Example {
            #[serde(deserialize_with = "read")]
            missing: String,
            #[serde(deserialize_with = "read")]
            #[ts(optional, type = "string")]
            typed: String,
            #[serde(deserialize_with = "read")]
            #[cfg_attr(feature = "ts", ts(skip))]
            skipped: String,
            #[ts(type = "string")]
            adjacent: String,
        }
    "#,
    )
    .unwrap();
    let mut check = Check::default();
    check.visit_file(&syntax);
    assert_eq!(check.missing, ["missing"]);
}
