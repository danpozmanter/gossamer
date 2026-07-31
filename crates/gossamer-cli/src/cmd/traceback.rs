//! Shared rendering of the bytecode VM's call-stack traceback.
//!
//! Both `gos` (on a fatal runtime error) and `gos test` (on a
//! failing `#[test]`) surface the VM's preserved call chain via
//! [`gossamer_interp::Vm::call_stack_snapshot`]. Keeping the format
//! in one place means the two commands render an identical trailer.

/// Renders a VM call-stack snapshot as an indented, outermost-first
/// trailer. Returns an empty string for an empty stack so callers can
/// append it unconditionally.
#[must_use]
pub(crate) fn render_call_stack(stack: &[String]) -> String {
    if stack.is_empty() {
        return String::new();
    }
    let mut rendered = String::from("\n  call stack (outermost first):");
    for name in stack {
        rendered.push_str("\n    at ");
        rendered.push_str(name);
    }
    rendered
}
