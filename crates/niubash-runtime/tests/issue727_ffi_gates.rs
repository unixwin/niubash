//! niubash#727 — the FFI default-flip gate evidence battery for
//! gate ① (product-path expansion) and gate ③ (Ctrl-C proof),
//! on the surface this platform can actually execute: the product
//! provider seam (`FfiProvider` over an injected backend — the
//! same policy core the production runtime drives), plus the
//! fallback / long-stability cells the gate enumerates.
//!
//! What this file proves:
//! - Byte equivalence at the seam: a scripted backend that
//!   implements real applet semantics (cat/head/grep/printenv
//!   shapes) over the ABI context's borrowed handles produces
//!   byte-identical stdout/stderr and the identical exit code as
//!   the spawn arm (the platform coreutils on the same argv, env,
//!   cwd, and handles) across plain, redirected, and
//!   pipeline-composed shapes. With the switch off the provider
//!   declines every one of those calls, so the spawn arm *is*
//!   the off arm by construction.
//! - Fallback robustness (gate ① fallback cells + gate ④'s
//!   silence): load failure, ABI below/above the known version,
//!   ABI query error, init failure, FFI-layer call errors, and
//!   context-build failure all decline/fall back silently — the
//!   command still runs the ordinary way, the status face reports
//!   `Unavailable` honestly, and a poisoned backend is never
//!   retried inside the process.
//! - Long stability: 10,000 sequential calls with no drift, and
//!   concurrent callers serialized by the provider's execution
//!   lock exactly as the process runtime shares it.
//! - Ctrl-C (gate ③): a long in-flight call that polls the ABI
//!   cancel callback (the cooperative contract) aborts promptly
//!   when the host's interrupt flag lands — from the prompt flag
//!   or from the engine's cancel check — the provider's state is
//!   not corrupted, the cancellation is not recorded as a
//!   fallback, and the next call runs normally.
//!
//! What this file does NOT prove (stated plainly, per the gate):
//! the real `winuxcore.dll` executing under the Windows console —
//! the production backend is Windows-only, so the PE DLL itself,
//! real console Ctrl-C delivery (0xc000013a shape), and
//! LoadLibrary bitness errors remain Windows-real-machine cells
//! (see the #727 cluster report). The scripted backends here
//! implement the DLL side of the ABI contract (poll the callback,
//! return `WINUX_FFI_ERR_CANCELLED`) so everything on the host
//! side of that contract is exercised for real.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use niubash_runtime::ffi::backend::{
    CancelFn, FfiBackend, InitRequest, ERR_CANCELLED, FFI_ABI_VERSION,
};
use niubash_runtime::ffi::config::FfiConfig;
use niubash_runtime::ffi::context::{FfiRequest, StdHandles, WideContext};
use niubash_runtime::ffi::discovery::DllCandidate;
use niubash_runtime::ffi::provider::{FfiOutcome, FfiProvider, ProviderState};
use niubash_runtime::ffi::tiers::TierTable;

/// Synthetic tier table in the generated shape: three real
/// coreutils names the byte battery drives, two test-only applets
/// (envprobe/failcode/blocker), and a tier-1 control row.
const TABLE: &str = "# winuxcmd-version: 9.9.9-issue727\n\
command\ttier\tfallback_shapes\tnote\n\
cat\ttier2\t-\tsynthetic battery row\n\
grep\ttier2\t--interactive\tsynthetic battery row with a fallback shape\n\
head\ttier2\t-\tsynthetic battery row\n\
envprobe\ttier2\t-\ttest-only applet (printenv semantics)\n\
failcode\ttier2\t-\ttest-only applet (exit-code passthrough)\n\
blocker\ttier2\t-\ttest-only applet (long task)\n\
cp\ttier1\t-\ttier-1 control row\n";

fn on() -> FfiConfig {
    FfiConfig::from_pairs([("NIU_WINUXCMD_FFI", "1")])
}

fn off() -> FfiConfig {
    FfiConfig::from_pairs(Vec::<(String, String)>::new())
}

fn candidate() -> DllCandidate {
    DllCandidate {
        dll_path: PathBuf::from("winuxcore.dll"),
        tiers_path: PathBuf::from("tiers.tsv"),
        version_dir: "v9".to_string(),
        install_root: PathBuf::from(if cfg!(windows) { "C:\\root" } else { "/root" }),
    }
}

fn table() -> TierTable {
    TierTable::parse(TABLE).expect("synthetic table parses")
}

fn request(name: &str, args: &[&str], cwd: PathBuf) -> FfiRequest {
    FfiRequest {
        name: name.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("LC_ALL".to_string(), "C".to_string()),
        ],
        cwd,
        std: StdHandles::default(),
    }
}

fn abs_cwd() -> PathBuf {
    PathBuf::from(if cfg!(windows) { "C:\\work" } else { "/work" })
}

// ---------------------------------------------------------------------
// A scripted backend: configurable handshake/call behavior with a
// shared, inspectable record. Stands in for the DLL side of the
// ABI contract.
// ---------------------------------------------------------------------

#[derive(Clone)]
enum CallScript {
    /// Return this result from every call.
    Fixed(Result<i32, String>),
    /// Return codes by call index (wraps).
    Cycle(Vec<i32>),
    /// A long task: poll the cancel callback every `poll`; when
    /// it reports set (and `respect_cancel`), return
    /// ERR_CANCELLED at that checkpoint; otherwise run out the
    /// `budget` and return `then`. With `respect_cancel == false`
    /// the callback is never polled — the non-cooperative shape.
    Block {
        budget: Duration,
        poll: Duration,
        then: i32,
        respect_cancel: bool,
    },
    /// Set the given flag in the middle of the call (an interrupt
    /// arriving while a non-polling applet runs), then return.
    SetFlagThen(Arc<AtomicBool>, i32),
}

#[derive(Default)]
struct Rec {
    script: Option<CallScript>,
    abi: Option<Result<u32, String>>,
    init: Option<Result<i32, String>>,
    abi_calls: usize,
    init_calls: usize,
    calls: usize,
    in_call_now: usize,
    max_in_call: usize,
    cancel_seen: bool,
    call_sleep: Duration,
}

struct ScriptedBackend {
    rec: Arc<Mutex<Rec>>,
}

impl ScriptedBackend {
    fn new() -> (Self, Arc<Mutex<Rec>>) {
        let rec = Arc::new(Mutex::new(Rec::default()));
        (ScriptedBackend { rec: rec.clone() }, rec)
    }
}

impl FfiBackend for ScriptedBackend {
    fn abi_version(&mut self) -> Result<u32, String> {
        let mut rec = self.rec.lock().unwrap();
        rec.abi_calls += 1;
        rec.abi.clone().unwrap_or(Ok(FFI_ABI_VERSION))
    }

    fn init(&mut self, _args: &InitRequest) -> Result<i32, String> {
        let mut rec = self.rec.lock().unwrap();
        rec.init_calls += 1;
        rec.init.clone().unwrap_or(Ok(0))
    }

    fn call(
        &mut self,
        _ctx: &WideContext,
        cancel: CancelFn,
        cancel_user_data: *mut c_void,
    ) -> Result<i32, String> {
        let (script, sleep) = {
            let mut rec = self.rec.lock().unwrap();
            rec.calls += 1;
            rec.in_call_now += 1;
            rec.max_in_call = rec.max_in_call.max(rec.in_call_now);
            (
                rec.script.clone().unwrap_or(CallScript::Fixed(Ok(0))),
                rec.call_sleep,
            )
        };
        if !sleep.is_zero() {
            std::thread::sleep(sleep);
        }
        let result = match script {
            CallScript::Fixed(result) => result,
            CallScript::Cycle(codes) => {
                let index = self.rec.lock().unwrap().calls - 1;
                Ok(codes[index % codes.len()])
            }
            CallScript::Block {
                budget,
                poll,
                then,
                respect_cancel,
            } => {
                let start = Instant::now();
                loop {
                    if respect_cancel {
                        // SAFETY: `cancel`/`cancel_user_data` are
                        // the provider's trampoline + sources for
                        // this call, live for its duration — the
                        // same contract the DLL runs under.
                        let flagged = unsafe { cancel(cancel_user_data) } != 0;
                        if flagged {
                            self.rec.lock().unwrap().cancel_seen = true;
                            break Ok(ERR_CANCELLED);
                        }
                    }
                    if start.elapsed() >= budget {
                        break Ok(then);
                    }
                    std::thread::sleep(poll);
                }
            }
            CallScript::SetFlagThen(flag, rc) => {
                flag.store(true, Ordering::SeqCst);
                Ok(rc)
            }
        };
        self.rec.lock().unwrap().in_call_now -= 1;
        result
    }
}

fn scripted_provider(config: FfiConfig, backend: ScriptedBackend) -> FfiProvider<ScriptedBackend> {
    FfiProvider::with_parts(
        config,
        Arc::new(AtomicBool::new(false)),
        Some(candidate()),
        Some(table()),
        Some(backend),
    )
}

fn grep_like_request() -> FfiRequest {
    request("grep", &["beta"], abs_cwd())
}

// ---------------------------------------------------------------------
// Gate ① fallback robustness + gate ④ silence (provider level).
// Every failure lands as a silent Declined/Fallback; the status
// face carries the reason.
// ---------------------------------------------------------------------

#[test]
fn issue727_backend_load_failure_falls_back_and_stays_unavailable() {
    // `with_parts` with no backend exercises the provider's
    // backend-factory error path (the factory is the in-repo
    // "no factory" error — the same outcome shape a LoadLibrary
    // failure produces through the production factory).
    let mut provider = FfiProvider::<ScriptedBackend>::with_parts(
        on(),
        Arc::new(AtomicBool::new(false)),
        Some(candidate()),
        Some(table()),
        None,
    );
    let first = provider.execute(&grep_like_request(), true);
    assert!(
        matches!(first, FfiOutcome::Fallback(_)),
        "load failure must fall back, got {first:?}"
    );
    let status = provider.status();
    assert_eq!(status.state, ProviderState::Unavailable);
    assert!(status.reason.is_some());
    // A second call takes the same recorded path — no crash, no
    // half-initialized state, the command still just runs.
    let second = provider.execute(&grep_like_request(), true);
    assert!(matches!(second, FfiOutcome::Fallback(_)));
    assert_eq!(provider.status().state, ProviderState::Unavailable);
    assert!(provider.status().last_fallback.is_some());
}

#[test]
fn issue727_abi_below_and_above_the_known_version_fall_back() {
    for abi in [FFI_ABI_VERSION - 1, FFI_ABI_VERSION + 1, 99] {
        let (backend, rec) = ScriptedBackend::new();
        rec.lock().unwrap().abi = Some(Ok(abi));
        let mut provider = scripted_provider(on(), backend);
        match provider.execute(&grep_like_request(), true) {
            FfiOutcome::Fallback(reason) => {
                assert!(
                    reason.contains(&format!("ABI version {abi}")),
                    "reason should name the version: {reason}"
                );
            }
            other => panic!("abi {abi}: expected Fallback, got {other:?}"),
        }
        let rec = rec.lock().unwrap();
        assert_eq!(rec.calls, 0, "no call may run under a bad ABI");
        assert_eq!(rec.init_calls, 0, "init runs only after the version check");
        drop(rec);
        let status = provider.status();
        assert_eq!(status.state, ProviderState::Unavailable);
        assert!(status
            .reason
            .unwrap()
            .contains(&format!("ABI version {abi}")));
    }
}

#[test]
fn issue727_abi_query_error_falls_back() {
    let (backend, rec) = ScriptedBackend::new();
    rec.lock().unwrap().abi = Some(Err("cannot ask".to_string()));
    let mut provider = scripted_provider(on(), backend);
    assert!(matches!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Fallback(_)
    ));
    assert_eq!(rec.lock().unwrap().calls, 0);
    assert_eq!(provider.status().state, ProviderState::Unavailable);
}

#[test]
fn issue727_init_failure_and_init_error_fall_back_and_poison() {
    for init in [Ok(-1001), Err("init boom".to_string())] {
        let (backend, rec) = ScriptedBackend::new();
        rec.lock().unwrap().init = Some(init);
        let mut provider = scripted_provider(on(), backend);
        assert!(matches!(
            provider.execute(&grep_like_request(), true),
            FfiOutcome::Fallback(_)
        ));
        assert!(matches!(
            provider.execute(&grep_like_request(), true),
            FfiOutcome::Fallback(_)
        ));
        let rec = rec.lock().unwrap();
        assert_eq!(rec.calls, 0);
        // The handshake failure poisons the backend for the
        // process: the second call does not re-run the handshake.
        assert_eq!(rec.abi_calls, 1, "poisoned backend must not be retried");
        assert_eq!(rec.init_calls, 1);
        drop(rec);
        assert_eq!(provider.status().state, ProviderState::Unavailable);
    }
}

#[test]
fn issue727_call_layer_errors_fall_back_but_cancelled_is_not_a_fallback() {
    // FFI-layer codes (<= -1000) and stray negative codes fall
    // back; the ERR_CANCELLED code is a cancellation verdict,
    // never a fallback — the dispatch layer must not spawn a
    // command the user interrupted.
    for rc in [-1000, -1004, -1, -999] {
        let (backend, _rec) = ScriptedBackend::new();
        _rec.lock().unwrap().script = Some(CallScript::Fixed(Ok(rc)));
        let mut provider = scripted_provider(on(), backend);
        match provider.execute(&grep_like_request(), true) {
            FfiOutcome::Fallback(reason) => {
                assert!(reason.contains(&rc.to_string()), "{reason}")
            }
            other => panic!("rc {rc}: expected Fallback, got {other:?}"),
        }
        assert!(provider.status().last_fallback.is_some());
    }
    let (backend, _rec) = ScriptedBackend::new();
    _rec.lock().unwrap().script = Some(CallScript::Fixed(Err("transport died".to_string())));
    let mut provider = scripted_provider(on(), backend);
    assert!(matches!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Fallback(_)
    ));

    let (backend, _rec) = ScriptedBackend::new();
    _rec.lock().unwrap().script = Some(CallScript::Fixed(Ok(ERR_CANCELLED)));
    let mut provider = scripted_provider(on(), backend);
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Cancelled
    );
    assert_eq!(
        provider.status().last_fallback,
        None,
        "a cancellation is not a fallback and must not be recorded as one"
    );
}

#[test]
fn issue727_context_build_failure_falls_back_without_touching_backend() {
    let (backend, rec) = ScriptedBackend::new();
    let mut provider = scripted_provider(on(), backend);
    // A relative cwd is an ABI contract violation: the context
    // build refuses it (never a silent NULL) and the call falls
    // back before the backend is touched.
    let mut req = grep_like_request();
    req.cwd = PathBuf::from("relative/dir");
    assert!(matches!(
        provider.execute(&req, true),
        FfiOutcome::Fallback(reason) if reason.contains("not absolute")
    ));
    assert_eq!(rec.lock().unwrap().calls, 0);
}

#[test]
fn issue727_no_candidate_or_no_table_declines_silently() {
    // Discovery produced nothing (no install root, missing
    // pointer, missing DLL — the discovery unit tests pin each
    // variant): with no tier table the route cannot claim, and
    // execution declines without touching any backend.
    let (backend, rec) = ScriptedBackend::new();
    let mut provider = FfiProvider::with_parts(
        on(),
        Arc::new(AtomicBool::new(false)),
        None,
        None,
        Some(backend),
    );
    match provider.execute(&grep_like_request(), true) {
        FfiOutcome::Declined(decision) => {
            assert_eq!(decision.route, niubash_runtime::ffi::Route::Spawn);
        }
        other => panic!("expected Declined, got {other:?}"),
    }
    assert_eq!(rec.lock().unwrap().calls, 0);
    assert_eq!(rec.lock().unwrap().abi_calls, 0);
}

#[test]
fn issue727_switch_off_declines_and_never_loads() {
    let (backend, rec) = ScriptedBackend::new();
    rec.lock().unwrap().script = Some(CallScript::Fixed(Ok(0)));
    let mut provider = scripted_provider(off(), backend);
    assert!(matches!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Declined(_)
    ));
    let rec = rec.lock().unwrap();
    assert_eq!(rec.calls, 0);
    assert_eq!(rec.abi_calls, 0, "off means the DLL is never even asked");
    drop(rec);
    assert_eq!(provider.status().state, ProviderState::Disabled);
}

// ---------------------------------------------------------------------
// Gate ① long stability: many calls, concurrent callers, repeated
// load cycles — no drift, no leaked state.
// ---------------------------------------------------------------------

#[test]
fn issue727_ten_thousand_calls_no_drift() {
    let (backend, rec) = ScriptedBackend::new();
    rec.lock().unwrap().script = Some(CallScript::Cycle(vec![0, 1, 2]));
    let mut provider = scripted_provider(on(), backend);
    for i in 0..10_000 {
        let outcome = provider.execute(&grep_like_request(), true);
        assert_eq!(outcome, FfiOutcome::Completed(i % 3), "call {i} drifted");
    }
    let rec = rec.lock().unwrap();
    assert_eq!(rec.calls, 10_000);
    assert_eq!(rec.abi_calls, 1, "one handshake for the process");
    assert_eq!(rec.init_calls, 1);
    assert_eq!(rec.max_in_call, 1);
    drop(rec);
    let status = provider.status();
    assert_eq!(status.state, ProviderState::Ready);
    assert_eq!(status.last_fallback, None);
    assert_eq!(status.loaded_abi, Some(FFI_ABI_VERSION));
}

#[test]
fn issue727_concurrent_callers_serialize_cleanly() {
    // The production runtime shares one provider behind a mutex
    // (FfiRuntime); the provider's own execution lock must still
    // serialize the borrowed window so at most one call is inside
    // the backend at any moment.
    let (backend, rec) = ScriptedBackend::new();
    {
        let mut guard = rec.lock().unwrap();
        guard.script = Some(CallScript::Fixed(Ok(3)));
        guard.call_sleep = Duration::from_millis(1);
    }
    let provider = Arc::new(Mutex::new(scripted_provider(on(), backend)));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let provider = provider.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..250 {
                let outcome = provider.lock().unwrap().execute(&grep_like_request(), true);
                assert_eq!(outcome, FfiOutcome::Completed(3));
            }
        }));
    }
    for thread in threads {
        thread.join().expect("caller thread panicked");
    }
    let rec = rec.lock().unwrap();
    assert_eq!(rec.calls, 8 * 250);
    assert_eq!(
        rec.max_in_call, 1,
        "the execution lock must serialize the borrowed window"
    );
    drop(rec);
    assert_eq!(provider.lock().unwrap().status().last_fallback, None);
}

#[test]
fn issue727_repeated_load_cycles_each_handshake_once() {
    // The DLL is never unloaded inside a process (the next
    // process is the retry), but each fresh provider must run
    // its own complete handshake exactly once and serve cleanly —
    // the repeated-load leg of the long-stability cell.
    for _ in 0..64 {
        let (backend, rec) = ScriptedBackend::new();
        rec.lock().unwrap().script = Some(CallScript::Fixed(Ok(11)));
        let mut provider = scripted_provider(on(), backend);
        assert_eq!(
            provider.execute(&grep_like_request(), true),
            FfiOutcome::Completed(11)
        );
        let rec = rec.lock().unwrap();
        assert_eq!(rec.abi_calls, 1);
        assert_eq!(rec.init_calls, 1);
        assert_eq!(rec.calls, 1);
    }
}

// ---------------------------------------------------------------------
// Gate ③ Ctrl-C: the product-path cancellation proof at the seam.
// The provider's cancel flag is the same Arc the runtime's
// `note_prompt_interrupt` writes (FfiRuntime hands it to the
// provider at construction); the engine check is the dispatcher's
// pending-interrupt peek the route seam supplies.
// ---------------------------------------------------------------------

fn blocker_provider(
    cancel: Arc<AtomicBool>,
    script: CallScript,
) -> (FfiProvider<ScriptedBackend>, Arc<Mutex<Rec>>) {
    let (backend, rec) = ScriptedBackend::new();
    rec.lock().unwrap().script = Some(script);
    let provider = FfiProvider::with_parts(
        on(),
        cancel,
        Some(candidate()),
        Some(table()),
        Some(backend),
    );
    (provider, rec)
}

#[test]
fn issue727_cancel_during_long_call_aborts_promptly_and_state_stays_clean() {
    let cancel = Arc::new(AtomicBool::new(false));
    let (provider, rec) = blocker_provider(
        cancel.clone(),
        CallScript::Block {
            budget: Duration::from_secs(8),
            poll: Duration::from_millis(1),
            then: 0,
            respect_cancel: true,
        },
    );
    let provider = Arc::new(Mutex::new(provider));
    let runner = {
        let provider = provider.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let outcome = provider
                .lock()
                .unwrap()
                .execute(&request("blocker", &[], abs_cwd()), true);
            (outcome, started.elapsed())
        })
    };
    // The user's Ctrl-C lands while the call is in flight: this
    // store is exactly what `ffi::note_prompt_interrupt` does.
    std::thread::sleep(Duration::from_millis(150));
    cancel.store(true, Ordering::SeqCst);
    let (outcome, elapsed) = runner.join().expect("runner panicked");
    assert_eq!(outcome, FfiOutcome::Cancelled);
    assert!(
        elapsed < Duration::from_secs(4),
        "the host must respond promptly, not after the 8s task: {elapsed:?}"
    );
    assert!(rec.lock().unwrap().cancel_seen);
    // State is not corrupted: the same provider serves the next
    // call normally, and the cancellation left no fallback mark.
    rec.lock().unwrap().script = Some(CallScript::Fixed(Ok(7)));
    let mut provider = provider.lock().unwrap();
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Completed(7)
    );
    let status = provider.status();
    assert_eq!(status.state, ProviderState::Ready);
    assert_eq!(status.last_fallback, None);
    assert!(!cancel.load(Ordering::SeqCst), "the flag was consumed");
}

#[test]
fn issue727_engine_cancel_during_long_call_aborts_and_peeks() {
    let own = Arc::new(AtomicBool::new(false));
    let (provider, rec) = blocker_provider(
        own.clone(),
        CallScript::Block {
            budget: Duration::from_secs(8),
            poll: Duration::from_millis(1),
            then: 0,
            respect_cancel: true,
        },
    );
    let engine_flag = Arc::new(AtomicBool::new(false));
    let provider = Arc::new(Mutex::new(provider));
    let runner = {
        let provider = provider.clone();
        let engine_flag = engine_flag.clone();
        std::thread::spawn(move || {
            let check = move || engine_flag.load(Ordering::SeqCst);
            provider.lock().unwrap().execute_with_engine_cancel(
                &request("blocker", &[], abs_cwd()),
                true,
                &check,
            )
        })
    };
    std::thread::sleep(Duration::from_millis(150));
    engine_flag.store(true, Ordering::SeqCst);
    let outcome = runner.join().expect("runner panicked");
    assert_eq!(outcome, FfiOutcome::Cancelled);
    assert!(rec.lock().unwrap().cancel_seen);
    // Peek semantics: the engine's record is the dispatcher's to
    // adjudicate — the provider did not consume it, and the
    // provider's own flag never moved.
    assert!(engine_flag.load(Ordering::SeqCst));
    assert!(!own.load(Ordering::SeqCst));
}

#[test]
fn issue727_cancel_pending_before_call_never_touches_backend() {
    let cancel = Arc::new(AtomicBool::new(true));
    let (mut provider, rec) = blocker_provider(cancel.clone(), CallScript::Fixed(Ok(5)));
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Cancelled
    );
    assert_eq!(
        rec.lock().unwrap().calls,
        0,
        "a command the user already interrupted must not run at all"
    );
    // The pending flag was consumed by that verdict: the next
    // call runs.
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Completed(5)
    );
}

#[test]
fn issue727_unconsumed_interrupt_during_call_is_cleared_not_deferred() {
    // An applet that never polls the callback and returns success
    // anyway: the interrupt that landed mid-call is cleared by
    // the provider (the engine's interrupt boundary owns what
    // the user sees of it) — it must not leak into the next call.
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut provider, _rec) =
        blocker_provider(cancel.clone(), CallScript::SetFlagThen(cancel.clone(), 0));
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Completed(0)
    );
    assert!(
        !cancel.load(Ordering::SeqCst),
        "an unconsumed mid-call interrupt is cleared at the call boundary"
    );
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Completed(0)
    );
}

#[test]
fn issue727_noncooperative_applet_waits_out_but_host_survives() {
    // The ABI contract is cooperative checkpoints: an applet
    // that never polls cannot be preempted in-process. The host
    // waits out the call (bounded by the applet's own length),
    // survives, and serves the next call — the exact boundary of
    // what in-process cancellation can promise; the real DLL's
    // applets poll through the shared dispatch (the WinuxCmd
    // side of the contract), and the Windows console shape is a
    // real-machine cell.
    let cancel = Arc::new(AtomicBool::new(false));
    let (provider, _rec) = blocker_provider(
        cancel.clone(),
        CallScript::Block {
            budget: Duration::from_millis(400),
            poll: Duration::from_millis(10),
            then: 0,
            respect_cancel: false,
        },
    );
    let provider = Arc::new(Mutex::new(provider));
    let runner = {
        let provider = provider.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let outcome = provider
                .lock()
                .unwrap()
                .execute(&request("blocker", &[], abs_cwd()), true);
            (outcome, started.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    cancel.store(true, Ordering::SeqCst);
    let (outcome, elapsed) = runner.join().expect("runner panicked");
    assert_eq!(outcome, FfiOutcome::Completed(0));
    assert!(
        elapsed >= Duration::from_millis(350),
        "a non-polling applet runs to its own end: {elapsed:?}"
    );
    let mut provider = provider.lock().unwrap();
    assert_eq!(
        provider.execute(&grep_like_request(), true),
        FfiOutcome::Completed(0)
    );
    assert_eq!(provider.status().state, ProviderState::Ready);
}

// ---------------------------------------------------------------------
// Gate ① byte equivalence at the seam (unix only — the applet
// backend works over real file descriptors). A backend that
// implements real applet semantics over the ABI context's
// borrowed handles is compared against the spawn arm: the
// platform coreutils on the identical argv, env, cwd, and
// handles. The mock implements the DLL side faithfully for the
// shapes the battery uses (coreutils message bytes included);
// every case asserts FFI bytes == spawn bytes == the literal
// expectation, so a both-arms-wrong pass cannot hide.
// ---------------------------------------------------------------------

#[cfg(unix)]
mod byte_battery {
    use super::*;
    use niubash_runtime::ffi::route::RouteReason;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::mem::ManuallyDrop;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicU32;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    const DATA: &[u8] = b"alpha\nbeta gamma\ndelta\nbeta\n";
    const EXTRA: &[u8] = b"second file\nbeta tail\n";

    /// # Safety
    /// `ptr` must point at a live NUL-terminated u16 buffer.
    unsafe fn read_wide(ptr: *const u16) -> String {
        let mut units = Vec::new();
        let mut cursor = ptr;
        loop {
            let unit = unsafe { *cursor };
            if unit == 0 {
                break;
            }
            units.push(unit);
            cursor = unsafe { cursor.add(1) };
        }
        String::from_utf16_lossy(&units)
    }

    fn read_fd(fd: usize) -> Vec<u8> {
        // SAFETY: the harness lends the backend real, open fds
        // for the call's duration; ManuallyDrop keeps ownership
        // with the harness.
        let mut file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd as i32) });
        let mut buf = Vec::new();
        let _ = file.read_to_end(&mut buf);
        buf
    }

    fn write_fd(fd: usize, bytes: &[u8]) {
        // SAFETY: as `read_fd` — borrowed for the call only.
        let mut file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd as i32) });
        let _ = file.write_all(bytes);
        let _ = file.flush();
    }

    /// The DLL side, in miniature: parse the ABI memory exactly
    /// as `winuxcore.dll` would, run the applet over the borrowed
    /// handles, return its exit code.
    struct AppletBackend;

    impl FfiBackend for AppletBackend {
        fn abi_version(&mut self) -> Result<u32, String> {
            Ok(FFI_ABI_VERSION)
        }

        fn init(&mut self, _args: &InitRequest) -> Result<i32, String> {
            Ok(0)
        }

        fn call(
            &mut self,
            ctx: &WideContext,
            _cancel: CancelFn,
            _cancel_user_data: *mut c_void,
        ) -> Result<i32, String> {
            let raw = ctx.as_raw();
            // SAFETY: the provider keeps the WideContext alive
            // for the call; every pointer in the raw view borrows
            // from it (the ABI's ownership rule).
            let (applet, argv) = unsafe {
                let applet = read_wide(raw.applet);
                let argv = if raw.argc > 0 {
                    let slice = std::slice::from_raw_parts(raw.argv, raw.argc as usize);
                    slice.iter().map(|p| read_wide(*p)).collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                (applet, argv)
            };
            let cwd = unsafe { read_wide(raw.cwd) };
            let env: Vec<(String, String)> = parse_env_block(ctx.env_block());
            let rc = run_applet(
                &applet,
                &argv,
                &env,
                Path::new(&cwd),
                raw.stdin,
                raw.stdout,
                raw.stderr,
            );
            Ok(rc)
        }
    }

    fn parse_env_block(block: &[u16]) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let mut current = Vec::new();
        for &unit in block {
            if unit == 0 {
                if current.is_empty() {
                    break;
                }
                let text = String::from_utf16_lossy(&current);
                if let Some((k, v)) = text.split_once('=') {
                    pairs.push((k.to_string(), v.to_string()));
                }
                current.clear();
            } else {
                current.push(unit);
            }
        }
        pairs
    }

    fn open_input(cwd: &Path, arg: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(cwd.join(arg))
    }

    fn first_lines(data: &[u8], n: usize) -> Vec<u8> {
        if n == 0 {
            return Vec::new();
        }
        let mut seen = 0;
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == n {
                    return data[..=i].to_vec();
                }
            }
        }
        data.to_vec()
    }

    fn run_applet(
        applet: &str,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        stdin_fd: usize,
        stdout_fd: usize,
        stderr_fd: usize,
    ) -> i32 {
        match applet {
            "cat" => {
                let mut rc = 0;
                if argv.is_empty() {
                    let data = read_fd(stdin_fd);
                    write_fd(stdout_fd, &data);
                } else {
                    for arg in argv {
                        match open_input(cwd, arg) {
                            Ok(data) => write_fd(stdout_fd, &data),
                            Err(_) => {
                                write_fd(
                                    stderr_fd,
                                    format!("cat: {arg}: No such file or directory\n").as_bytes(),
                                );
                                rc = 1;
                            }
                        }
                    }
                }
                rc
            }
            "head" => {
                let mut rest: &[String] = argv;
                let mut n = 10usize;
                if argv.len() >= 2 && argv[0] == "-n" {
                    n = argv[1].parse().unwrap_or(10);
                    rest = &argv[2..];
                }
                let data = if rest.is_empty() {
                    read_fd(stdin_fd)
                } else {
                    match open_input(cwd, &rest[0]) {
                        Ok(data) => data,
                        Err(_) => {
                            write_fd(
                                stderr_fd,
                                format!(
                                    "head: cannot open '{}' for reading: No such file or directory\n",
                                    rest[0]
                                )
                                .as_bytes(),
                            );
                            return 1;
                        }
                    }
                };
                write_fd(stdout_fd, &first_lines(&data, n));
                0
            }
            "grep" => {
                let mut rest: &[String] = argv;
                let mut number = false;
                if rest.first().map(|s| s.as_str()) == Some("-n") {
                    number = true;
                    rest = &rest[1..];
                }
                let Some((pattern, files)) = rest.split_first() else {
                    write_fd(stderr_fd, b"grep: no pattern\n");
                    return 2;
                };
                let mut any_match = false;
                let mut any_error = false;
                let sources: Vec<(Option<String>, Vec<u8>)> = if files.is_empty() {
                    vec![(None, read_fd(stdin_fd))]
                } else {
                    let mut v = Vec::new();
                    for arg in files {
                        match open_input(cwd, arg) {
                            Ok(data) => v.push((Some(arg.clone()), data)),
                            Err(_) => {
                                write_fd(
                                    stderr_fd,
                                    format!("grep: {arg}: No such file or directory\n").as_bytes(),
                                );
                                any_error = true;
                            }
                        }
                    }
                    v
                };
                let multi = files.len() > 1;
                for (name, data) in sources {
                    for (index, line) in data.split_inclusive(|&b| b == b'\n').enumerate() {
                        let matched = pattern.is_empty()
                            || line.windows(pattern.len()).any(|w| w == pattern.as_bytes());
                        if !matched {
                            continue;
                        }
                        any_match = true;
                        let mut out = Vec::new();
                        if multi {
                            out.extend(name.as_deref().unwrap_or("").as_bytes());
                            out.push(b':');
                        }
                        if number {
                            out.extend(format!("{}:", index + 1).as_bytes());
                        }
                        out.extend(line);
                        // GNU terminates a matched final line
                        // that had no newline of its own.
                        if !line.ends_with(b"\n") {
                            out.push(b'\n');
                        }
                        write_fd(stdout_fd, &out);
                    }
                }
                if any_error {
                    2
                } else if any_match {
                    0
                } else {
                    1
                }
            }
            "envprobe" => {
                let Some(name) = argv.first() else {
                    return 1;
                };
                match env.iter().find(|(k, _)| k == name) {
                    Some((_, value)) => {
                        write_fd(stdout_fd, format!("{value}\n").as_bytes());
                        0
                    }
                    None => 1,
                }
            }
            "failcode" => argv
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0),
            // WINUX_FFI_ERR_UNKNOWN_APPLET: an FFI-layer error.
            _ => -1004,
        }
    }

    #[derive(Clone)]
    struct Case {
        name: &'static str,
        args: Vec<&'static str>,
        extra_env: Vec<(&'static str, &'static str)>,
        stdin: Vec<u8>,
        files: Vec<(&'static str, Vec<u8>)>,
        expect_stdout: Vec<u8>,
        expect_stderr: Vec<u8>,
        expect_rc: i32,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ArmResult {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        rc: i32,
    }

    fn case_dir(label: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "niu-issue727-battery-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create case dir");
        dir
    }

    fn seed(dir: &Path, case: &Case) {
        for (name, bytes) in &case.files {
            std::fs::write(dir.join(name), bytes).expect("seed file");
        }
    }

    fn env_pairs(case: &Case) -> Vec<(String, String)> {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        let mut pairs = vec![
            ("PATH".to_string(), path),
            ("LC_ALL".to_string(), "C".to_string()),
        ];
        for (k, v) in &case.extra_env {
            pairs.push((k.to_string(), v.to_string()));
        }
        pairs
    }

    fn ffi_arm(case: &Case, dir: &Path) -> ArmResult {
        seed(dir, case);
        let stdin_path = dir.join("stdin.bin");
        std::fs::write(&stdin_path, &case.stdin).unwrap();
        let stdout_path = dir.join("stdout.bin");
        let stderr_path = dir.join("stderr.bin");
        let stdin_file = File::open(&stdin_path).unwrap();
        let stdout_file = File::create(&stdout_path).unwrap();
        let stderr_file = File::create(&stderr_path).unwrap();
        let mut provider = FfiProvider::with_parts(
            on(),
            Arc::new(AtomicBool::new(false)),
            Some(candidate()),
            Some(table()),
            Some(AppletBackend),
        );
        let req = FfiRequest {
            name: case.name.to_string(),
            args: case.args.iter().map(|s| s.to_string()).collect(),
            env: env_pairs(case),
            cwd: dir.to_path_buf(),
            std: StdHandles {
                stdin: stdin_file.as_raw_fd() as usize,
                stdout: stdout_file.as_raw_fd() as usize,
                stderr: stderr_file.as_raw_fd() as usize,
            },
        };
        let outcome = provider.execute(&req, true);
        let rc = match outcome {
            FfiOutcome::Completed(rc) => rc,
            other => panic!("case {}: FFI arm must complete, got {other:?}", case.name),
        };
        drop(stdin_file);
        drop(stdout_file);
        drop(stderr_file);
        ArmResult {
            stdout: std::fs::read(&stdout_path).unwrap(),
            stderr: std::fs::read(&stderr_path).unwrap(),
            rc,
        }
    }

    fn coreutil(name: &str) -> PathBuf {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        for dir in path.split(':') {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!("coreutil {name} not found on PATH");
    }

    fn spawn_arm(case: &Case, dir: &Path) -> ArmResult {
        seed(dir, case);
        let stdin_path = dir.join("stdin.bin");
        std::fs::write(&stdin_path, &case.stdin).unwrap();
        let stdout_path = dir.join("stdout.bin");
        let stderr_path = dir.join("stderr.bin");
        let display_name = match case.name {
            "envprobe" => "printenv",
            name => name,
        };
        let program = coreutil(display_name);
        let status = Command::new(program)
            // A shell invokes the bare name: argv[0] is `head`,
            // not the resolved path — GNU diagnostics embed
            // argv[0], so the reference arm must present the
            // same argv[0] the product arms present.
            .arg0(display_name)
            .args(&case.args)
            .env_clear()
            .envs(env_pairs(case))
            .current_dir(dir)
            .stdin(Stdio::from(File::open(&stdin_path).unwrap()))
            .stdout(Stdio::from(File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(File::create(&stderr_path).unwrap()))
            .status()
            .expect("spawn arm runs");
        ArmResult {
            stdout: std::fs::read(&stdout_path).unwrap(),
            stderr: std::fs::read(&stderr_path).unwrap(),
            rc: status.code().expect("spawn arm exits by code"),
        }
    }

    /// Three-way pin: FFI arm == spawn arm == literal expectation.
    fn run_case(label: &str, case: Case) {
        let ffi = ffi_arm(&case, &case_dir(&format!("{label}-ffi")));
        let spawn = spawn_arm(&case, &case_dir(&format!("{label}-spawn")));
        let expected = ArmResult {
            stdout: case.expect_stdout.clone(),
            stderr: case.expect_stderr.clone(),
            rc: case.expect_rc,
        };
        assert_eq!(ffi, expected, "{label}: FFI arm vs expectation");
        assert_eq!(spawn, expected, "{label}: spawn arm vs expectation");
    }

    fn base_case(name: &'static str, args: Vec<&'static str>) -> Case {
        Case {
            name,
            args,
            extra_env: Vec::new(),
            stdin: Vec::new(),
            files: Vec::new(),
            expect_stdout: Vec::new(),
            expect_stderr: Vec::new(),
            expect_rc: 0,
        }
    }

    #[test]
    fn issue727_battery_cat_stdin_and_files() {
        // Binary stdin passes through byte-for-byte (NULs and
        // high bytes included) — the no-argument-massaging rule
        // made observable.
        let mut case = base_case("cat", vec![]);
        case.stdin = b"a\0b\xff\xfe\nbinary\0tail".to_vec();
        case.expect_stdout = case.stdin.clone();
        run_case("cat-stdin", case);

        let mut case = base_case("cat", vec!["data.txt", "extra.txt"]);
        case.files = vec![("data.txt", DATA.to_vec()), ("extra.txt", EXTRA.to_vec())];
        case.expect_stdout = [DATA, EXTRA].concat();
        run_case("cat-files", case);
    }

    #[test]
    fn issue727_battery_cat_missing_file_error_bytes() {
        let mut case = base_case("cat", vec!["missing.txt"]);
        case.expect_stderr = b"cat: missing.txt: No such file or directory\n".to_vec();
        case.expect_rc = 1;
        run_case("cat-missing", case);

        // The error does not stop the remaining operands.
        let mut case = base_case("cat", vec!["data.txt", "missing.txt", "extra.txt"]);
        case.files = vec![("data.txt", DATA.to_vec()), ("extra.txt", EXTRA.to_vec())];
        case.expect_stdout = [DATA, EXTRA].concat();
        case.expect_stderr = b"cat: missing.txt: No such file or directory\n".to_vec();
        case.expect_rc = 1;
        run_case("cat-mixed", case);
    }

    #[test]
    fn issue727_battery_grep_match_shapes() {
        let mut case = base_case("grep", vec!["beta", "data.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        case.expect_stdout = b"beta gamma\nbeta\n".to_vec();
        run_case("grep-match", case);

        let mut case = base_case("grep", vec!["-n", "beta", "data.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        case.expect_stdout = b"2:beta gamma\n4:beta\n".to_vec();
        run_case("grep-numbered", case);

        let mut case = base_case("grep", vec!["beta"]);
        case.stdin = DATA.to_vec();
        case.expect_stdout = b"beta gamma\nbeta\n".to_vec();
        run_case("grep-stdin", case);

        let mut case = base_case("grep", vec!["omega", "data.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        case.expect_rc = 1;
        run_case("grep-nomatch", case);
    }

    #[test]
    fn issue727_battery_grep_error_shapes() {
        let mut case = base_case("grep", vec!["beta", "missing.txt"]);
        case.expect_stderr = b"grep: missing.txt: No such file or directory\n".to_vec();
        case.expect_rc = 2;
        run_case("grep-missing", case);

        // Two operands: GNU prefixes matches with the file name,
        // the error still lands on stderr, and the status is 2.
        let mut case = base_case("grep", vec!["beta", "data.txt", "missing.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        case.expect_stdout = b"data.txt:beta gamma\ndata.txt:beta\n".to_vec();
        case.expect_stderr = b"grep: missing.txt: No such file or directory\n".to_vec();
        case.expect_rc = 2;
        run_case("grep-mixed", case);
    }

    #[test]
    fn issue727_battery_head_shapes() {
        let mut case = base_case("head", vec!["-n", "2", "data.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        case.expect_stdout = b"alpha\nbeta gamma\n".to_vec();
        run_case("head-file", case);

        let mut case = base_case("head", vec!["-n", "1"]);
        case.stdin = DATA.to_vec();
        case.expect_stdout = b"alpha\n".to_vec();
        run_case("head-stdin", case);

        let mut case = base_case("head", vec!["-n", "0", "data.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        run_case("head-zero", case);

        let mut case = base_case("head", vec!["-n", "2", "missing.txt"]);
        case.expect_stderr =
            b"head: cannot open 'missing.txt' for reading: No such file or directory\n".to_vec();
        case.expect_rc = 1;
        run_case("head-missing", case);
    }

    #[test]
    fn issue727_battery_envprobe_env_fidelity() {
        // The env block the executor composed (prefix assignments
        // included) reaches the applet verbatim — the probe reads
        // its answer out of the ABI env block, the spawn arm out
        // of the process env; the bytes must agree.
        for (var, expect_stdout, expect_rc) in [
            ("PROBE", b"hello world\n".to_vec(), 0),
            ("EMPTY", b"\n".to_vec(), 0),
            ("ABSENT", Vec::new(), 1),
        ] {
            let mut case = base_case("envprobe", vec![var]);
            case.extra_env = vec![("PROBE", "hello world"), ("EMPTY", "")];
            case.expect_stdout = expect_stdout;
            case.expect_rc = expect_rc;
            run_case(&format!("envprobe-{var}"), case);
        }
    }

    #[test]
    fn issue727_battery_pipeline_composition() {
        // cat | grep beta | head -n 1, composed stage by stage:
        // each stage is its own routed call shape (the bytes a
        // pipe would carry are materialized between stages).
        let dir = case_dir("pipe-ffi");
        std::fs::write(dir.join("data.txt"), DATA).unwrap();
        let stage = |name: &'static str, args: Vec<&'static str>, stdin: Vec<u8>| Case {
            name,
            args,
            extra_env: Vec::new(),
            stdin,
            files: Vec::new(),
            expect_stdout: Vec::new(),
            expect_stderr: Vec::new(),
            expect_rc: 0,
        };
        let ffi1 = ffi_arm(&stage("cat", vec!["data.txt"], Vec::new()), &dir);
        let ffi2 = ffi_arm(&stage("grep", vec!["beta"], ffi1.stdout.clone()), &dir);
        let ffi3 = ffi_arm(&stage("head", vec!["-n", "1"], ffi2.stdout.clone()), &dir);

        let dir = case_dir("pipe-spawn");
        std::fs::write(dir.join("data.txt"), DATA).unwrap();
        let spawn1 = spawn_arm(&stage("cat", vec!["data.txt"], Vec::new()), &dir);
        let spawn2 = spawn_arm(&stage("grep", vec!["beta"], spawn1.stdout.clone()), &dir);
        let spawn3 = spawn_arm(&stage("head", vec!["-n", "1"], spawn2.stdout.clone()), &dir);

        assert_eq!(ffi1, spawn1);
        assert_eq!(ffi2, spawn2);
        assert_eq!(ffi3, spawn3);
        assert_eq!(ffi3.stdout, b"beta gamma\n".to_vec());
        assert_eq!(ffi3.rc, 0);
    }

    #[test]
    fn issue727_battery_failcode_matches_spawn_reference() {
        // Applet exit codes pass through the ABI unchanged; the
        // spawn reference for a bare code is the shell's own exit.
        for code in [0, 1, 42] {
            let case = base_case("failcode", vec![]);
            let dir = case_dir("failcode-ffi");
            let mut provider = FfiProvider::with_parts(
                on(),
                Arc::new(AtomicBool::new(false)),
                Some(candidate()),
                Some(table()),
                Some(AppletBackend),
            );
            let mut req = request("failcode", &[&code.to_string()], dir.clone());
            req.env = env_pairs(&case);
            assert_eq!(
                provider.execute(&req, true),
                FfiOutcome::Completed(code),
                "failcode {code} through the FFI arm"
            );
            let status = Command::new("/bin/sh")
                .args(["-c", &format!("exit {code}")])
                .status()
                .expect("sh reference runs");
            assert_eq!(status.code(), Some(code));
        }
        // The wider passthrough range, pinned against the ABI
        // contract (codes >= 0 pass unchanged).
        for code in [2, 127, 200, 255] {
            let dir = case_dir("failcode-range");
            let mut provider = FfiProvider::with_parts(
                on(),
                Arc::new(AtomicBool::new(false)),
                Some(candidate()),
                Some(table()),
                Some(AppletBackend),
            );
            let req = request("failcode", &[&code.to_string()], dir);
            assert_eq!(provider.execute(&req, true), FfiOutcome::Completed(code));
        }
    }

    #[test]
    fn issue727_battery_switch_off_arm_is_the_spawn_arm() {
        // With the switch off the provider declines, so the spawn
        // arm's bytes ARE the off arm's bytes; combined with the
        // three-way pins above (FFI == spawn), off == on is
        // anchored, not assumed.
        let mut case = base_case("grep", vec!["beta", "data.txt"]);
        case.files = vec![("data.txt", DATA.to_vec())];
        let dir = case_dir("off-arm");
        seed(&dir, &case);
        let mut provider = FfiProvider::with_parts(
            off(),
            Arc::new(AtomicBool::new(false)),
            Some(candidate()),
            Some(table()),
            Some(AppletBackend),
        );
        let req = FfiRequest {
            name: case.name.to_string(),
            args: case.args.iter().map(|s| s.to_string()).collect(),
            env: env_pairs(&case),
            cwd: dir.clone(),
            std: StdHandles::default(),
        };
        match provider.execute(&req, true) {
            FfiOutcome::Declined(decision) => {
                assert_eq!(decision.reason, RouteReason::SwitchOff);
            }
            other => panic!("switch off must decline, got {other:?}"),
        }
        let spawn = spawn_arm(&case, &case_dir("off-arm-spawn"));
        assert_eq!(spawn.stdout, b"beta gamma\nbeta\n".to_vec());
        assert_eq!(spawn.rc, 0);
    }
}
