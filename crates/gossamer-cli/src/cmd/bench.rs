//! `gos bench FILE [--iterations N]` — runs every `#[bench]`
//! function in `FILE` for `iterations` rounds and reports the mean
//! wall-clock cost per call.

use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::cmd::attr_walk::{item_has_attr, run_selected_fns};

/// Entry point for `gos bench`.
pub(crate) fn run(file: &PathBuf, iterations: u32) -> Result<()> {
    let iters = iterations.max(1);
    let (runs, failures, total_nanos) =
        run_selected_fns(file, |item| item_has_attr(item, "bench"), iters)?;
    if runs == 0 && failures == 0 {
        println!("bench: no #[bench] functions found in {}", file.display());
        return Ok(());
    }
    if failures > 0 {
        return Err(anyhow!("{failures} bench function(s) panicked"));
    }
    let mean = if runs == 0 {
        0
    } else {
        total_nanos / u128::from(runs)
    };
    println!("bench: {runs} iterations across #[bench] functions; mean {mean} ns/iter");
    Ok(())
}
