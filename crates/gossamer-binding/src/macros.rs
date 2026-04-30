//! Declarative macros that turn idiomatic Rust function items
//! into [`crate::ItemFn`] entries inside a [`crate::Module`]
//! registered in [`crate::REGISTRY`].
//!
//! Two binding shapes are supported:
//!
//! - `fn name(arg: T, ...) -> R { body }` — plain binding fn.
//! - `cb_fn name(dispatch, arg: T, ...) -> R { body }` — same as
//!   plain, but the body has access to `dispatch: &mut dyn
//!   NativeDispatch` so it can re-enter the interpreter through
//!   [`crate::NativeDispatch::call_value`] (`Terminal::draw` and
//!   any other higher-order binding APIs).

/// Internal: counts the supplied identifiers at compile time.
#[doc(hidden)]
#[macro_export]
macro_rules! __binding_count {
    () => { 0_usize };
    ($head:ident $($tail:ident)*) => { 1_usize + $crate::__binding_count!($($tail)*) };
}

/// Declares one or more Gossamer-callable functions and registers
/// them as a single [`crate::Module`].
///
/// The required `symbol_prefix` literal is the C-ABI export
/// prefix the macro stamps on every plain `fn` thunk
/// (`gos_binding_<symbol_prefix>__<fn_name>`). It must match the
/// scheme [`crate::mangle_binding_symbol`] uses against the
/// `path` literal — the codegen relies on that equivalence to
/// emit calls into the binding from compiled `.gos` source.
#[macro_export]
macro_rules! register_module {
    (
        $modname:ident,
        path: $path:literal,
        symbol_prefix: $sym:ident,
        doc: $doc:literal,
        $($body:tt)*
    ) => {
        $crate::__rm_munch! {
            $modname, $path, $sym, $doc,
            simple = [],
            cb = [],
            rest = [ $($body)* ]
        }
    };

    // Backwards-compatible form without `symbol_prefix:` — only
    // the interpreter thunks are emitted, so binding fns from
    // these modules are reachable from `gos run` but not
    // `gos build`. Documented as the legacy path; new bindings
    // should specify `symbol_prefix:` explicitly.
    (
        $modname:ident,
        path: $path:literal,
        doc: $doc:literal,
        $($body:tt)*
    ) => {
        $crate::__rm_munch! {
            $modname, $path, __nosym, $doc,
            simple = [],
            cb = [],
            rest = [ $($body)* ]
        }
    };
}

/// Internal: tt-muncher that walks the binding-fn list and
/// classifies each entry as plain or callback-aware before the
/// final emit step.
#[doc(hidden)]
#[macro_export]
macro_rules! __rm_munch {
    // ---- terminal: emit module ---------------------------------
    (
        $modname:ident, $path:literal, $sym:tt, $doc:literal,
        simple = [ $({
            $sn:ident,
            ( $($sa:ident : $st:ty),* ),
            $sr:ty,
            $sb:block
        })* ],
        cb = [ $({
            $cn:ident,
            $cdisp:ident,
            ( $($ca:ident : $ct:ty),* ),
            $cr:ty,
            $cb_body:block
        })* ],
        rest = []
    ) => {
        #[allow(non_snake_case, dead_code, clippy::missing_docs_in_private_items)]
        mod $modname {
            use super::*;

            $crate::__paste::paste! {
                $(
                    pub fn $sn($($sa : $st),*) -> $sr $sb

                    #[allow(non_snake_case)]
                    pub fn [< __thunk_ $sn >](
                        _dispatch: &mut dyn $crate::NativeDispatch,
                        args: &[$crate::Value],
                    ) -> $crate::RuntimeResult<$crate::Value> {
                        let expected = $crate::__binding_count!($($sa)*);
                        if args.len() != expected {
                            return Err($crate::RuntimeError::Arity {
                                expected,
                                found: args.len(),
                            });
                        }
                        let mut iter = args.iter();
                        $(
                            let $sa: $st =
                                <$st as $crate::FromGos>::from_gos(iter.next().unwrap())?;
                        )*
                        let out: $sr = $sn($($sa),*);
                        Ok(<$sr as $crate::ToGos>::to_gos(out))
                    }

                    $crate::__rm_emit_native_export! {
                        $sym, $sn, ( $($sa : $st),* ), $sr
                    }
                )*

                $(
                    pub fn $cn(
                        $cdisp: &mut dyn $crate::NativeDispatch,
                        $($ca : $ct),*
                    ) -> $cr $cb_body

                    #[allow(non_snake_case)]
                    pub fn [< __thunk_ $cn >](
                        _dispatch: &mut dyn $crate::NativeDispatch,
                        args: &[$crate::Value],
                    ) -> $crate::RuntimeResult<$crate::Value> {
                        let expected = $crate::__binding_count!($($ca)*);
                        if args.len() != expected {
                            return Err($crate::RuntimeError::Arity {
                                expected,
                                found: args.len(),
                            });
                        }
                        let mut iter = args.iter();
                        $(
                            let $ca: $ct =
                                <$ct as $crate::FromGos>::from_gos(iter.next().unwrap())?;
                        )*
                        let out: $cr = $cn(_dispatch, $($ca),*);
                        Ok(<$cr as $crate::ToGos>::to_gos(out))
                    }
                )*

                pub static ITEMS: &[$crate::ItemFn] = &[
                    $(
                        $crate::ItemFn {
                            name: stringify!($sn),
                            call: [< __thunk_ $sn >],
                            signature: $crate::Signature {
                                params: &[
                                    $( <$st as $crate::SigType>::TYPE ),*
                                ],
                                ret: <$sr as $crate::SigType>::TYPE,
                            },
                            doc: "",
                        },
                    )*
                    $(
                        $crate::ItemFn {
                            name: stringify!($cn),
                            call: [< __thunk_ $cn >],
                            signature: $crate::Signature {
                                params: &[
                                    $( <$ct as $crate::SigType>::TYPE ),*
                                ],
                                ret: <$cr as $crate::SigType>::TYPE,
                            },
                            doc: "",
                        },
                    )*
                ];
            }

            pub static MODULE: $crate::Module = $crate::Module {
                path: $path,
                doc: $doc,
                items: ITEMS,
            };

            #[$crate::linkme::distributed_slice($crate::REGISTRY)]
            #[linkme(crate = $crate::linkme)]
            #[allow(non_upper_case_globals)]
            static REGISTERED: &'static $crate::Module = &MODULE;

            /// Emits a hard reference to `MODULE` so the linker
            /// keeps the [`linkme`] entry alive across LTO. Every
            /// binding crate must expose `pub fn
            /// __bindings_force_link()` at its crate root that
            /// chains into this; see
            /// [`crate::register_module!`] for the convention.
            pub fn force_link() {
                let _: &'static $crate::Module = &MODULE;
            }
        }
    };

    // ---- munch: cb_fn ------------------------------------------
    (
        $modname:ident, $path:literal, $sym:tt, $doc:literal,
        simple = [ $($simple:tt)* ],
        cb = [ $($cb:tt)* ],
        rest = [
            cb_fn $name:ident( $disp:ident, $($arg:ident : $argty:ty),* $(,)? ) -> $ret:ty $body:block
            $($rest:tt)*
        ]
    ) => {
        $crate::__rm_munch! {
            $modname, $path, $sym, $doc,
            simple = [ $($simple)* ],
            cb = [ $($cb)* {
                $name,
                $disp,
                ( $($arg : $argty),* ),
                $ret,
                $body
            } ],
            rest = [ $($rest)* ]
        }
    };

    // ---- munch: plain fn ---------------------------------------
    (
        $modname:ident, $path:literal, $sym:tt, $doc:literal,
        simple = [ $($simple:tt)* ],
        cb = [ $($cb:tt)* ],
        rest = [
            fn $name:ident( $($arg:ident : $argty:ty),* $(,)? ) -> $ret:ty $body:block
            $($rest:tt)*
        ]
    ) => {
        $crate::__rm_munch! {
            $modname, $path, $sym, $doc,
            simple = [ $($simple)* {
                $name,
                ( $($arg : $argty),* ),
                $ret,
                $body
            } ],
            cb = [ $($cb)* ],
            rest = [ $($rest)* ]
        }
    };
}

/// Internal: emits the `extern "C"` thunk for one plain binding
/// fn. Skipped when `$sym` is the `__nosym` sentinel (the
/// legacy `register_module!` form without `symbol_prefix:`).
#[doc(hidden)]
#[macro_export]
macro_rules! __rm_emit_native_export {
    (__nosym, $name:ident, ( $($arg:ident : $argty:ty),* ), $ret:ty) => {};
    ($sym:ident, $name:ident, ( $($arg:ident : $argty:ty),* ), $ret:ty) => {
        $crate::__paste::paste! {
            #[unsafe(no_mangle)]
            #[allow(non_snake_case, unused_variables, unused_unsafe)]
            pub extern "C" fn [< gos_binding_ $sym __ $name >](
                $( $arg : <$argty as $crate::native::BindingAbi>::Input ),*
            ) -> <$ret as $crate::native::BindingAbi>::Output {
                let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    $(
                        // SAFETY: the codegen guarantees `$arg`
                        // is the C-ABI `Input` shape declared
                        // by `BindingAbi` for `$argty`.
                        let $arg: $argty =
                            unsafe { <$argty as $crate::native::BindingAbi>::from_input($arg) };
                    )*
                    let out: $ret = $name($($arg),*);
                    <$ret as $crate::native::BindingAbi>::to_output(out)
                }));
                result.unwrap_or_else(|_| {
                    <<$ret as $crate::native::BindingAbi>::Output as ::core::default::Default>::default()
                })
            }
        }
    };
}
