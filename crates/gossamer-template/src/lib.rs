//! Text and context-aware HTML template engines for Gossamer.
//!
//! Relocated below `gossamer-runtime` so the compiled-tier runtime can
//! render templates directly (the `gos_rt_html_template_render_json`
//! shim) without a dependency cycle through `gossamer-std`. The VM
//! tier reaches the same code through `gossamer-std`'s re-exports
//! (`gossamer_std::text::template`, `gossamer_std::html::template`).

pub mod html;
pub mod text;
