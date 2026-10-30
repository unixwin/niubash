//! The provider: policy over a backend.
//!
//! Owns the route decision for a call, the one-time discovery +
//! load + ABI handshake, the process-wide FFI execution lock
//! (DESIGN §5: the borrowed-window — process cwd/env/handles — is
//! serialized; at most one FFI call is in flight per process),
//! the cancellation flag the ABI callback reads, and the honest
//! status snapshot.
//!
//! Every failure below the route claim declines or falls back to
//! spawn (DESIGN §2.3): a claimed call whose backend load, ABI
//! check, init, context build, or call returns an FFI-layer error
//! comes back as [`FfiOutcome::Fallback`] and the caller runs the
//! external form — the user sees the command work, and the status
//! face records why FFI did not serve it.

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::backend::{self, FfiBackend, InitRequest, ERR_CANCELLED, ERR_FLOOR, FFI_ABI_VERSION};
use super::config::{FfiConfig, SwitchState};
use super::context::{FfiRequest, WideContext};
use super::discovery::{resolve_candidate, DllCandidate};
use super::route::{self, Route, RouteContext, RouteDecision};
use super::tiers::TierTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderState {
    /// Master switch off (default, or the kill switch).
    Disabled,
    /// Switch on; discovery has not run yet in this process.
    EnabledUnprobed,
    /// Discovery + tier table in hand; calls may be claimed.
    /// (The DLL itself loads lazily at the first claimed call, or
    /// eagerly under the doctor's probe.)
    Ready,
    /// Switch on but FFI cannot serve: no DLL, bad pointer, ABI
    /// out of range, init failure. Reason is in the status.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiStatus {
    pub switch: SwitchState,
    pub enabled: bool,
    pub state: ProviderState,
    /// Why FFI is unavailable (only in `Unavailable`).
    pub reason: Option<String>,
    pub dll_path: Option<PathBuf>,
    pub version_dir: Option<String>,
    /// WinuxCmd version from the tier table header.
    pub winuxcmd_version: Option<String>,
    pub loaded_abi: Option<u32>,
    /// (total tier rows, tier-2 rows, tier-1 rows).
    pub tier_counts: (usize, usize, usize),
    pub tier1_claimable: bool,
    /// Most recent silent-fallback reason, if any.
    pub last_fallback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfiOutcome {
    /// The applet ran in-process; the value is its exit code.
    Completed(i32),
    /// The route decision did not claim this call (switch off,
    /// emulation's name, not a winuxcmd name…): run it the
    /// ordinary way.
    Declined(RouteDecision),
    /// The route claimed the call but the FFI layer failed
    /// (<= -1000, load/ABI/init/context failure): run the
    /// external form. The string is the recorded reason.
    Fallback(String),
    /// A user interrupt was pending before the call or the applet
    /// reported cancellation: do NOT spawn it — the dispatch
    /// layer owns the interrupted status (130).
    Cancelled,
}

type BackendFactory<B> = Box<dyn FnMut(&Path) -> Result<B, String> + Send>;

pub struct FfiProvider<B: FfiBackend> {
    config: FfiConfig,
    cancel: Arc<AtomicBool>,
    root_resolver: Box<dyn Fn() -> Option<PathBuf> + Send>,
    backend_factory: BackendFactory<B>,
    probed: bool,
    candidate: Option<DllCandidate>,
    discovery_error: Option<String>,
    tiers: Option<TierTable>,
    tiers_error: Option<String>,
    backend: Option<B>,
    backend_error: Option<String>,
    /// The ABI handshake (version check + one-time init) has run
    /// for the backend in hand — tracked separately from the
    /// backend's presence so a pre-seeded backend (tests, future
    /// embedding) gets exactly the same handshake as a loaded one.
    backend_ready: bool,
    loaded_abi: Option<u32>,
    execution_lock: Mutex<()>,
    logged_routes: HashSet<String>,
    last_fallback: Option<String>,
}

fn default_install_root() -> Option<PathBuf> {
    let exe = crate::winuxcmd::find_winuxcmd()?;
    Some(crate::winuxcmd::installation_root(&exe))
}

fn wide_z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_wide_z(path: &Path) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
    #[cfg(not(windows))]
    {
        wide_z(&path.to_string_lossy())
    }
}

/// The locale snapshot for `winux_ffi_init`: effective
/// `WINUX_LANG`, else `LC_ALL`, else `LANG`, from the process
/// environment at init time (the ABI compares later per-call env
/// stamps against this snapshot itself).
fn locale_stamp() -> Option<Vec<u16>> {
    for name in ["WINUX_LANG", "LC_ALL", "LANG"] {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                return Some(wide_z(&value));
            }
        }
    }
    None
}

/// The ABI cancellation trampoline: reads the provider's flag
/// through the opaque user-data pointer.
///
/// # Safety
/// `user_data` must be the `Arc<AtomicBool>` pointer the provider
/// installed, or NULL.
unsafe extern "C" fn cancel_trampoline(user_data: *mut c_void) -> i32 {
    if user_data.is_null() {
        return 0;
    }
    // SAFETY: the provider passes `Arc::as_ptr` of its flag and
    // keeps the Arc alive for the whole call.
    let flag = unsafe { &*(user_data as *const AtomicBool) };
    flag.load(Ordering::SeqCst) as i32
}

impl<B: FfiBackend> FfiProvider<B> {
    pub fn with_factory<F>(config: FfiConfig, cancel: Arc<AtomicBool>, factory: F) -> Self
    where
        F: FnMut(&Path) -> Result<B, String> + Send + 'static,
    {
        FfiProvider {
            config,
            cancel,
            root_resolver: Box::new(default_install_root),
            backend_factory: Box::new(factory),
            probed: false,
            candidate: None,
            discovery_error: None,
            tiers: None,
            tiers_error: None,
            backend: None,
            backend_error: None,
            backend_ready: false,
            loaded_abi: None,
            execution_lock: Mutex::new(()),
            logged_routes: HashSet::new(),
            last_fallback: None,
        }
    }

    /// Test/embedding constructor with every resolved part in
    /// hand; the provider starts out probed.
    pub fn with_parts(
        config: FfiConfig,
        cancel: Arc<AtomicBool>,
        candidate: Option<DllCandidate>,
        tiers: Option<TierTable>,
        backend: Option<B>,
    ) -> Self {
        let mut provider = FfiProvider::with_factory(config, cancel, |_| {
            Err("no factory in with_parts provider".to_string())
        });
        provider.probed = true;
        provider.candidate = candidate;
        provider.tiers = tiers;
        provider.backend = backend;
        provider
    }

    pub fn config(&self) -> &FfiConfig {
        &self.config
    }

    /// Resolve discovery + the tier table (once). No-op when the
    /// switch is off: off means off, and startup must not pay for
    /// a disabled accelerator.
    pub fn probe(&mut self) {
        if self.probed {
            return;
        }
        self.probed = true;
        if !self.config.enabled {
            return;
        }
        let root = (self.root_resolver)();
        match resolve_candidate(self.config.dll_override.as_deref(), root.as_deref()) {
            Ok(candidate) => {
                match std::fs::read_to_string(&candidate.tiers_path) {
                    Ok(text) => match TierTable::parse(&text) {
                        Ok(table) => self.tiers = Some(table),
                        Err(err) => {
                            self.tiers_error =
                                Some(format!("{}: {err}", candidate.tiers_path.display()))
                        }
                    },
                    Err(err) => {
                        self.tiers_error =
                            Some(format!("{}: {err}", candidate.tiers_path.display()))
                    }
                }
                self.candidate = Some(candidate);
            }
            Err(err) => {
                self.discovery_error = Some(err.describe());
            }
        }
    }

    /// Probe, and when a candidate resolved, also attempt the
    /// load + ABI handshake so the status distinguishes
    /// "installed" from "loadable". The doctor path; session
    /// warm-up deliberately stops at [`FfiProvider::probe`].
    pub fn probe_and_load(&mut self) {
        self.probe();
        if self.candidate.is_some() && self.tiers.is_some() {
            let _ = self.ensure_backend();
        }
    }

    /// The route decision for one call. Probes lazily on first
    /// use (when the switch is on).
    pub fn route(&mut self, name: &str, args: &[String]) -> RouteDecision {
        self.probe();
        let decision = route::route_for(
            name,
            args,
            &RouteContext {
                config: &self.config,
                tiers: self.tiers.as_ref(),
                builtins: rubash::executor::Executor::builtin_command_names(),
                emulation_available: true,
            },
        );
        self.log_route(name, &decision);
        decision
    }

    fn log_route(&mut self, name: &str, decision: &RouteDecision) {
        if self.config.log_routes && !self.logged_routes.contains(name) {
            self.logged_routes.insert(name.to_string());
            eprintln!(
                "niu: ffi: route {name} -> {:?} ({:?})",
                decision.route, decision.reason
            );
        }
    }

    /// Execute one call through the FFI route when it claims it.
    /// `emulation_available` mirrors the session's emulation-layer
    /// state for the tier-1 decision.
    pub fn execute(&mut self, request: &FfiRequest, emulation_available: bool) -> FfiOutcome {
        self.probe();
        let decision = route::route_for(
            &request.name,
            &request.args,
            &RouteContext {
                config: &self.config,
                tiers: self.tiers.as_ref(),
                builtins: rubash::executor::Executor::builtin_command_names(),
                emulation_available,
            },
        );
        self.log_route(&request.name, &decision);
        if decision.route != Route::Ffi {
            return FfiOutcome::Declined(decision);
        }
        // An interrupt already pending means this command must
        // not run at all (spawn would be equally wrong); consume
        // the flag and report the cancellation.
        if self.cancel.swap(false, Ordering::SeqCst) {
            return FfiOutcome::Cancelled;
        }
        if let Err(reason) = self.ensure_backend() {
            return self.fallback(reason);
        }
        let applet = self
            .tiers
            .as_ref()
            .and_then(|tiers| tiers.canonical_name(&request.name))
            .unwrap_or_else(|| request.name.clone());
        let context = match WideContext::build(&applet, request) {
            Ok(context) => context,
            Err(err) => return self.fallback(format!("context: {}", err.describe())),
        };
        let cancel_data = Arc::as_ptr(&self.cancel) as *mut c_void;
        let call_result = {
            // The §5 borrow window: exactly one FFI call in
            // flight per process. The guard drops at the end of
            // this block, before any &mut self bookkeeping.
            let _borrow_window = self.execution_lock.lock().expect("ffi execution lock");
            let backend = self.backend.as_mut().expect("backend ensured");
            backend.call(&context, cancel_trampoline, cancel_data)
        };
        // An interrupt the applet did not consume must not leak
        // into the next call; the engine's own interrupt boundary
        // owns what the user sees of it.
        self.cancel.store(false, Ordering::SeqCst);
        match call_result {
            Ok(rc) if rc >= 0 => FfiOutcome::Completed(rc),
            Ok(ERR_CANCELLED) => FfiOutcome::Cancelled,
            Ok(rc) if rc <= ERR_FLOOR => {
                self.fallback(format!("FFI layer error {rc} from winuxcore.dll"))
            }
            Ok(rc) => self.fallback(format!("unexpected negative code {rc} from winuxcore.dll")),
            Err(reason) => self.fallback(reason),
        }
    }

    fn fallback(&mut self, reason: String) -> FfiOutcome {
        self.last_fallback = Some(reason.clone());
        FfiOutcome::Fallback(reason)
    }

    /// Load the backend and run the ABI handshake: version first
    /// (before any other call, per the ABI), then one-time init
    /// with the explicit install root and locale snapshot. Any
    /// failure poisons the backend for the process (the DLL is
    /// never unloaded or retried — the next process is the retry).
    fn ensure_backend(&mut self) -> Result<(), String> {
        if let Some(err) = &self.backend_error {
            return Err(err.clone());
        }
        if self.backend_ready {
            return Ok(());
        }
        // Step 1: have a backend. The factory path needs the
        // resolved candidate; a pre-seeded backend skips the load.
        if self.backend.is_none() {
            let Some(candidate) = &self.candidate else {
                return Err(self
                    .discovery_error
                    .clone()
                    .unwrap_or_else(|| "no DLL candidate".to_string()));
            };
            match (self.backend_factory)(&candidate.dll_path) {
                Ok(backend) => self.backend = Some(backend),
                Err(err) => {
                    self.backend_error = Some(err.clone());
                    return Err(err);
                }
            }
        }
        // Step 2: the ABI handshake, exactly once, for whatever
        // backend is in hand — version first (before any other
        // call, per the ABI), then one-time init with the explicit
        // install root and locale snapshot. (A backend seeded
        // without a candidate inits with an empty root, which the
        // ABI accepts as NULL = "derive from the module path".)
        let install_root = self
            .candidate
            .as_ref()
            .map(|candidate| path_wide_z(&candidate.install_root))
            .unwrap_or_default();
        let backend = self.backend.as_mut().expect("backend present");
        let abi = match backend.abi_version() {
            Ok(abi) => abi,
            Err(err) => {
                self.backend_error = Some(err.clone());
                return Err(err);
            }
        };
        if abi != FFI_ABI_VERSION {
            let err = format!(
                "winuxcore.dll ABI version {abi} is outside the known range (host speaks {FFI_ABI_VERSION})"
            );
            self.backend_error = Some(err.clone());
            return Err(err);
        }
        let init = InitRequest {
            install_root,
            locale_stamp: locale_stamp(),
        };
        match backend.init(&init) {
            Ok(0) => {}
            Ok(rc) => {
                let err = format!("winux_ffi_init failed with code {rc}");
                self.backend_error = Some(err.clone());
                return Err(err);
            }
            Err(err) => {
                self.backend_error = Some(err.clone());
                return Err(err);
            }
        }
        self.loaded_abi = Some(abi);
        self.backend_ready = true;
        Ok(())
    }

    pub fn status(&self) -> FfiStatus {
        let state = if !self.config.enabled {
            ProviderState::Disabled
        } else if !self.probed {
            ProviderState::EnabledUnprobed
        } else if let Some(err) = self
            .discovery_error
            .clone()
            .or_else(|| self.tiers_error.clone())
            .or_else(|| self.backend_error.clone())
        {
            return FfiStatus {
                switch: self.config.switch,
                enabled: true,
                state: ProviderState::Unavailable,
                reason: Some(err),
                dll_path: self.candidate.as_ref().map(|c| c.dll_path.clone()),
                version_dir: self.candidate.as_ref().map(|c| c.version_dir.clone()),
                winuxcmd_version: self.tiers.as_ref().and_then(|t| t.winuxcmd_version.clone()),
                loaded_abi: self.loaded_abi,
                tier_counts: self.tier_counts(),
                tier1_claimable: route::TIER1_FFI_CLAIMABLE,
                last_fallback: self.last_fallback.clone(),
            };
        } else {
            ProviderState::Ready
        };
        FfiStatus {
            switch: self.config.switch,
            enabled: self.config.enabled,
            state,
            reason: None,
            dll_path: self.candidate.as_ref().map(|c| c.dll_path.clone()),
            version_dir: self.candidate.as_ref().map(|c| c.version_dir.clone()),
            winuxcmd_version: self.tiers.as_ref().and_then(|t| t.winuxcmd_version.clone()),
            loaded_abi: self.loaded_abi,
            tier_counts: self.tier_counts(),
            tier1_claimable: route::TIER1_FFI_CLAIMABLE,
            last_fallback: self.last_fallback.clone(),
        }
    }

    fn tier_counts(&self) -> (usize, usize, usize) {
        match &self.tiers {
            Some(tiers) => (
                tiers.len(),
                tiers.names_with_tier(super::tiers::Tier::Tier2).len(),
                tiers.names_with_tier(super::tiers::Tier::Tier1).len(),
            ),
            None => (0, 0, 0),
        }
    }
}

#[cfg(windows)]
impl FfiProvider<backend::WindowsDllBackend> {
    /// The production constructor the runtime singleton uses on
    /// Windows: the backend factory is the DLL loader.
    pub fn new(config: FfiConfig, cancel: Arc<AtomicBool>) -> Self {
        Self::with_factory(config, cancel, backend::WindowsDllBackend::load)
    }
}

#[cfg(not(windows))]
impl FfiProvider<backend::UnavailableBackend> {
    /// Off Windows there is nothing to load: the provider still
    /// routes, probes, and reports status, and every execution
    /// attempt declines through the unavailable backend.
    pub fn new(config: FfiConfig, cancel: Arc<AtomicBool>) -> Self {
        Self::with_factory(config, cancel, |_| Ok(backend::UnavailableBackend))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::context::{RawContext, StdHandles};
    use std::sync::Mutex as StdMutex;

    const TABLE: &str = "# winuxcmd-version: 9.9.9-test\n\
command\ttier\tfallback_shapes\tnote\n\
cp\ttier1\t-\tTier-1\n\
grep\ttier2\t-\tTier-2\n";

    #[derive(Default)]
    struct MockState {
        abi: u32,
        init_rc: i32,
        call_rc: i32,
        set_flag_during_call: bool,
        calls: usize,
        captured_applet: String,
        captured_argv: Vec<String>,
        captured_env: Vec<u16>,
        captured_cwd: String,
        captured_handles: (usize, usize, usize),
        cancel_observed: i32,
        init_root: Vec<u16>,
    }

    struct MockBackend {
        state: Arc<StdMutex<MockState>>,
    }

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

    impl FfiBackend for MockBackend {
        fn abi_version(&mut self) -> Result<u32, String> {
            Ok(self.state.lock().unwrap().abi)
        }

        fn init(&mut self, args: &InitRequest) -> Result<i32, String> {
            let mut state = self.state.lock().unwrap();
            state.init_root = args.install_root.clone();
            Ok(state.init_rc)
        }

        fn call(
            &mut self,
            ctx: &WideContext,
            cancel: backend::CancelFn,
            cancel_user_data: *mut c_void,
        ) -> Result<i32, String> {
            let raw: RawContext = ctx.as_raw();
            let mut state = self.state.lock().unwrap();
            state.calls += 1;
            // Read the ABI memory exactly as the DLL would.
            unsafe {
                state.captured_applet = read_wide(raw.applet);
                state.captured_cwd = read_wide(raw.cwd);
                if raw.argc > 0 {
                    let argv = std::slice::from_raw_parts(raw.argv, raw.argc as usize);
                    state.captured_argv = argv.iter().map(|p| read_wide(*p)).collect();
                }
                state.captured_env = ctx.env_block().to_vec();
                state.captured_handles = (raw.stdin, raw.stdout, raw.stderr);
                if state.set_flag_during_call {
                    let flag = &*(cancel_user_data as *const AtomicBool);
                    flag.store(true, Ordering::SeqCst);
                }
                state.cancel_observed = cancel(cancel_user_data);
            }
            Ok(state.call_rc)
        }
    }

    fn provider_with_mock(
        state: Arc<StdMutex<MockState>>,
        config: FfiConfig,
    ) -> FfiProvider<MockBackend> {
        let tiers = TierTable::parse(TABLE).expect("table");
        let candidate = DllCandidate {
            dll_path: PathBuf::from("winuxcore.dll"),
            tiers_path: PathBuf::from("tiers.tsv"),
            version_dir: "v9".to_string(),
            install_root: PathBuf::from(if cfg!(windows) { "C:\\root" } else { "/root" }),
        };
        FfiProvider::with_parts(
            config,
            Arc::new(AtomicBool::new(false)),
            Some(candidate),
            Some(tiers),
            Some(MockBackend { state }),
        )
    }

    fn on() -> FfiConfig {
        FfiConfig::from_pairs([("NIU_WINUXCMD_FFI", "1")])
    }

    fn mock_state(abi: u32, call_rc: i32) -> Arc<StdMutex<MockState>> {
        Arc::new(StdMutex::new(MockState {
            abi,
            call_rc,
            ..Default::default()
        }))
    }

    fn grep_request() -> FfiRequest {
        FfiRequest {
            name: "grep".to_string(),
            args: vec!["-n".to_string(), "a b".to_string()],
            env: vec![("PREFIXED".to_string(), "1".to_string())],
            cwd: PathBuf::from(if cfg!(windows) { "C:\\work" } else { "/work" }),
            std: StdHandles {
                stdin: 5,
                stdout: 6,
                stderr: 7,
            },
        }
    }

    #[test]
    fn claimed_call_reaches_backend_with_context_verbatim() {
        let state = mock_state(FFI_ABI_VERSION, 42);
        let mut provider = provider_with_mock(state.clone(), on());
        let outcome = provider.execute(&grep_request(), true);
        assert_eq!(outcome, FfiOutcome::Completed(42));
        let state = state.lock().unwrap();
        assert_eq!(state.calls, 1);
        assert_eq!(state.captured_applet, "grep");
        assert_eq!(state.captured_argv, vec!["-n", "a b"]);
        assert_eq!(
            state.captured_cwd,
            if cfg!(windows) { "C:\\work" } else { "/work" }
        );
        assert_eq!(state.captured_handles, (5, 6, 7));
        let expected_env: Vec<u16> = "PREFIXED=1".encode_utf16().chain([0, 0]).collect();
        assert_eq!(state.captured_env, expected_env);
        // Init ran once, with the candidate's install root.
        assert!(!state.init_root.is_empty());
    }

    #[test]
    fn ffi_layer_error_falls_back_to_spawn() {
        // -1004 = WINUX_FFI_ERR_UNKNOWN_APPLET, an FFI-layer error.
        let state = mock_state(FFI_ABI_VERSION, -1004);
        let mut provider = provider_with_mock(state, on());
        match provider.execute(&grep_request(), true) {
            FfiOutcome::Fallback(reason) => assert!(reason.contains("-1004")),
            other => panic!("expected Fallback, got {other:?}"),
        }
        assert!(provider.status().last_fallback.is_some());
    }

    #[test]
    fn abi_mismatch_is_unavailable_not_a_crash() {
        let state = mock_state(99, 0);
        let mut provider = provider_with_mock(state.clone(), on());
        assert!(matches!(
            provider.execute(&grep_request(), true),
            FfiOutcome::Fallback(_)
        ));
        assert_eq!(state.lock().unwrap().calls, 0);
        let status = provider.status();
        assert_eq!(status.state, ProviderState::Unavailable);
        assert!(status.reason.unwrap().contains("ABI version 99"));
    }

    #[test]
    fn pending_interrupt_cancels_without_calling() {
        let state = mock_state(FFI_ABI_VERSION, 0);
        let mut provider = provider_with_mock(state.clone(), on());
        provider.cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            provider.execute(&grep_request(), true),
            FfiOutcome::Cancelled
        );
        assert_eq!(state.lock().unwrap().calls, 0);
    }

    #[test]
    fn interrupt_during_call_is_observed_by_the_callback() {
        let state = mock_state(FFI_ABI_VERSION, ERR_CANCELLED);
        state.lock().unwrap().set_flag_during_call = true;
        let mut provider = provider_with_mock(state.clone(), on());
        assert_eq!(
            provider.execute(&grep_request(), true),
            FfiOutcome::Cancelled
        );
        assert_eq!(state.lock().unwrap().cancel_observed, 1);
    }

    #[test]
    fn declined_routes_never_touch_the_backend() {
        let state = mock_state(FFI_ABI_VERSION, 0);
        let mut provider = provider_with_mock(state.clone(), on());
        // tier-1 with emulation available: declined to emulation.
        let mut req = grep_request();
        req.name = "cp".to_string();
        match provider.execute(&req, true) {
            FfiOutcome::Declined(d) => assert_eq!(d.route, Route::Emulation),
            other => panic!("expected Declined, got {other:?}"),
        }
        assert_eq!(state.lock().unwrap().calls, 0);
    }

    #[test]
    fn switch_off_declines_and_status_is_disabled() {
        let state = Arc::new(StdMutex::new(MockState::default()));
        let mut provider =
            provider_with_mock(state, FfiConfig::from_pairs(Vec::<(String, String)>::new()));
        assert!(matches!(
            provider.execute(&grep_request(), true),
            FfiOutcome::Declined(_)
        ));
        assert_eq!(provider.status().state, ProviderState::Disabled);
    }
}
