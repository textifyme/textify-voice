//! Live probe for [`voice_context::MacosContextProvider`] — NOT part of
//! `cargo test --workspace` (examples aren't test targets), so this
//! requires no gating to keep that gate green for someone without the
//! macOS Accessibility permission. Run it directly to see real output:
//!
//! ```text
//! cargo run -p voice-context --example probe_macos
//! ```
//!
//! On macOS this calls the real `NSWorkspace`/`AXUIElement` path and prints
//! the frontmost app, the focused element's role/subrole/writable/secure,
//! and a timing proof that `capture()` returned before the background read
//! could have completed. On every other target it just says so and exits —
//! the crate's macOS backend is `#[cfg(target_os = "macos")]`-gated, so
//! there is nothing live to probe elsewhere.

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};
    use voice_context::{ContextProvider, Coverage, MacosContextProvider};

    let provider = MacosContextProvider::with_timeout(Duration::from_millis(500));

    println!("=== capture() timing (must return near-instantly, never block) ===");
    let start = Instant::now();
    let capture = provider.capture();
    let elapsed = start.elapsed();
    println!("capture() returned in {elapsed:?}");
    println!("first snapshot.seq = {} (0 == the 'not yet captured' placeholder)", capture.snapshot.seq);

    println!("\n=== waiting for the background NSWorkspace/AXUIElement read to resolve ===");
    let Some(pending) = capture.pending else {
        println!("no pending read — unexpected for a real provider");
        return;
    };
    let resolved = match pending.wait() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            println!("background read channel disconnected: {err}");
            return;
        }
    };

    println!("\n=== resolved snapshot ===");
    match &resolved.frontmost_app {
        Some(app) => println!("frontmost_app: name={:?} kind={:?}", app.name, app.kind),
        None => println!("frontmost_app: None"),
    }
    match &resolved.focused_element {
        Some(el) => {
            println!("focused_element:");
            println!("  role      = {:?}", el.role);
            println!("  label     = {:?}", el.label);
            println!("  writable  = {}", el.writable);
            println!("  secure    = {}", el.secure);
            println!("  enabled   = {}", el.enabled);
            println!("  position  = {:?}", el.position);
        }
        None => println!("focused_element: None"),
    }
    match resolved.actionable_map.coverage() {
        Coverage::Full => println!("actionable_map.coverage = Full ({} elements)", resolved.actionable_map.len()),
        Coverage::Partial { reason } => println!("actionable_map.coverage = Partial({reason:?}), {} elements", resolved.actionable_map.len()),
        Coverage::Unavailable { reason } => println!("actionable_map.coverage = Unavailable({reason:?})"),
    }

    println!("\n=== second capture() — its 'previous' should be the snapshot that just resolved ===");
    let capture2 = provider.capture();
    println!("second snapshot.seq = {} (should equal the resolved seq above)", capture2.snapshot.seq);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("voice-context's macOS backend is target_os = \"macos\"-gated; nothing to probe on this platform.");
}
