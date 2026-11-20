//! Explicit access to the calling process's environment.
//!
//! Every function here changes or reads process-wide state. That is the point:
//! the expansion and path helpers stay pure unless a caller supplies process
//! callbacks, while these functions edit the environment the operating system
//! gives the process.
//!
//! Reads and writes are serialized among this crate's own calls by a private
//! mutex, because `std::env::set_var` and `std::env::remove_var` require that no
//! other thread reads or writes the environment at the same time. The mutex
//! cannot constrain code outside this crate: the caller must not touch the
//! process environment from another thread while one of these calls runs.

use std::sync::{Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;

use crate::{
    EnvError, Platform, Result, append_path_platform, prepend_path_platform, validate_key,
};

/// Serializes this crate's own process-environment access.
///
/// A panic while the lock is held poisons it. Continuing with the guarded data
/// is correct here because the guarded value is `()`, so the tests and the
/// library use the same recovery.
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

fn environment_lock() -> MutexGuard<'static, ()> {
    ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Reports whether `key` can be passed to a standard-library environment
/// function without panicking.
///
/// A process can contain names this crate would never write itself, so a read
/// accepts anything the platform accepts. This is deliberately weaker than the
/// name rule [`set_var`] and [`unset_var`] enforce.
fn queryable(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.contains('\0')
}

/// Reads a value and keeps invalid Unicode visible as absence.
fn read_variable(key: &str) -> Option<String> {
    std::env::var_os(key)?.into_string().ok()
}

/// Writes a value, refusing a key or value the operating system cannot store.
///
/// The caller must hold [`ENVIRONMENT_LOCK`].
#[allow(unsafe_code)]
fn write_variable(key: &str, value: &str) -> Result<()> {
    validate_key(key)?;
    if value.as_bytes().contains(&0) {
        return Err(EnvError::NulValue);
    }
    // SAFETY: every call in this crate takes ENVIRONMENT_LOCK for the whole
    // read-modify-write, so this crate's calls are serialized. Code outside
    // this crate can still race, which is the caller's documented obligation.
    unsafe { std::env::set_var(key, value) };
    Ok(())
}

/// Removes a value, refusing a key the operating system cannot accept.
///
/// The caller must hold [`ENVIRONMENT_LOCK`].
#[allow(unsafe_code)]
fn remove_variable(key: &str) -> Result<()> {
    validate_key(key)?;
    // SAFETY: every call in this crate takes ENVIRONMENT_LOCK for the whole
    // operation, so this crate's calls are serialized. Code outside this crate
    // can still race, which is the caller's documented obligation.
    unsafe { std::env::remove_var(key) };
    Ok(())
}

/// Returns the process value of `key`, or `None` when it is not set.
///
/// An empty value is `Some("")`, which is stricter than Go's `Get`: use
/// [`has_var`] when the question is whether the name exists. A name this
/// platform cannot query, or a value that is not valid Unicode, is `None`.
///
/// Name matching follows the operating system, exactly as [`has_var`]
/// describes: case-insensitive on Windows, case-sensitive elsewhere.
///
/// This reads the calling process's environment. The crate serializes its own
/// calls with a private mutex, but the caller must not mutate the environment
/// from another thread while this runs, for the reason `std::env::var_os`
/// documents.
#[must_use]
pub fn get_var(key: &str) -> Option<String> {
    if !queryable(key) {
        return None;
    }
    let _guard = environment_lock();
    read_variable(key)
}

/// Sets `key` in the calling process's environment.
///
/// This writes process-wide state. The crate serializes its own calls with a
/// private mutex, but the caller must not read or write the environment from
/// another thread while this runs; `std::env::set_var` carries the same
/// requirement.
///
/// # Errors
///
/// Returns [`EnvError::InvalidKey`] when `key` is not a shell name, and
/// [`EnvError::NulValue`] when `value` contains a NUL byte. In both cases the
/// process environment is not touched.
///
/// ```
/// use neotales_env::{get_var, set_var};
///
/// set_var("NEOTALES_ENV_EXAMPLE", "1").unwrap();
/// assert_eq!(get_var("NEOTALES_ENV_EXAMPLE").as_deref(), Some("1"));
/// ```
pub fn set_var(key: &str, value: &str) -> Result<()> {
    let _guard = environment_lock();
    write_variable(key, value)
}

/// Reports whether `key` exists in the calling process's environment.
///
/// This includes a variable whose value is empty, and unlike [`get_var`] it says
/// nothing about whether the value is Unicode.
///
/// Name matching is the operating system's rule, which is the rule [`Platform`]
/// records: Windows environment names are case-insensitive, so `has_var("Path")`
/// is true when only `PATH` is set, while every other platform compares the
/// exact bytes, so `has_var("path")` is false when only `PATH` is set.
/// [`std::env::var_os`] asks the operating system, so nothing here folds case
/// that the platform would not fold itself. [`vars`] lists the spellings the
/// operating system supplies; a Windows environment block can collapse two
/// case spellings of one name into a single entry.
///
/// This reads the calling process's environment. The crate serializes its own
/// calls with a private mutex, but the caller must not mutate the environment
/// from another thread while this runs.
#[must_use]
pub fn has_var(key: &str) -> bool {
    if !queryable(key) {
        return false;
    }
    let _guard = environment_lock();
    std::env::var_os(key).is_some()
}

/// Removes `key` from the calling process's environment.
///
/// This writes process-wide state. The crate serializes its own calls with a
/// private mutex, but the caller must not read or write the environment from
/// another thread while this runs; `std::env::remove_var` carries the same
/// requirement.
///
/// # Errors
///
/// Returns [`EnvError::InvalidKey`] when `key` is not a shell name, and does not
/// call the operating system in that case.
pub fn unset_var(key: &str) -> Result<()> {
    let _guard = environment_lock();
    remove_variable(key)
}

/// Returns the process environment in the order the platform lists it.
///
/// Empty values are dropped, matching Go's `All`. A value that itself contains
/// `=` is kept: the operating system already supplies the pair split at the
/// first `=` in the entry. Pairs that are not valid Unicode on either side are
/// skipped rather than causing a panic. If the platform supplies the same key
/// twice, the last value wins and the first position is kept. On Windows, where
/// names are case-insensitive, two case spellings of one name are one entry to
/// the operating system, so the map holds the single entry the OS supplies.
///
/// This reads the calling process's environment. The crate serializes its own
/// calls with a private mutex, but the caller must not mutate the environment
/// from another thread while this runs.
#[must_use]
pub fn vars() -> IndexMap<String, String> {
    let _guard = environment_lock();
    let mut result = IndexMap::new();
    for (key, value) in std::env::vars_os() {
        let (Ok(key), Ok(value)) = (key.into_string(), value.into_string()) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        result.insert(key, value);
    }
    result
}

/// Replaces the platform's `PATH` value in the calling process.
///
/// This writes process-wide state. The crate serializes its own calls with a
/// private mutex, but the caller must not read or write the environment from
/// another thread while this runs.
///
/// # Errors
///
/// Returns [`EnvError::NulValue`] when `value` contains a NUL byte. The `PATH`
/// key itself is always a valid name.
pub fn set_path(value: &str) -> Result<()> {
    let _guard = environment_lock();
    write_variable(Platform::host().path_key(), value)
}

/// Puts `entry` at the front of the process `PATH`.
///
/// The call is a no-op returning `Ok(())` when `entry` is already the first
/// entry, matching Go's `PrependPath`. An unset or empty `PATH` is treated as
/// having no entries, so no leading separator is introduced.
///
/// This writes process-wide state. The crate serializes its own calls with a
/// private mutex, but the caller must not read or write the environment from
/// another thread while this runs.
///
/// # Errors
///
/// Returns [`EnvError::NulValue`] when `entry` contains a NUL byte.
///
/// ```
/// use neotales_env::{has_path, prepend_path};
///
/// prepend_path("/opt/bin").unwrap();
/// assert!(has_path("/opt/bin"));
/// ```
pub fn prepend_path(entry: &str) -> Result<()> {
    let platform = Platform::host();
    let key = platform.path_key();
    let _guard = environment_lock();
    let current = read_variable(key);
    let already_first = current.as_deref().is_some_and(|value| {
        value
            .split(platform.path_separator())
            .next()
            .is_some_and(|first| platform.entries_equal(first, entry))
    });
    if already_first {
        return Ok(());
    }
    let value = prepend_path_platform(
        current.as_deref(),
        std::slice::from_ref(&entry.to_owned()),
        platform,
    );
    write_variable(key, &value)
}

/// Puts `entry` at the end of the process `PATH`.
///
/// The call is a no-op returning `Ok(())` when `entry` is already the last
/// entry, matching Go's `AppendPath`. An unset or empty `PATH` is treated as
/// having no entries, so no trailing separator is introduced.
///
/// This writes process-wide state. The crate serializes its own calls with a
/// private mutex, but the caller must not read or write the environment from
/// another thread while this runs.
///
/// # Errors
///
/// Returns [`EnvError::NulValue`] when `entry` contains a NUL byte.
pub fn append_path(entry: &str) -> Result<()> {
    let platform = Platform::host();
    let key = platform.path_key();
    let _guard = environment_lock();
    let current = read_variable(key);
    let already_last = current.as_deref().is_some_and(|value| {
        value
            .split(platform.path_separator())
            .next_back()
            .is_some_and(|last| platform.entries_equal(last, entry))
    });
    if already_last {
        return Ok(());
    }
    let value = append_path_platform(
        current.as_deref(),
        std::slice::from_ref(&entry.to_owned()),
        platform,
    );
    write_variable(key, &value)
}

/// Reports whether `entry` is in the process `PATH`.
///
/// This is `false` when `PATH` is unset. Empty entries never match, so an empty
/// entry left by a trailing separator cannot be found by a query such as
/// `has_path("")`.
///
/// Entries are compared by the rules of [`Platform::host()`], the same rule
/// [`has_var`] applies to names: Windows ignores case, so `C:\Tools` and
/// `c:\tools` are the same entry, while every other platform compares the exact
/// bytes, so `/Tools` and `/tools` are different entries. The comparison is on
/// entries, not on the variable name, and a Unix entry is never folded to a
/// canonical spelling before it is compared.
///
/// This reads the calling process's environment. The crate serializes its own
/// calls with a private mutex, but the caller must not mutate the environment
/// from another thread while this runs.
#[must_use]
pub fn has_path(entry: &str) -> bool {
    let platform = Platform::host();
    let _guard = environment_lock();
    let Some(current) = read_variable(platform.path_key()) else {
        return false;
    };
    current
        .split(platform.path_separator())
        .any(|existing| !existing.is_empty() && platform.entries_equal(existing, entry))
}

#[cfg(test)]
mod tests {
    use std::sync::{MutexGuard, PoisonError};

    use super::{
        append_path, get_var, has_path, has_var, prepend_path, set_path, set_var, unset_var, vars,
    };
    use crate::{EnvError, Platform, TEST_ENV_LOCK};

    fn test_lock() -> MutexGuard<'static, ()> {
        TEST_ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Restores `PATH` and unsets every key a test touched when it drops.
    ///
    /// Dropped while the test lock is still held, so the restore cannot race
    /// another test.
    struct Restore {
        keys: Vec<String>,
        path: Option<String>,
    }

    impl Restore {
        fn capture() -> Self {
            Self {
                keys: Vec::new(),
                path: get_var(Platform::host().path_key()),
            }
        }

        fn track(&mut self, key: &str) {
            if !self.keys.iter().any(|tracked| tracked == key) {
                self.keys.push(key.to_owned());
            }
        }
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            for key in &self.keys {
                let _ = unset_var(key);
            }
            match &self.path {
                Some(value) => {
                    let _ = set_path(value);
                }
                None => {
                    let _ = unset_var(Platform::host().path_key());
                }
            }
        }
    }

    const KEY: &str = "NEOTALES_ENV_TEST_VALUE";
    /// The same name as [`KEY`] in another case, for the case-rule test.
    const LOWER_KEY: &str = "neotales_env_test_value";
    const EMPTY_KEY: &str = "NEOTALES_ENV_TEST_EMPTY";
    const EQUALS_KEY: &str = "NEOTALES_ENV_TEST_EQUALS";
    const EXPAND_KEY: &str = "NEOTALES_ENV_TEST_EXPAND";
    const ASSIGN_KEY: &str = "NEOTALES_ENV_TEST_ASSIGN";

    #[test]
    fn set_get_has_and_unset_round_trip() {
        let _guard = test_lock();
        let mut restore = Restore::capture();
        restore.track(KEY);
        let _ = unset_var(KEY);

        assert!(!has_var(KEY));
        assert_eq!(get_var(KEY), None);

        set_var(KEY, "value").unwrap();
        assert_eq!(get_var(KEY).as_deref(), Some("value"));
        assert!(has_var(KEY));

        unset_var(KEY).unwrap();
        assert_eq!(get_var(KEY), None);
        assert!(!has_var(KEY));
    }

    /// The process name lookup follows the operating system, which is the rule
    /// `Platform::host()` records. The variable is written once and queried in
    /// another case, so a regression to unconditional folding (or to none)
    /// fails on at least one platform.
    #[test]
    fn has_var_uses_the_host_platform_case_rule() {
        let _guard = test_lock();
        let mut restore = Restore::capture();
        restore.track(KEY);
        restore.track(LOWER_KEY);
        let _ = unset_var(LOWER_KEY);
        let _ = unset_var(KEY);

        set_var(KEY, "value").unwrap();
        assert!(has_var(KEY));
        match Platform::host() {
            // `std::env::var_os` asks the OS, and Windows names ignore case.
            Platform::Windows => assert!(has_var(LOWER_KEY)),
            Platform::Unix => assert!(!has_var(LOWER_KEY)),
        }
        assert_eq!(get_var(LOWER_KEY).is_some(), has_var(LOWER_KEY));
    }

    #[test]
    fn an_empty_value_is_set_but_is_not_listed() {
        let _guard = test_lock();
        let mut restore = Restore::capture();
        restore.track(EMPTY_KEY);

        set_var(EMPTY_KEY, "").unwrap();
        assert_eq!(get_var(EMPTY_KEY), Some(String::new()));
        assert!(has_var(EMPTY_KEY));
        assert!(!vars().contains_key(EMPTY_KEY));
    }

    #[test]
    fn an_invalid_key_is_refused_before_the_environment_is_touched() {
        let _guard = test_lock();
        let _restore = Restore::capture();

        for key in ["1BAD", "BAD-KEY", "", "A=B", "A\0B"] {
            let error = set_var(key, "value").unwrap_err();
            assert_eq!(
                error,
                EnvError::InvalidKey {
                    key: key.to_owned()
                },
                "set_var accepted {key:?}"
            );
            assert_eq!(unset_var(key).unwrap_err(), error);
            assert!(!has_var(key), "set_var stored {key:?}");
        }
    }

    #[test]
    fn vars_keeps_a_value_that_contains_an_equals_sign() {
        let _guard = test_lock();
        let mut restore = Restore::capture();
        restore.track(EQUALS_KEY);

        set_var(EQUALS_KEY, "left=right").unwrap();
        let listing = vars();
        assert_eq!(
            listing.get(EQUALS_KEY).map(String::as_str),
            Some("left=right")
        );
    }

    #[test]
    fn path_functions_read_write_prepend_and_append() {
        let _guard = test_lock();
        let _restore = Restore::capture();
        let platform = super::Platform::host();

        let original = platform.join_path(["/first", "/last"]);
        set_path(&original).unwrap();
        assert_eq!(get_var(platform.path_key()).as_deref(), Some(&*original));
        assert!(has_path("/first"));
        assert!(has_path("/last"));
        assert!(!has_path("/absent"));

        prepend_path("/first").unwrap();
        assert_eq!(get_var(platform.path_key()).as_deref(), Some(&*original));
        append_path("/last").unwrap();
        assert_eq!(get_var(platform.path_key()).as_deref(), Some(&*original));

        prepend_path("/new").unwrap();
        assert_eq!(
            get_var(platform.path_key()).as_deref(),
            Some(&*platform.join_path(["/new", "/first", "/last"]))
        );
        append_path("/tail").unwrap();
        assert_eq!(
            get_var(platform.path_key()).as_deref(),
            Some(&*platform.join_path(["/new", "/first", "/last", "/tail"]))
        );
        assert!(has_path("/tail"));
    }

    #[test]
    fn has_path_is_false_when_path_is_unset() {
        let _guard = test_lock();
        let _restore = Restore::capture();
        let platform = super::Platform::host();

        unset_var(platform.path_key()).unwrap();
        assert_eq!(get_var(platform.path_key()), None);
        assert!(!has_path("/anything"));
        assert!(!has_path(""));
    }

    /// The process query follows `Platform::host()`, so the expectation is
    /// conditional: Windows folds case, every other platform does not. The
    /// entry is written in the platform's own `PATH` spelling, and the query
    /// differs only in case, which is the whole assertion.
    #[test]
    fn has_path_uses_the_host_platform_case_rule() {
        let _guard = test_lock();
        let _restore = Restore::capture();
        let platform = super::Platform::host();

        match platform {
            super::Platform::Windows => {
                set_path(r"C:\Tools").unwrap();
                assert!(has_path(r"C:\Tools"));
                assert!(has_path(r"c:\tools"));
            }
            super::Platform::Unix => {
                set_path("/Tools").unwrap();
                assert!(has_path("/Tools"));
                assert!(!has_path("/tools"));
                // A same-spelling entry is still found after the case check
                // above, so the negative is about case and not about listing.
                set_path("/tools").unwrap();
                assert!(has_path("/tools"));
            }
        }
    }

    #[test]
    fn expand_defaults_read_and_write_the_process() {
        let _guard = test_lock();
        let mut restore = Restore::capture();
        restore.track(EXPAND_KEY);
        restore.track(ASSIGN_KEY);
        let _ = unset_var(ASSIGN_KEY);

        set_var(EXPAND_KEY, "/home/dev").unwrap();
        let mut options = crate::ExpandOptions::new();
        assert_eq!(
            options.expand(&format!("${EXPAND_KEY}/bin")).unwrap(),
            "/home/dev/bin"
        );
        // Windows references and command substitution stay off by default.
        assert_eq!(
            options.expand(&format!("%{EXPAND_KEY}%")).unwrap(),
            format!("%{EXPAND_KEY}%")
        );
        assert_eq!(options.expand("$(echo hi)").unwrap(), "$(echo hi)");

        // The default setter really writes the process.
        assert_eq!(
            options
                .expand(&format!("${{{ASSIGN_KEY}:=fallback}}"))
                .unwrap(),
            "fallback"
        );
        assert_eq!(get_var(ASSIGN_KEY).as_deref(), Some("fallback"));

        // The free function takes options by value and uses the same defaults.
        assert_eq!(
            crate::expand(&format!("${EXPAND_KEY}"), crate::ExpandOptions::new()).unwrap(),
            "/home/dev"
        );

        // A custom getter fully replaces the process lookup.
        let mut custom = crate::ExpandOptions::new()
            .with_get(|name: &str| (name == "FROM_GET").then(|| "custom".to_owned()));
        assert_eq!(custom.expand("$FROM_GET").unwrap(), "custom");
        assert_eq!(
            custom
                .expand(&format!("${{{EXPAND_KEY}:-missing}}"))
                .unwrap(),
            "missing"
        );
        assert_eq!(get_var(EXPAND_KEY).as_deref(), Some("/home/dev"));
    }

    #[test]
    fn the_public_names_are_crate_root_functions() {
        // Compiles only while the names resolve at the crate root.
        let _: fn(&str) -> Option<String> = crate::get_var;
        let _: fn(&str, &str) -> crate::Result<()> = crate::set_var;
        let _: fn(&str) -> bool = crate::has_var;
        let _: fn(&str) -> crate::Result<()> = crate::unset_var;
        let _: fn() -> indexmap::IndexMap<String, String> = crate::vars;
        let _: fn(&str) -> crate::Result<()> = crate::set_path;
        let _: fn(&str) -> crate::Result<()> = crate::prepend_path;
        let _: fn(&str) -> crate::Result<()> = crate::append_path;
        let _: fn(&str) -> bool = crate::has_path;
    }
}
