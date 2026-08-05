/// Crash-injection failpoint (T6). When the `SNAG_FAILPOINT` env var equals
/// `stage`, terminate the process abruptly, simulating a crash at that precise
/// point. For report/write paths the enclosing transaction or staged-then-
/// atomic-publish pattern guarantees the observable outcome is either the
/// prior verified state or the new complete one — never a half-published state
/// that looks valid.
pub fn failpoint(stage: &str) {
    if std::env::var("SNAG_FAILPOINT").as_deref() == Ok(stage) {
        eprintln!("snag: failpoint abort at {}", stage);
        std::process::abort();
    }
}
