//! Wrapper that exposes a small slice of the `unic-segment`
//! crate (Unicode grapheme iteration) to Gossamer programs.

use gossamer_binding::register_module;
use unic_segment::Graphemes;

register_module!(
    binding,
    path: "unic_segment",
    symbol_prefix: unic_segment,
    doc: "Unicode grapheme iteration (wraps unic-segment).",

    fn graphemes(s: String) -> Vec<String> {
        Graphemes::new(&s).map(str::to_string).collect()
    }

    fn grapheme_count(s: String) -> i64 {
        let n = Graphemes::new(&s).count();
        i64::try_from(n).unwrap_or(i64::MAX)
    }
);

/// Linker-hook required by the runner template.
pub fn __bindings_force_link() {
    binding::force_link();
}
