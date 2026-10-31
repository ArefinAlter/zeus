// Native Windows regression: own console versus no console, with redirected standard IO.
// Included only by cfg(windows) test modules. It never changes the runner's console or decides a request.
pub fn assert_console_gate(test_name: &str, probe: impl FnOnce() -> bool) {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const MODE: &str = "ZEUS_TEST_CONTROLLING_CONSOLE_MODE";
    if let Ok(mode) = std::env::var(MODE) {
        assert!(matches!(mode.as_str(), "attached" | "detached"));
        assert_eq!(
            probe(),
            mode == "attached",
            "controlling-console gate: {mode}"
        );
        return;
    }
    for (mode, flags) in [("attached", 0x0000_0010), ("detached", 0x0000_0008)] {
        // CREATE_NEW_CONSOLE / DETACHED_PROCESS: neither inherits a user's terminal.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(MODE, mode)
            .creation_flags(flags)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("controlling-console child exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
