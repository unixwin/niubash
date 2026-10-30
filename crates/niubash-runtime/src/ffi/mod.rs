//! WinuxCmd FFI host wiring (niubash#567, P4): the in-process route
//! provider for `winuxcore.dll` (ABI v2).
//!
//! PATH integration stays the default, the fallback, and the
//! equivalence reference (AGENTS.md, 2026-10-27 ruling). This module
//! is the opt-in acceleration surface beside it:
//!
//! - [`config`] — the `NIU_WINUXCMD_FFI*` switches. The master switch
//!   defaults OFF in this delivery: the #567 zero-problem gate items
//!   ① (round-2 differential) and ③ (long-task Ctrl-C proof) are
//!   still open, so [`config::DEFAULT_ENABLED`] stays `false` and
//!   flipping the default is that one constant once the gate passes.
//! - [`discovery`] — DLL candidate resolution. Absolute paths only:
//!   the `NIU_WINUXCMD_FFI_DLL` override, else the
//!   `<root>/usr/bin/ffi/current.txt` pointer under the installation
//!   root niu's existing winuxcmd discovery selected. Never the CWD,
//!   never a PATH search.
//! - [`tiers`] — the generated `tiers.tsv` shipped beside the DLL
//!   (WinuxCmd build-time generation from the audited manifest,
//!   DESIGN §6 R1–R7). The host never hand-maintains a name list.
//! - [`route`] — the route classifier. Builtins/keywords dispatch
//!   before any external route exists; the registry identity rows
//!   (`uname`/`arch`/`hostname`) and the host terminal row (`stty`)
//!   are not FFI names; tier-1 names stay on the emulation route
//!   until their own differential gate passes (#567 gate note ⑤).
//! - [`context`] — the ABI v2 context builder: argv wide strings
//!   byte-for-byte as the executor resolved them, the complete env
//!   as one wide double-NUL block (prefix assignments included), an
//!   absolute non-NULL cwd, and the host's already-resolved stdio
//!   handles borrowed raw. No capture layer, no copy-back.
//! - [`backend`] — the injectable backend seam. The production
//!   backend loads the DLL once per process and never unloads it;
//!   tests inject a mock and assert the context field by field.
//! - [`provider`] — route decision + execution over a backend,
//!   serialized by the process-wide FFI lock (DESIGN §5), with a
//!   real cancellation callback driven by niu's interrupt state.
//!
//! Failure policy (DESIGN §2.3): every FFI-layer failure — DLL
//! missing, ABI out of the known range, init failure, any result
//! code <= -1000 — declines silently to the spawn form. The status
//! face ([`status`] / `niu doctor`) reports the state honestly: a
//! fallen-back FFI shows as unavailable, never as enabled.
//!
//! Trust red line (user ruling, 2026-10-28): this module performs
//! NO load-time hash check, NO signature check, and NO drift or
//! integrity re-verification of the DLL. The only gate before a
//! call is the functional ABI-version compatibility check. (The
//! ffi-route DESIGN §3.2 item 4 / §4.3 load-time SHA-256 re-check
//! is deliberately NOT implemented; see the P4 report.)

pub mod backend;
pub mod config;
pub mod context;
pub mod discovery;
pub mod provider;
pub mod route;
pub mod tiers;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub use config::FfiConfig;
pub use context::{FfiRequest, StdHandles};
pub use provider::{FfiOutcome, FfiProvider, FfiStatus, ProviderState};
pub use route::{Route, RouteDecision};

/// The production backend on this platform: the real DLL loader on
/// Windows, the permanently-unavailable backend elsewhere (route
/// decisions and status stay computable; execution always declines
/// to spawn, which is the only correct answer off Windows).
#[cfg(windows)]
pub type PlatformBackend = backend::WindowsDllBackend;
#[cfg(not(windows))]
pub type PlatformBackend = backend::UnavailableBackend;

/// Process-wide FFI runtime (DESIGN §5: the route table is a
/// process singleton — the first session's resolved DLL version is
/// the process's version; no session loads a second one).
pub struct FfiRuntime {
    provider: Mutex<FfiProvider<PlatformBackend>>,
    cancel: Arc<AtomicBool>,
}

fn runtime() -> &'static FfiRuntime {
    static RUNTIME: OnceLock<FfiRuntime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let cancel = Arc::new(AtomicBool::new(false));
        FfiRuntime {
            provider: Mutex::new(FfiProvider::new(FfiConfig::from_env(), cancel.clone())),
            cancel,
        }
    })
}

/// Record a user interrupt (prompt Ctrl+C funnel,
/// `ctrl_c::queue_prompt_sigint`). An FFI call in flight observes
/// this through its `cancel_check` callback; a call that starts
/// with the flag already set declines as cancelled instead of
/// running (the upstream exit status for that shape is 130, owned
/// by the dispatch layer).
pub fn note_prompt_interrupt() {
    runtime().cancel.store(true, Ordering::SeqCst);
}

/// The shared cancellation flag the provider's `cancel_check`
/// callback reads. Exposed for the dispatch seam that assembles
/// calls (P3) so its context carries the same flag.
pub fn cancel_flag() -> Arc<AtomicBool> {
    runtime().cancel.clone()
}

/// Resolve discovery (and the tier table) ahead of first use, but
/// only when the master switch is on: with the switch off this is a
/// no-op, so session startup pays nothing for FFI (the non-interactive
/// fast path included). Called from the shell's session build.
pub fn warm_up() {
    let mut provider = runtime().provider.lock().expect("ffi provider lock");
    if provider.config().enabled {
        provider.probe();
    }
}

/// The current FFI status snapshot for status faces (`niu doctor`).
/// Does not force discovery or a load by itself; call
/// [`probe_for_status`] when the face wants the resolved picture.
pub fn status() -> FfiStatus {
    runtime()
        .provider
        .lock()
        .expect("ffi provider lock")
        .status()
}

/// Probe discovery + (when the switch is on) the DLL load and ABI
/// check, then snapshot. This is the `niu doctor` path: the doctor
/// process is short-lived, so loading once there is harmless, and
/// the report must distinguish "not installed" from "loadable".
pub fn probe_for_status() -> FfiStatus {
    let mut provider = runtime().provider.lock().expect("ffi provider lock");
    provider.probe_and_load();
    provider.status()
}

/// The `type` / `command -V` annotation for a name the FFI route
/// claims, e.g. `winuxcmd FFI (in-process via winuxcore.dll)`.
/// `None` for every name FFI does not claim. Pure route query: no
/// load is forced.
pub fn route_annotation(name: &str, args: &[String]) -> Option<String> {
    let mut provider = runtime().provider.lock().expect("ffi provider lock");
    let decision = provider.route(name, args);
    if decision.route == Route::Ffi {
        Some(route::FFI_ANNOTATION.to_string())
    } else {
        None
    }
}

/// niubash#567 route registration (the engine seam, P3): the
/// `rubash::api::ExternalRouteProvider` face over this runtime.
/// The engine dispatcher consults it for a resolved foreground
/// external whose spawn is fully prepared; the provider here
/// re-derives its route decision from the name + argv (its
/// classifier consumes tiers.tsv host-side, as designed) and
/// either runs the call through the DLL backend over the
/// request's live handles or declines to the prepared spawn.
pub struct FfiRouteProvider;

/// The boxed provider the session builder registers
/// (`SessionBuilder::external_route_provider`). Registration is
/// unconditional and inert: while the provider's own switches
/// are off it declines every call before touching the backend,
/// so a session with the switch off behaves byte-identically
/// to one with no registration at all.
pub fn route_provider() -> Box<dyn rubash::api::ExternalRouteProvider> {
    Box::new(FfiRouteProvider)
}

impl rubash::api::ExternalRouteProvider for FfiRouteProvider {
    fn execute_route(
        &mut self,
        request: &rubash::api::ExternalRouteRequest,
    ) -> rubash::api::ExternalRouteOutcome {
        let ffi_request = ffi_request_from_route(request);
        route_outcome_from_ffi(execute_routed(&ffi_request, request.cancel_check))
    }

    fn route_annotation(&self, name: &str) -> Option<String> {
        // The seam's annotation query is name-only (it backs
        // `type` / `command -V`, which ask about a name, not an
        // invocation); the classifier's argv-sensitive rows are
        // invocation refinements, not name claims.
        route_annotation(name, &[])
    }
}

/// Run one routed call through the process provider. The
/// engine's pending-interrupt check joins the provider's own
/// prompt flag as a cancellation source (see
/// [`FfiProvider::execute_with_engine_cancel`]).
pub fn execute_routed(request: &FfiRequest, engine_cancel: &dyn Fn() -> bool) -> FfiOutcome {
    let mut provider = runtime().provider.lock().expect("ffi provider lock");
    // `emulation_available` is pinned true: the flag only selects
    // WHICH decline a tier-1 name receives (the emulation route
    // vs the tier-1 gate-closed route) — both decline across this
    // seam, so the choice cannot change whether a call is
    // claimed, only the recorded reason.
    provider.execute_with_engine_cancel(request, true, engine_cancel)
}

/// The seam request in the provider's own request type: the
/// command word comes off the argv head (the provider's request
/// keeps name and args apart), the environment pairs carry
/// over verbatim, and each stdio handle maps through
/// [`rubash::api::ExternalRouteHandle::raw_or_zero`] — inherit
/// becomes the ABI's NULL ("keep the current handle").
fn ffi_request_from_route(request: &rubash::api::ExternalRouteRequest) -> FfiRequest {
    FfiRequest {
        name: request.name.to_string(),
        args: request.argv.get(1..).unwrap_or(&[]).to_vec(),
        env: request.env.to_vec(),
        cwd: request.cwd.to_path_buf(),
        std: StdHandles {
            stdin: request.stdio.stdin.raw_or_zero(),
            stdout: request.stdio.stdout.raw_or_zero(),
            stderr: request.stdio.stderr.raw_or_zero(),
        },
    }
}

/// The provider's outcome in the seam's outcome type. A provider
/// fallback is already recorded in the provider's own status
/// (`last_fallback`); across the seam it is simply a decline —
/// the engine spawns the prepared command.
fn route_outcome_from_ffi(outcome: FfiOutcome) -> rubash::api::ExternalRouteOutcome {
    match outcome {
        FfiOutcome::Completed(status) => rubash::api::ExternalRouteOutcome::Completed(status),
        FfiOutcome::Cancelled => rubash::api::ExternalRouteOutcome::Cancelled,
        FfiOutcome::Declined(_) | FfiOutcome::Fallback(_) => {
            rubash::api::ExternalRouteOutcome::Declined
        }
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use rubash::api::{ExternalRouteHandle, ExternalRouteStdio};
    use std::path::Path;

    fn route_request() -> (
        Vec<String>,
        Vec<(String, String)>,
        std::path::PathBuf,
        ExternalRouteStdio,
    ) {
        (
            vec!["grep".to_string(), "-n".to_string(), "a b".to_string()],
            vec![("PREFIXED".to_string(), "1".to_string())],
            std::path::PathBuf::from("/work"),
            ExternalRouteStdio {
                stdin: ExternalRouteHandle::Handle(5),
                stdout: ExternalRouteHandle::Inherit,
                stderr: ExternalRouteHandle::Handle(7),
            },
        )
    }

    #[test]
    fn route_request_maps_to_ffi_request_verbatim() {
        let (argv, env, cwd, stdio) = route_request();
        let request = rubash::api::ExternalRouteRequest {
            name: "grep",
            argv: &argv,
            program: Path::new("/bin/grep"),
            env: &env,
            cwd: &cwd,
            stdio: &stdio,
            cancel_check: &|| false,
        };
        let mapped = ffi_request_from_route(&request);
        assert_eq!(mapped.name, "grep");
        assert_eq!(mapped.args, vec!["-n".to_string(), "a b".to_string()]);
        assert_eq!(mapped.env, env);
        assert_eq!(mapped.cwd, cwd);
        assert_eq!(mapped.std.stdin, 5);
        assert_eq!(mapped.std.stdout, 0);
        assert_eq!(mapped.std.stderr, 7);
    }

    #[test]
    fn ffi_outcomes_map_to_route_outcomes() {
        use rubash::api::ExternalRouteOutcome as Route;
        assert_eq!(
            route_outcome_from_ffi(FfiOutcome::Completed(42)),
            Route::Completed(42)
        );
        assert_eq!(
            route_outcome_from_ffi(FfiOutcome::Cancelled),
            Route::Cancelled
        );
        let decision = RouteDecision {
            route: route::Route::Spawn,
            reason: route::RouteReason::SwitchOff,
        };
        assert_eq!(
            route_outcome_from_ffi(FfiOutcome::Declined(decision)),
            Route::Declined
        );
        assert_eq!(
            route_outcome_from_ffi(FfiOutcome::Fallback("boom".to_string())),
            Route::Declined
        );
    }
}
