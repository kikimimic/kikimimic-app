//! Spawning a real process lives in its own test binary: while a child is
//! between fork and exec it holds copies of the parent's file descriptors,
//! including flock()ed ones, which made unrelated lock tests in the library
//! binary fail at random (annotations' `.annotations.lock`).

#[cfg(unix)]
#[test]
fn running_pipes_the_prompt_through_and_reports_failures() {
    use kikimimic_core::agent::run_program;
    assert_eq!(run_program("cat", &[], "hello").unwrap(), "hello");
    assert!(run_program("sh", &["-c", "echo nope >&2; exit 3"], "")
        .unwrap_err()
        .to_string()
        .contains("nope"));
    assert!(run_program("kkm-no-such-agent", &[], "").is_err());
}
