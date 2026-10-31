//! niubash#727 — gate evidence on the production runtime faces
//! themselves: the process-wide `FfiRuntime` singleton behind
//! `ffi::probe_for_status`, `ffi::note_prompt_interrupt`,
//! `ffi::execute_routed`, and the engine-seam adapter
//! `ffi::route_provider` (registered on every session by
//! shell.rs). The provider-seam battery lives in
//! `issue727_ffi_gates.rs`; this file drives the public faces a
//! real session drives, against a synthetic FFI installation: a
//! dummy `winuxcore.dll` (present for discovery, unloadable as a
//! library on every platform) beside a synthetic `tiers.tsv`.
//!
//! Isolation contract: integration test files are separate
//! processes, and this file is the only consumer of the runtime
//! singleton in its process. Its single test sets the switch
//! environment BEFORE the first ffi call (which is what lazily
//! initializes the runtime) and then drives the faces in one
//! fixed order — mirroring a session's lifetime: probe, claim,
//! interrupt, fall back.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use niubash_runtime::ffi::{self, FfiOutcome, FfiRequest, ProviderState, StdHandles};
use rubash::api::{
    ExternalRouteHandle, ExternalRouteOutcome, ExternalRouteRequest, ExternalRouteStdio,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const TIERS: &str = "# winuxcmd-version: 9.9.9-issue727\n\
command\ttier\tfallback_shapes\tnote\n\
cat\ttier2\t-\tsynthetic battery row\n\
grep\ttier2\t-\tsynthetic battery row\n\
head\ttier2\t-\tsynthetic battery row\n\
cp\ttier1\t-\ttier-1 control row\n";

#[test]
fn issue727_runtime_faces_probe_claim_cancel_fallback() {
    // Synthetic installation tree: discovery must resolve it,
    // the load must fail (dummy bytes are not a loadable
    // library anywhere), and every face must stay honest.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("niu-issue727-runtime-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let dll = dir.join("winuxcore.dll");
    std::fs::write(
        &dll,
        b"issue727 dummy: present for discovery, not a library",
    )
    .expect("write dummy dll");
    std::fs::write(dir.join("tiers.tsv"), TIERS).expect("write tiers");

    // The switch environment is read once, when the runtime
    // initializes at the first ffi call below.
    std::env::set_var("NIU_WINUXCMD_FFI", "1");
    std::env::set_var("NIU_WINUXCMD_FFI_DLL", &dll);
    std::env::remove_var("NIU_WINUXCMD_FFI_EXCLUDE");
    std::env::remove_var("NIU_WINUXCMD_FFI_LOG");

    // Probe (the `niu doctor` path): discovery + table resolve,
    // the load fails, and the status face says so — FFI shows as
    // unavailable, never as enabled (gate ④'s honest status).
    let status = ffi::probe_for_status();
    assert!(status.enabled);
    assert_eq!(status.state, ProviderState::Unavailable);
    assert!(
        status.reason.is_some(),
        "the reason is reported, not hidden"
    );
    assert_eq!(status.dll_path.as_deref(), Some(dll.as_path()));
    assert_eq!(status.tier_counts, (4, 3, 1), "the fixture table loaded");
    assert_eq!(status.winuxcmd_version.as_deref(), Some("9.9.9-issue727"));
    assert_eq!(ffi::status().state, ProviderState::Unavailable);

    // The claim face: the introspection annotation answers for a
    // tier-2 name even though execution cannot be served here.
    assert_eq!(
        ffi::route_annotation("grep", &[]),
        Some("winuxcmd FFI (in-process via winuxcore.dll)".to_string())
    );
    assert_eq!(ffi::route_annotation("git", &[]), None);

    let cwd = std::env::current_dir().expect("cwd");
    let routed = |name: &str| FfiRequest {
        name: name.to_string(),
        args: vec!["beta".to_string()],
        env: Vec::new(),
        cwd: cwd.clone(),
        std: StdHandles::default(),
    };

    // Gate ③ on the production funnel: the prompt Ctrl-C writes
    // the runtime's cancel flag (`note_prompt_interrupt` is the
    // exact call the prompt funnel makes); the next routed call
    // observes it and cancels BEFORE any backend is touched.
    ffi::note_prompt_interrupt();
    assert_eq!(
        ffi::execute_routed(&routed("grep"), &|| false),
        FfiOutcome::Cancelled
    );
    // The interrupt is consumed by that verdict — the following
    // call takes the ordinary path, which here is the silent
    // fallback to spawn (the DLL cannot load on this platform).
    // Post-cancel state is clean: no corruption, no stuck flag.
    match ffi::execute_routed(&routed("grep"), &|| false) {
        FfiOutcome::Fallback(reason) => {
            assert!(!reason.is_empty());
        }
        other => panic!("expected Fallback after the cancel, got {other:?}"),
    }
    assert_eq!(
        ffi::status().last_fallback.is_some(),
        true,
        "the fallback is recorded on the status face"
    );

    // The engine seam adapter maps the same verdicts into the
    // engine's outcome type: an engine-side pending interrupt
    // cancels across the seam; otherwise the fallback becomes a
    // plain decline (the engine spawns the prepared command).
    let mut provider = ffi::route_provider();
    let argv = vec!["grep".to_string(), "beta".to_string()];
    let env: Vec<(String, String)> = Vec::new();
    let stdio = ExternalRouteStdio {
        stdin: ExternalRouteHandle::Inherit,
        stdout: ExternalRouteHandle::Inherit,
        stderr: ExternalRouteHandle::Inherit,
    };
    let program = PathBuf::from("/usr/bin/grep");
    let check_on = || true;
    let request = ExternalRouteRequest {
        name: "grep",
        argv: &argv,
        program: &program,
        env: &env,
        cwd: &cwd,
        stdio: &stdio,
        cancel_check: &check_on,
    };
    assert_eq!(
        provider.execute_route(&request),
        ExternalRouteOutcome::Cancelled
    );
    let check_off = || false;
    let request = ExternalRouteRequest {
        cancel_check: &check_off,
        ..request
    };
    assert_eq!(
        provider.execute_route(&request),
        ExternalRouteOutcome::Declined
    );

    let _ = std::fs::remove_dir_all(&dir);
}
