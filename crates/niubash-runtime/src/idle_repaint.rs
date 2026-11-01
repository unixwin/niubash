//! niubash#787: the idle-prompt repaint channel for landed prompt data.
//!
//! The git prompt segment's value is computed on a scoped background
//! refresh thread (`git_prompt_cache`); when the refresh lands, the
//! cache notifies its landing observers. During command activity the
//! landed value is picked up by the next pass's stamp, but a prompt
//! sitting idle inside `read_line` had no consumer for the landing:
//! the segment stayed invisible until the first keypress.
//!
//! This module is the consumer. It holds, for the one interactive
//! session a process runs:
//!
//!  - the current editor's repaint-signal handle and the shared
//!    cycle-menu session (both re-registered on every editor build —
//!    rebuilds happen on edit-mode changes), and
//!  - the live Vcs cell of the prompt pass currently inside
//!    `read_line` (registered by the REPL around the read, nowhere
//!    else — so nothing here can fire while a command executes or in
//!    a non-interactive shell).
//!
//! On a landing for the displayed pass's worktree it re-stamps the
//! pass's live cell in place and requests exactly one editor repaint.
//! The editor re-evaluates the same prompt object, whose Vcs render
//! now reads the re-stamped cell — the S2 stamp contract is kept:
//! renders still never touch the cache; the re-stamp is a host write
//! on the landing thread, not a render read. While a completion-menu
//! session is open the repaint request is skipped (the cell is still
//! updated, so the next natural repaint carries the value) — an idle
//! repaint must never race the menu's own frames (niubash#778).
//!
//! Holding a repaint-signal clone switches the editor's input loop
//! from a blocking read to its designed 100 ms poll (the same mode
//! the external-printer feature uses), so a landing becomes visible
//! within one poll interval. No timer or poll is added on this side:
//! the landing itself is the only trigger.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::completion::cycle_menu::SharedCycleSession;

/// A request-repaint handle for the current editor: the closure
/// owns one clone of the editor's repaint signal and calls
/// `request_repaint` on it. The signal type itself is not exported
/// from the editor crate's root at this pin, so the handle is
/// type-erased — what arms the editor's input poll is the signal
/// clone's lifetime, which the closure preserves exactly.
pub(crate) type RepaintRequest = Arc<dyn Fn() + Send + Sync>;

/// The Vcs segment value one displayed prompt pass reads, shared
/// between the pass's prompt clone (which renders it) and this
/// module's landing path (which re-stamps it when a refresh lands
/// mid-pass). Created at stamp time with the value the pass read from
/// the segment cache; afterwards only a landing for the same
/// worktree writes it.
#[derive(Debug)]
pub struct LiveVcsCell {
    /// The worktree the segment belongs to. Identity/debugging
    /// state: the registry matches landings against the pass's own
    /// worktree key, so on non-Windows builds (where no production
    /// face stamps a cell) nothing reads this back.
    #[cfg_attr(not(windows), allow(dead_code))]
    worktree: PathBuf,
    value: Mutex<Option<String>>,
}

impl LiveVcsCell {
    pub(crate) fn new(worktree: PathBuf, value: Option<String>) -> Self {
        Self {
            worktree,
            value: Mutex::new(value),
        }
    }

    /// The worktree this cell's segment belongs to.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn worktree(&self) -> &Path {
        &self.worktree
    }

    /// The value the prompt currently shows.
    pub(crate) fn get(&self) -> Option<String> {
        self.value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Store a landed value; returns `true` when it differs from
    /// what the prompt currently shows (a repaint can change
    /// something), `false` when the landing is a no-op visually.
    pub(crate) fn set_if_changed(&self, value: &Option<String>) -> bool {
        let mut current = self
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *current == *value {
            return false;
        }
        *current = value.clone();
        true
    }
}

/// The editor-side hooks the landing path needs, replaced on every
/// editor build.
struct EditorHooks {
    request_repaint: RepaintRequest,
    cycle_session: Option<SharedCycleSession>,
}

#[derive(Default)]
struct Registry {
    editor: Option<EditorHooks>,
    /// The worktree the currently displayed pass's prompt belongs
    /// to, and its live Vcs cell when its face carries one (a
    /// stamped Segments prompt, the gitbash factory prompt). Both
    /// are set only while `read_line` is running.
    pass_worktree: Option<PathBuf>,
    pass_cell: Option<Arc<LiveVcsCell>>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    editor: None,
    pass_worktree: None,
    pass_cell: None,
});

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Register the freshly built editor's repaint hooks. Called by
/// `build_line_editor` on every build, so an edit-mode rebuild
/// replaces the previous editor's signal and menu session.
pub(crate) fn register_editor(
    request_repaint: RepaintRequest,
    cycle_session: Option<SharedCycleSession>,
) {
    registry().editor = Some(EditorHooks {
        request_repaint,
        cycle_session,
    });
}

/// Mark the start of a prompt pass inside `read_line`: landings for
/// `worktree` may now re-stamp `cell` and request a repaint.
pub(crate) fn begin_pass(worktree: Option<PathBuf>, cell: Option<Arc<LiveVcsCell>>) {
    let mut registry = registry();
    registry.pass_worktree = worktree;
    registry.pass_cell = cell;
}

/// Mark the end of the pass: later landings find no displayed
/// prompt and stay silent (the next pass stamps the landed value
/// itself).
pub(crate) fn end_pass() {
    let mut registry = registry();
    registry.pass_worktree = None;
    registry.pass_cell = None;
}

/// The cache landing observer's production instance: one shared
/// closure for the process, so repeated editor builds register the
/// same observer (the cache deduplicates by identity).
fn landing_observer() -> Arc<dyn Fn(&Path, &Option<String>) + Send + Sync> {
    static OBSERVER: OnceLock<Arc<dyn Fn(&Path, &Option<String>) + Send + Sync>> = OnceLock::new();
    OBSERVER
        .get_or_init(|| Arc::new(|worktree, value| on_segment_landed(worktree, value)))
        .clone()
}

/// Install the landing observer into the segment cache (idempotent;
/// called on every editor build).
pub(crate) fn ensure_cache_observer() {
    crate::git_prompt_cache::add_landing_observer(landing_observer());
}

/// Repaint requests fired so far (test builds only): the editor's
/// repaint signal exposes no reader outside the engine, so the
/// registry tests count requests here instead of peeking at the flag.
#[cfg(test)]
static REPAINT_REQUESTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn repaint_request_count() -> usize {
    REPAINT_REQUESTS.load(std::sync::atomic::Ordering::SeqCst)
}

/// A segment refresh landed for `worktree` with `value`. Fires the
/// one idle repaint when the landing belongs to the displayed pass
/// and changes what it shows; see the module docs for the guards.
pub(crate) fn on_segment_landed(worktree: &Path, value: &Option<String>) {
    let request = {
        let registry = registry();
        // No displayed pass, or a pass over a different repository:
        // nothing on screen shows this worktree's segment.
        if registry.pass_worktree.as_deref() != Some(worktree) {
            return;
        }
        // Re-stamp the live cell first: even when the repaint below
        // is skipped (menu open), the next natural repaint must
        // carry the landed value.
        if let Some(cell) = &registry.pass_cell {
            if !cell.set_if_changed(value) {
                return; // The landing changes nothing on screen.
            }
        }
        let Some(editor) = &registry.editor else {
            return;
        };
        // Never disturb an active completion-menu session
        // (niubash#778): the updated cell rides the next natural
        // repaint instead.
        if let Some(session) = &editor.cycle_session {
            if session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_open()
            {
                return;
            }
        }
        editor.request_repaint.clone()
    };
    #[cfg(test)]
    REPAINT_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    request();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::cycle_menu::CycleSession;
    use crate::test_support::PROCESS_STATE_LOCK;

    #[test]
    fn issue787_landing_restamps_the_cell_and_fires_once() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap();
        end_pass();
        let worktree = PathBuf::from("/tmp/issue787-cell-repo");
        let cell = Arc::new(LiveVcsCell::new(worktree.clone(), None));
        register_editor(Arc::new(|| {}), None);
        begin_pass(Some(worktree.clone()), Some(cell.clone()));
        let before = repaint_request_count();
        on_segment_landed(&worktree, &Some("main".to_string()));
        assert_eq!(cell.get().as_deref(), Some("main"));
        assert_eq!(
            repaint_request_count() - before,
            1,
            "a landing that changes the shown value fires exactly one repaint"
        );
        // A second landing with the same value is a visual no-op:
        // no re-stamp change, no repaint.
        on_segment_landed(&worktree, &Some("main".to_string()));
        assert_eq!(repaint_request_count() - before, 1);
        // A dirty flip re-stamps and fires once more.
        on_segment_landed(&worktree, &Some("main*".to_string()));
        assert_eq!(cell.get().as_deref(), Some("main*"));
        assert_eq!(repaint_request_count() - before, 2);
        end_pass();
    }

    #[test]
    fn issue787_landing_for_another_worktree_is_ignored() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap();
        end_pass();
        let worktree = PathBuf::from("/tmp/issue787-other-repo");
        let cell = Arc::new(LiveVcsCell::new(worktree.clone(), None));
        register_editor(Arc::new(|| {}), None);
        begin_pass(Some(worktree.clone()), Some(cell.clone()));
        let before = repaint_request_count();
        on_segment_landed(
            Path::new("/tmp/issue787-elsewhere"),
            &Some("dev".to_string()),
        );
        assert_eq!(
            cell.get(),
            None,
            "another repo's landing must not land here"
        );
        assert_eq!(repaint_request_count(), before);
        end_pass();
    }

    #[test]
    fn issue787_landing_without_a_pass_is_ignored() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap();
        end_pass();
        let worktree = PathBuf::from("/tmp/issue787-nopass-repo");
        let cell = Arc::new(LiveVcsCell::new(worktree.clone(), None));
        register_editor(Arc::new(|| {}), None);
        let before = repaint_request_count();
        // No begin_pass: a landing while a command executes (or in a
        // non-interactive shell) must not touch any prompt state.
        on_segment_landed(&worktree, &Some("main".to_string()));
        assert_eq!(cell.get(), None);
        assert_eq!(repaint_request_count(), before);
    }

    #[test]
    fn issue787_open_menu_defers_the_repaint_but_keeps_the_value() {
        let _guard = PROCESS_STATE_LOCK.lock().unwrap();
        end_pass();
        let worktree = PathBuf::from("/tmp/issue787-menu-repo");
        let cell = Arc::new(LiveVcsCell::new(worktree.clone(), None));
        let session: SharedCycleSession = Arc::new(Mutex::new(CycleSession::default()));
        session.lock().unwrap().open = true;
        register_editor(Arc::new(|| {}), Some(session.clone()));
        begin_pass(Some(worktree.clone()), Some(cell.clone()));
        let before = repaint_request_count();
        on_segment_landed(&worktree, &Some("main".to_string()));
        assert_eq!(
            cell.get().as_deref(),
            Some("main"),
            "the value lands in the cell even while the menu owns the screen"
        );
        assert_eq!(
            repaint_request_count(),
            before,
            "no repaint may race an open completion menu; the updated cell rides the next natural repaint"
        );
        end_pass();
    }
}
