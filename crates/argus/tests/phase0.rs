//! End-to-end Phase 0 smoke test: run the real multi-process binary and confirm
//! it brings up a content process + net service, moves a framebuffer across
//! shared memory, and survives a content-process crash.

use std::process::Command;

#[test]
fn phase0_end_to_end() {
    let exe = env!("CARGO_BIN_EXE_argus");
    // `--headless` runs the verifier and exits (no window) so this is CI-safe.
    let out = Command::new(exe)
        .arg("--headless")
        .output()
        .expect("spawn argus binary");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "argus exited unsuccessfully: {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("PHASE0 OK"),
        "missing success marker\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
