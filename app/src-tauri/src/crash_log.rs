use ftm_core::paths;
use std::backtrace::Backtrace;
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `ftm-app.log`, next to the daemon's `ftmd.log`. A Finder launch sends stderr nowhere, so this is the only place a
/// panic message survives.
pub fn path() -> PathBuf {
    paths::socket_path().with_file_name("ftm-app.log")
}

/// Past this, a log moves to `*.log.old` when it is next opened, so it never grows without bound but the last crash
/// survives.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Appends every panic (message, location, backtrace) to [`path`], then runs the default hook as before.
pub fn install() {
    rotate(&path(), MAX_LOG_BYTES);
    let record = recorder(path());
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record(info);
        default(info);
    }));
    #[cfg(target_os = "macos")]
    objc_exceptions::install(path());
}

fn recorder(log: PathBuf) -> impl Fn(&PanicHookInfo<'_>) + Send + Sync + 'static {
    move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        append(&log, &format!("thread '{name}' {info}\n{}", Backtrace::force_capture()));
    }
}

pub fn rotate(log: &Path, max_bytes: u64) {
    if std::fs::metadata(log).is_ok_and(|m| m.len() > max_bytes) {
        let _ = std::fs::rename(log, log.with_extension("log.old"));
    }
}

fn append(log: &Path, entry: &str) {
    if let Some(dir) = log.parent() {
        let _ = paths::private_dir(dir, paths::socket_dir_is_ours(dir));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(file, "{} {entry}", timestamp(SystemTime::now()));
    }
}

/// RFC 3339 UTC to the second, like ftmd.log's timestamps.
fn timestamp(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// tao overrides `-[NSApplication sendEvent:]` as `extern "C"`, so an Objective-C exception raised by AppKit while it
/// dispatches an event cannot reach the handler in `-[NSApplication run]`; it aborts the app as
/// `panic_cannot_unwind`, and the crash report never names the exception. Log each one as it is thrown instead.
#[cfg(target_os = "macos")]
mod objc_exceptions {
    use super::{Backtrace, PathBuf, append};
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSException;
    use std::cell::Cell;
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock, PoisonError};

    type Preprocessor = unsafe extern "C" fn(*mut AnyObject) -> *mut AnyObject;

    unsafe extern "C" {
        fn objc_setExceptionPreprocessor(preprocessor: Preprocessor) -> Option<Preprocessor>;
    }

    /// The preprocessor also sees exceptions that AppKit or WebKit throw and catch themselves, possibly over and over.
    /// Each distinct one is logged once, so a new (possibly fatal) one is always recorded; this caps the distinct ones.
    const MAX_DISTINCT: usize = 100;

    static LOG: OnceLock<PathBuf> = OnceLock::new();
    static PREVIOUS: OnceLock<Option<Preprocessor>> = OnceLock::new();
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    thread_local! {
        static LOGGING: Cell<bool> = const { Cell::new(false) };
    }

    pub fn install(log: PathBuf) {
        if LOG.set(log).is_ok() {
            let _ = PREVIOUS.set(unsafe { objc_setExceptionPreprocessor(preprocess) });
        }
    }

    unsafe extern "C" fn preprocess(exception: *mut AnyObject) -> *mut AnyObject {
        // An exception thrown while this one is being logged is passed on unlogged instead of recursing.
        if !LOGGING.replace(true) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| log(exception)));
            LOGGING.set(false);
        }
        match PREVIOUS.get().copied().flatten() {
            Some(previous) => unsafe { previous(exception) },
            None => exception,
        }
    }

    fn log(exception: *mut AnyObject) {
        let Some(log) = LOG.get() else { return };
        let what = match unsafe { exception.as_ref() }.and_then(|e| e.downcast_ref::<NSException>()) {
            Some(e) => format!("{}: {}", e.name(), e.reason().map(|r| r.to_string()).unwrap_or_default()),
            None => "a non-NSException object".into(),
        };
        if !first_sighting(&what) {
            return;
        }
        append(
            log,
            &format!(
                "Objective-C exception thrown (it may still be caught): {what}\n{}",
                Backtrace::force_capture()
            ),
        );
    }

    pub(super) fn first_sighting(what: &str) -> bool {
        let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
        let seen = seen.get_or_insert_with(HashSet::new);
        seen.len() < MAX_DISTINCT && seen.insert(what.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_utc_rfc3339() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let crash = UNIX_EPOCH + Duration::from_secs(1_791_991_192);
        assert_eq!(timestamp(crash), "2026-10-14T15:19:52Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(timestamp(leap), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn panics_are_appended_with_location_and_backtrace() {
        let dir = std::env::temp_dir().join(format!("ftm-crash-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = dir.join("ftm-app.log");
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(recorder(log.clone())));
        let result = std::thread::Builder::new()
            .name("crash-log-test".into())
            .spawn(|| panic!("boom from a test"))
            .unwrap()
            .join();
        std::panic::set_hook(default);
        assert!(result.is_err());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("boom from a test"), "{text}");
        assert!(text.contains("thread 'crash-log-test' panicked at"), "{text}");
        assert!(text.contains("crash_log.rs"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_logs_move_aside_at_startup() {
        let dir = std::env::temp_dir().join(format!("ftm-rotate-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("ftm-app.log");
        std::fs::write(&log, "small").unwrap();
        rotate(&log, 10);
        assert!(log.exists());
        std::fs::write(&log, "the last crash, and more").unwrap();
        rotate(&log, 10);
        assert!(!log.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("ftm-app.log.old")).unwrap(),
            "the last crash, and more"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn repeated_objc_exceptions_are_logged_once_and_new_ones_still_are() {
        assert!(objc_exceptions::first_sighting("NSRangeException: noisy"));
        for _ in 0..500 {
            assert!(!objc_exceptions::first_sighting("NSRangeException: noisy"));
        }
        assert!(objc_exceptions::first_sighting("NSInternalInconsistencyException: fatal"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn objc_exceptions_are_logged_when_thrown() {
        use objc2_foundation::{NSException, NSString};
        let dir = std::env::temp_dir().join(format!("ftm-objc-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = dir.join("ftm-app.log");
        objc_exceptions::install(log.clone());
        let exception = unsafe {
            NSException::exceptionWithName_reason_userInfo(
                &NSString::from_str("NSInternalInconsistencyException"),
                Some(&NSString::from_str(
                    "Assertion failure in NSCampoLightweightUIController.m:1429",
                )),
                None,
            )
        };
        let caught = objc2::exception::catch(|| objc2::exception::throw(NSException::into_exception(exception)));
        assert!(caught.is_err());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("NSInternalInconsistencyException: Assertion failure in NSCampoLightweightUIController.m:1429"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The crash itself: an exception thrown under an `extern "C"` frame (tao's `sendEvent:`) with an Objective-C
    /// handler further up (`-[NSApplication run]`) aborts. Run in a child process, since it kills its process.
    #[cfg(target_os = "macos")]
    #[test]
    fn objc_exception_through_extern_c_aborts_but_leaves_its_reason_in_the_log() {
        use objc2_foundation::{NSException, NSString};
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "FTM_CRASH_LOG_CHILD";

        if let Some(log) = std::env::var_os(CHILD) {
            let log = PathBuf::from(log);
            std::panic::set_hook(Box::new(recorder(log.clone())));
            objc_exceptions::install(log);
            extern "C" fn send_event() {
                let exception = unsafe {
                    NSException::exceptionWithName_reason_userInfo(
                        &NSString::from_str("NSInternalInconsistencyException"),
                        Some(&NSString::from_str("child assertion")),
                        None,
                    )
                };
                objc2::exception::throw(NSException::into_exception(exception))
            }
            let _ = objc2::exception::catch(|| send_event());
            return;
        }

        let dir = std::env::temp_dir().join(format!("ftm-abort-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = dir.join("ftm-app.log");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_log::tests::objc_exception_through_extern_c_aborts_but_leaves_its_reason_in_the_log",
            ])
            .env(CHILD, &log)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(6), "{status:?}");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("NSInternalInconsistencyException: child assertion"), "{text}");
        assert!(text.contains("panic in a function that cannot unwind"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
