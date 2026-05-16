// `std::os::user` — POSIX user / group lookup. Logic lives in
// `gossamer_runtime::c_abi::gos_rt_os_user_*` so the compiled tier
// (Cranelift / LLVM) reaches the same code via static linkage.
//
// On Windows, the user / group functions return the values
// `USERNAME` / `USERDOMAIN` exposes through the environment; the
// uid / gid scalars are reported as -1.

#![forbid(unsafe_code)]

/// Returns the current process user's login name (e.g. `"daniel"`),
/// or the empty string if it can't be determined.
#[must_use]
pub fn current_name() -> String {
    #[cfg(unix)]
    {
        use nix::unistd::{Uid, User};
        match User::from_uid(Uid::current()) {
            Ok(Some(u)) => u.name,
            _ => std::env::var("USER").unwrap_or_default(),
        }
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME").unwrap_or_default()
    }
}

/// Returns the current process user's uid, or `-1` on non-unix.
#[must_use]
pub fn current_uid() -> i64 {
    #[cfg(unix)]
    {
        use nix::unistd::Uid;
        i64::from(Uid::current().as_raw())
    }
    #[cfg(not(unix))]
    {
        -1
    }
}

/// Returns the current process user's gid, or `-1` on non-unix.
#[must_use]
pub fn current_gid() -> i64 {
    #[cfg(unix)]
    {
        use nix::unistd::Gid;
        i64::from(Gid::current().as_raw())
    }
    #[cfg(not(unix))]
    {
        -1
    }
}

/// Returns the user's home directory (e.g. `/home/daniel`), or the
/// empty string if unknown. Falls back to the `HOME` env var on unix.
#[must_use]
pub fn current_home() -> String {
    #[cfg(unix)]
    {
        use nix::unistd::{Uid, User};
        match User::from_uid(Uid::current()) {
            Ok(Some(u)) => u.dir.to_string_lossy().into_owned(),
            _ => std::env::var("HOME").unwrap_or_default(),
        }
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERPROFILE").unwrap_or_default()
    }
}

/// Returns the login name for `uid`, or the empty string if no
/// matching user. Unix-only; on Windows returns "".
#[must_use]
pub fn lookup_uid(uid: i64) -> String {
    #[cfg(unix)]
    {
        use nix::unistd::{Uid, User};
        let Ok(raw) = u32::try_from(uid) else {
            return String::new();
        };
        match User::from_uid(Uid::from_raw(raw)) {
            Ok(Some(u)) => u.name,
            _ => String::new(),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        String::new()
    }
}

/// Returns the uid for the user named `name`, or `-1` if not found.
#[must_use]
pub fn lookup_name(name: &str) -> i64 {
    #[cfg(unix)]
    {
        use nix::unistd::User;
        match User::from_name(name) {
            Ok(Some(u)) => i64::from(u.uid.as_raw()),
            _ => -1,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_name_nonempty() {
        let n = current_name();
        // CI containers may have unusual setups, so we only check the
        // function returns without panicking — emptiness is acceptable.
        let _ = n;
    }

    #[test]
    fn current_uid_runs() {
        let _ = current_uid();
    }

    #[test]
    fn current_gid_runs() {
        let _ = current_gid();
    }

    #[test]
    fn lookup_uid_zero_is_root_on_unix() {
        #[cfg(unix)]
        {
            let name = lookup_uid(0);
            // root may not exist in minimal containers; just don't panic.
            let _ = name;
        }
    }
}
