//! Runtime support for `std::panic`.
//! Exposes the `panic` / `catch_unwind` shape Gossamer
//! programs see. The runtime uses the host's std facilities during
//! bring-up; once the native runtime lands, the implementation will
//! switch to DWARF/SEH unwinding directly.

#![forbid(unsafe_code)]

/// Panic payload captured by [`catch_unwind`].
#[derive(Debug, Clone)]
pub struct PanicInfo {
    /// Message recovered from the panic.
    pub message: String,
}

/// Runs `f` and returns its value. If `f` panics with a string
/// payload, the panic is caught and wrapped in a [`PanicInfo`].
pub fn catch_unwind<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> Result<R, PanicInfo> {
    match std::panic::catch_unwind(f) {
        Ok(value) => Ok(value),
        Err(payload) => Err(PanicInfo {
            message: stringify_panic(&payload),
        }),
    }
}

fn stringify_panic(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "panic".to_string()
}

/// Immediate panic helper — callers use `panic!(...)` at the source
/// level; this function exists so derived traits can invoke it
/// without going through the macro.
pub fn panic(message: impl Into<String>) -> ! {
    std::panic::panic_any(message.into());
}

/// Silences the Rust default panic hook. Used in tests so a captured
/// panic does not pollute stderr.
pub fn quiet_panics<R>(f: impl FnOnce() -> R) -> R {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = f();
    std::panic::set_hook(previous);
    result
}
