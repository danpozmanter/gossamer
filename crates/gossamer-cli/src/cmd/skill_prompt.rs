//! `gos skill-prompt` — prints the embedded Gossamer skill card for
//! AI tooling that needs a quick reference. The canonical source
//! lives in `docs_src/skill_card.md` (mkdocs input); embedding it
//! directly avoids depending on the generated `docs/` output.

const SKILL_CARD: &str = include_str!("../../../../docs_src/skill_card.md");

/// Entry point for `gos skill-prompt`.
pub(crate) fn run() {
    print!("{SKILL_CARD}");
}
