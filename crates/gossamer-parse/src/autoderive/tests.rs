#[cfg(test)]
mod autoderive_tests {
    use gossamer_lex::SourceMap;

    use crate::ParseError;

    fn serde_field_errors(source: &str) -> Vec<(String, String)> {
        let mut map = SourceMap::new();
        let file = map.add_file("test.gos", source.to_string());
        let (_, diags) = super::parse_with_autoderive(source, file);
        diags
            .into_iter()
            .filter_map(|d| match d.error {
                ParseError::SerdeUnserializableField {
                    field, field_ty, ..
                } => Some((field, field_ty)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unserializable_field_used_in_serde_is_reported() {
        let src = "enum Color { Red, Green }\n\
                   struct Paint { name: String, shade: Color }\n\
                   fn main() { let _ = to_json::<Paint>(Paint { name: \"w\", shade: Color::Red }); }";
        let errs = serde_field_errors(src);
        assert_eq!(errs, vec![("shade".to_string(), "Color".to_string())]);
    }

    #[test]
    fn unserializable_field_never_serialized_is_silent() {
        let src = "enum Color { Red, Green }\n\
                   struct Paint { name: String, shade: Color }\n\
                   fn main() { let p = Paint { name: \"w\", shade: Color::Red }; let _ = p.name; }";
        assert!(serde_field_errors(src).is_empty());
    }

    #[test]
    fn fully_serializable_struct_is_silent() {
        let src = "struct Inner { n: i64 }\n\
                   struct Outer { id: i64, tags: Vec<String>, inner: Inner }\n\
                   fn main() { let _ = to_json::<Outer>(Outer { id: 1, tags: Vec::from([\"a\"]), inner: Inner { n: 2 } }); }";
        assert!(serde_field_errors(src).is_empty());
    }

    #[test]
    fn prescan_ignores_type_keywords_in_comments_and_strings() {
        let src = "fn main() {\n\
                   \tlet _ = \"struct NotAType\"\n\
                   \t// enum AlsoNotAType { A }\n\
                   }\n";
        assert!(!super::source_may_need_ast_synthesis(src));
        assert_eq!(super::augment_source(src), src);
    }

    #[test]
    fn prescan_detects_real_type_declarations() {
        assert!(super::source_may_need_ast_synthesis(
            "struct Point { x: i64 }\nfn main() {}\n"
        ));
        assert!(super::source_may_need_ast_synthesis(
            "enum Color { Red }\nfn main() {}\n"
        ));
    }

    #[test]
    fn validator_only_source_still_augments_without_type_declarations() {
        let src = "fn main() { let _ = regex!(\"^[a]+$\") }\n";
        let augmented = super::augment_source(src);
        assert!(augmented.contains("__gos_regex_validate"));
        assert!(augmented.starts_with(src));
    }
}
