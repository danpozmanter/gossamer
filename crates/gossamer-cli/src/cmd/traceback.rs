//! Shared rendering of the bytecode VM's call-stack traceback.
//!
//! Both `gos` (on a fatal runtime error) and `gos test` (on a
//! failing `#[test]`) surface the VM's preserved call chain via
//! [`gossamer_interp::Vm::call_stack_frames`]. Keeping the format
//! in one place means the two commands render an identical trailer.

use std::fmt::Write as _;

/// Renders a VM call-stack snapshot as an indented, outermost-first
/// trailer. Returns an empty string for an empty stack so callers can
/// append it unconditionally.
///
/// A run of identical adjacent frames collapses to one line with a
/// repeat count: a recursion that reaches the depth cap is thousands of
/// frames deep, and the shape of the cycle is what identifies it.
#[must_use]
pub(crate) fn render_call_stack(stack: &[gossamer_interp::CallStackFrame]) -> String {
    if stack.is_empty() {
        return String::new();
    }
    let mut rendered = String::from("\n  call stack (outermost first):");
    let mut index = 0usize;
    while index < stack.len() {
        let frame = &stack[index];
        let mut repeats = 1usize;
        while index + repeats < stack.len() && same_site(frame, &stack[index + repeats]) {
            repeats += 1;
        }
        rendered.push_str("\n    at ");
        rendered.push_str(&frame.function);
        match (&frame.file, frame.line, frame.column) {
            (Some(file), Some(line), Some(column)) => {
                let _ = write!(rendered, " ({file}:{line}:{column})");
            }
            (Some(file), Some(line), None) => {
                let _ = write!(rendered, " ({file}:{line})");
            }
            _ => {}
        }
        if repeats > 1 {
            let _ = write!(rendered, " x{repeats}");
        }
        index += repeats;
    }
    rendered
}

/// Whether two frames name the same function at the same source position,
/// which is what makes them one repeat of a recursion rather than distinct
/// steps through it.
fn same_site(a: &gossamer_interp::CallStackFrame, b: &gossamer_interp::CallStackFrame) -> bool {
    a.function == b.function && a.file == b.file && a.line == b.line && a.column == b.column
}
