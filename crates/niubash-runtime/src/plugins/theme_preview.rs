//! Live theme preview (niubash#170): render a theme's real PS1 through a
//! throwaway child `niu`.
//!
//! The gallery preview must show what the user would actually get after the
//! pick — the theme's own prompt with its multi-line structure, segments and
//! colors — not a description of it. A theme activates by *sourcing bash
//! code* (the same managed block the rc carries, `assets::build_theme_block`),
//! which a wizard or menu cannot do in-process without mutating its own
//! session. So the render runs in a child `niu -c` process:
//!
//! 1. The child gets a sandboxed HOME (the golden-journey pattern) plus the
//!    real `NIU_PLUGIN_SOURCES_ROOT`, the managed theme block as its `-c`
//!    script, and the [`PRINT_RENDERED_PS1_ENV`] gate.
//! 2. The gate makes the `-c` host run the exact interactive pipeline once —
//!    precmd hooks (`PROMPT_COMMAND` through the engine's
//!    `execute_prompt_command`) then `expand_prompt_string_mut(PS1)`, the
//!    same call `shell.rs::sync_bash_prompt_from_env` makes on every
//!    interactive prompt — and print the bytes between markers, so theme
//!    load noise on stdout never reaches the parser here.
//! 3. The parent turns the bytes into display lines with the same
//!    width/escape discipline the menu paints under (drop the `\[`/`\]`
//!    non-printing markers, keep ANSI colors, cap the pane).
//!
//! Isolation: the child never sees the session's PS1/PROMPT_COMMAND/OSH_*
//! environment (removed before spawn) and its HOME/USERPROFILE point at a
//! throwaway directory, so a render cannot leak `OSH_THEME` into the caller
//! or touch the user's files. Each render is time-bounded (the child is
//! killed at [`RENDER_TIMEOUT`]); a hung theme degrades to a one-line
//! "(preview unavailable: …)" instead of freezing the gallery.
//!
//! [`GalleryPreviews`] is the cache both surfaces share (the setup wizard's
//! theme gallery and `niu plugin ui`'s theme section): renders are lazy
//! (started when a theme is first highlighted), run on a small worker pool,
//! cached for the life of the gallery, and the synchronous menu preview
//! callback only ever reads ready bytes or shows the rendering placeholder.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::assets;

/// Start marker of the rendered-PS1 block a preview child prints
/// (`shell.rs::print_rendered_prompt_for_preview`). Marker lines keep theme
/// load noise (a fixture theme may print while sourcing) out of the bytes
/// the preview parser reads.
pub const PS1_BEGIN_MARKER: &str = "__NIU_PS1_BEGIN__";
/// End marker of the rendered-PS1 block.
pub const PS1_END_MARKER: &str = "__NIU_PS1_END__";

/// The gate that turns a `niu -c` child into a prompt-render child: when set
/// on a `-c` invocation, the host prints the session's rendered PS1 between
/// the markers after the script finishes.
pub const PRINT_RENDERED_PS1_ENV: &str = "NIU_PRINT_RENDERED_PS1";

/// How long one theme render may take before the child is killed and the
/// preview degrades. A hung theme must never freeze the gallery.
pub const RENDER_TIMEOUT: Duration = Duration::from_millis(1500);

/// Prompt lines the preview pane shows at most (the theme header line above
/// them is the caller's). Multi-line prompts beyond this are truncated with
/// a `…` tail so the pane height stays fixed.
pub const MAX_PREVIEW_LINES: usize = 4;

/// Worker threads draining the render queue. Two is enough to keep up with
/// arrow-key browsing; more would only burn startup CPU on a 150-theme
/// gallery sweep.
const RENDER_WORKERS: usize = 2;

/// Placeholder shown while a theme's render is in flight.
pub const RENDERING_PLACEHOLDER: &str = "rendering preview …";

/// One theme a gallery can preview. The (source_id, name) pair is the same
/// identity the gallery row and the rc activation block use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreviewRequest {
    pub source_id: String,
    pub name: String,
}

/// What one theme render produced. `Rendered` carries the display lines
/// (prompt bytes with colors, ignore markers stripped); `Unavailable`
/// carries the degradation reason shown after "(preview unavailable: …)".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewOutcome {
    Rendered(Vec<String>),
    Unavailable(String),
}

impl PreviewOutcome {
    /// The lines the preview pane paints for this outcome (English defaults;
    /// the wizard localizes the prefix itself via [`PreviewState`]).
    pub fn lines(&self) -> Vec<String> {
        match self {
            PreviewOutcome::Rendered(lines) => lines.clone(),
            PreviewOutcome::Unavailable(reason) => {
                vec![format!("(preview unavailable: {reason})")]
            }
        }
    }
}

/// What the synchronous menu callback gets for one theme at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewState {
    /// The render finished; the display lines are ready.
    Ready(Vec<String>),
    /// The render is still in flight.
    Rendering,
    /// The render failed; the reason degrades to a one-line note.
    Unavailable(String),
}

/// The niu executable the preview children run. Tests point
/// `NIU_PREVIEW_NIU_EXE` at the freshly built binary — under `cargo test`
/// `current_exe` is the test harness, not niu.
fn preview_niu_exe() -> Option<PathBuf> {
    if let Some(exe) = std::env::var_os("NIU_PREVIEW_NIU_EXE") {
        if !exe.is_empty() {
            return Some(PathBuf::from(exe));
        }
    }
    std::env::current_exe().ok()
}

/// Render one theme now, synchronously: spawn the throwaway child, wait
/// within [`RENDER_TIMEOUT`], extract the marker block, and turn the bytes
/// into display lines. Never panics, never blocks past the timeout; every
/// failure comes back as a [`PreviewOutcome::Unavailable`] reason.
pub fn render_theme(request: &ThemePreviewRequest) -> PreviewOutcome {
    let Some(block) = assets::build_theme_block(&request.source_id, &request.name) else {
        return PreviewOutcome::Unavailable(format!(
            "source '{}' is not installed",
            request.source_id
        ));
    };
    let Some(exe) = preview_niu_exe() else {
        return PreviewOutcome::Unavailable("niu executable not found".into());
    };

    let sandbox = sandbox_home();
    let mut command = crate::console_guard::suppressed_command(&exe);
    command.arg("-c").arg(&block);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    // Isolation (niubash#170): the render must not read the session's prompt
    // state or touch the user's files. HOME/USERPROFILE move to a throwaway
    // dir (the golden-journey pattern — USERPROFILE wins in this product, so
    // both point at the sandbox), while NIU_PLUGIN_SOURCES_ROOT stays real so
    // the managed block still finds the source tree. Everything that could
    // inject a foreign prompt or startup file is removed.
    command.env("HOME", &sandbox);
    command.env("USERPROFILE", &sandbox);
    command.env("LOCALAPPDATA", sandbox.join("local-appdata"));
    command.env("APPDATA", sandbox.join("appdata"));
    command.env("TEMP", sandbox.join("tmp"));
    command.env("TMP", sandbox.join("tmp"));
    command.env(
        "NIU_PLUGIN_SOURCES_ROOT",
        super::sources::sources_root().as_os_str(),
    );
    command.env(PRINT_RENDERED_PS1_ENV, "1");
    command.env("NIU_PLUGIN_BOOTSTRAP", "off");
    for var in [
        "PS1",
        "PS2",
        "PS0",
        "PS4",
        "PROMPT_COMMAND",
        "RPROMPT",
        "OSH",
        "OSH_THEME",
        "BASH_IT_THEME",
        "NIU_THEME_SOURCE",
        "NIU_THEME",
        "NIU_ENV",
        "BASH_ENV",
        "ENV",
    ] {
        command.env_remove(var);
    }

    let spawned = command.spawn();
    let mut child = match spawned {
        Ok(child) => {
            crate::console_guard::adopt_orphan_console(child.id());
            child
        }
        Err(err) => {
            cleanup_sandbox(&sandbox);
            return PreviewOutcome::Unavailable(format!("could not start niu: {err}"));
        }
    };

    // Drain both pipes on threads so a chatty theme can never block on a
    // full pipe while the parent is waiting.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });

    let deadline = Instant::now() + RENDER_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_sandbox(&sandbox);
                return PreviewOutcome::Unavailable(format!("render failed: {err}"));
            }
        }
    };

    // Kill BEFORE draining the pipes: the readers finish only at pipe EOF,
    // and a still-running child holds the pipes open — joining first would
    // wait out the whole child runtime instead of the bound.
    if timed_out {
        let _ = child.kill();
        let _ = child.wait();
    }
    let stdout_bytes = stdout_thread.join().unwrap_or_default();
    let stderr_bytes = stderr_thread.join().unwrap_or_default();
    cleanup_sandbox(&sandbox);

    let Some(status) = status else {
        return PreviewOutcome::Unavailable(format!(
            "render timed out after {}ms",
            RENDER_TIMEOUT.as_millis()
        ));
    };

    let Some(rendered) = extract_rendered_ps1(&stdout_bytes) else {
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            let tail: String = stderr
                .lines()
                .next_back()
                .unwrap_or("theme failed to load")
                .to_string();
            return PreviewOutcome::Unavailable(tail);
        }
        return PreviewOutcome::Unavailable("theme set no prompt".into());
    };
    PreviewOutcome::Rendered(preview_lines(&rendered))
}

/// A throwaway HOME for one render (the golden-journey sandbox pattern).
fn sandbox_home() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("niu-theme-preview-{}-{nanos}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn cleanup_sandbox(sandbox: &std::path::Path) {
    let _ = std::fs::remove_dir_all(sandbox);
}

/// Pull the rendered PS1 out of a preview child's stdout: the bytes between
/// the begin marker (plus newline) and the newline before the end marker.
/// Everything outside the markers — theme load noise, framework prints — is
/// ignored.
pub fn extract_rendered_ps1(stdout: &[u8]) -> Option<String> {
    let begin = find_subslice(stdout, PS1_BEGIN_MARKER.as_bytes())?;
    let start = begin + PS1_BEGIN_MARKER.len();
    let start = match stdout.get(start) {
        Some(b'\n') => start + 1,
        Some(_) => start,
        None => return None,
    };
    let rest = stdout.get(start..)?;
    let end = find_subslice(rest, PS1_END_MARKER.as_bytes())?;
    let mut bytes = &rest[..end];
    if bytes.last() == Some(&b'\n') {
        bytes = &bytes[..bytes.len() - 1];
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Turn rendered PS1 bytes into preview pane lines: drop the `\[`/`\]`
/// non-printing markers (and the raw `\x01`/`\x02` readline markers the
/// engine emits with line editing on), keep ANSI colors, split on newlines,
/// cap at [`MAX_PREVIEW_LINES`].
pub fn preview_lines(rendered: &str) -> Vec<String> {
    let cleaned = rendered
        .replace(['\x01', '\x02'], "")
        .replace("\\[", "")
        .replace("\\]", "");
    let mut lines: Vec<String> = cleaned.split('\n').map(str::to_string).collect();
    // The trailing newline the channel appends must not read as an extra
    // empty prompt line.
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.len() > MAX_PREVIEW_LINES {
        lines.truncate(MAX_PREVIEW_LINES);
        if let Some(last) = lines.last_mut() {
            last.push_str(" …");
        }
    }
    lines
}

/// Worker body for one render job.
pub type ThemeRenderer = Arc<dyn Fn(&ThemePreviewRequest) -> PreviewOutcome + Send + Sync>;

struct Job {
    key: String,
    request: ThemePreviewRequest,
}

#[derive(Debug, Clone)]
enum Slot {
    /// Queued or rendering; no result yet.
    Pending,
    Ready(Vec<String>),
    Failed(String),
}

struct GalleryInner {
    slots: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
    tx: mpsc::Sender<Job>,
    rx: Mutex<mpsc::Receiver<Job>>,
    done: AtomicBool,
}

/// Shared, lazy, time-bounded preview cache for one gallery session (the
/// wizard's theme question or `niu plugin ui`'s theme section). Renders run
/// on a small worker pool; the synchronous menu callback only reads ready
/// results or shows the placeholder — with zero grace at the menu surfaces
/// (interactive_menu repaints the pane when a pending render lands), so a
/// highlight move costs a cache read and a queue send, never a render wait,
/// and a hung theme can only ever cost its own bounded child.
pub struct GalleryPreviews {
    inner: Arc<GalleryInner>,
}

impl Default for GalleryPreviews {
    fn default() -> Self {
        Self::new()
    }
}

impl GalleryPreviews {
    /// A cache rendering through the real child pipeline.
    pub fn new() -> Self {
        Self::with_renderer(Arc::new(render_theme))
    }

    /// A cache with an injected renderer (tests: no child processes).
    pub fn with_renderer(renderer: ThemeRenderer) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let inner = Arc::new(GalleryInner {
            slots: Mutex::new(HashMap::new()),
            tx: tx.clone(),
            rx: Mutex::new(rx),
            done: AtomicBool::new(false),
        });
        for _ in 0..RENDER_WORKERS {
            let inner = Arc::clone(&inner);
            let renderer = Arc::clone(&renderer);
            std::thread::spawn(move || loop {
                if inner.done.load(Ordering::Relaxed) {
                    break;
                }
                let job = match inner
                    .rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(100))
                {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let outcome = renderer(&job.request);
                let slot = Self::slot_of(&inner, &job.key);
                *slot.lock().unwrap() = match outcome {
                    PreviewOutcome::Rendered(lines) => Slot::Ready(lines),
                    PreviewOutcome::Unavailable(reason) => Slot::Failed(reason),
                };
                if inner.done.load(Ordering::Relaxed) {
                    break;
                }
            });
        }
        GalleryPreviews { inner }
    }

    fn slot_of(inner: &Arc<GalleryInner>, key: &str) -> Arc<Mutex<Slot>> {
        let mut slots = inner.slots.lock().unwrap();
        slots
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(Slot::Pending)))
            .clone()
    }

    /// The menu-callback entry point: returns immediately with the state for
    /// one theme, starting the render on first request. Waits up to `grace`
    /// for an in-flight render — bounded, so a slow or hung theme never
    /// freezes the menu.
    pub fn state_for(&self, source_id: &str, name: &str, grace: Duration) -> PreviewState {
        let slot = self.ensure_queued(source_id, name);

        let deadline = Instant::now() + grace;
        loop {
            let state = slot.lock().unwrap().clone();
            match state {
                Slot::Ready(lines) => return PreviewState::Ready(lines),
                Slot::Failed(reason) => return PreviewState::Unavailable(reason),
                Slot::Pending if Instant::now() >= deadline => {
                    return PreviewState::Rendering;
                }
                Slot::Pending => std::thread::sleep(Duration::from_millis(15)),
            }
        }
    }

    /// Queue a render for a theme the user is about to reach (the highlight
    /// neighbor) without ever waiting: the next highlight finds it ready.
    pub fn prefetch(&self, source_id: &str, name: &str) {
        let _ = self.ensure_queued(source_id, name);
    }

    fn ensure_queued(&self, source_id: &str, name: &str) -> Arc<Mutex<Slot>> {
        // \u{1} is a keyspace separator that cannot appear in a theme name:
        // same-name themes from different sources render separately.
        let key = format!("{source_id}\u{1}{name}");
        let (slot, fresh) = {
            let mut slots = self.inner.slots.lock().unwrap();
            match slots.get(&key) {
                Some(slot) => (Arc::clone(slot), false),
                None => {
                    let slot = Arc::new(Mutex::new(Slot::Pending));
                    slots.insert(key.clone(), Arc::clone(&slot));
                    (slot, true)
                }
            }
        };
        if fresh {
            let job = Job {
                key,
                request: ThemePreviewRequest {
                    source_id: source_id.to_string(),
                    name: name.to_string(),
                },
            };
            if self.inner.tx.send(job).is_err() {
                *slot.lock().unwrap() = Slot::Failed("preview renderer unavailable".into());
            }
        }
        slot
    }

    /// The lines the pane paints for one theme's state (English defaults;
    /// the wizard localizes the placeholder and the unavailable prefix).
    pub fn lines_for(&self, source_id: &str, name: &str, grace: Duration) -> Vec<String> {
        match self.state_for(source_id, name, grace) {
            PreviewState::Ready(lines) => lines,
            PreviewState::Rendering => vec![RENDERING_PLACEHOLDER.to_string()],
            PreviewState::Unavailable(reason) => {
                vec![format!("(preview unavailable: {reason})")]
            }
        }
    }

    /// The fixed pane height the Measure phase reports (constants only — the
    /// layout sweep must never start renders, niubash#170).
    pub fn measure_placeholder() -> Vec<String> {
        // One header line (the caller's) + MAX_PREVIEW_LINES prompt lines.
        vec![String::new(); MAX_PREVIEW_LINES + 1]
    }
}

impl Drop for GalleryPreviews {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn request(source: &str, name: &str) -> ThemePreviewRequest {
        ThemePreviewRequest {
            source_id: source.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn marker_extraction_ignores_load_noise_and_keeps_exact_bytes() {
        let noisy =
            b"loading robbyrussell\n__NIU_PS1_BEGIN__\n\x1b[32mface \x1b[0m$ \n__NIU_PS1_END__\n";
        let extracted = extract_rendered_ps1(noisy).expect("markers found");
        assert_eq!(extracted, "\x1b[32mface \x1b[0m$ ");
        assert!(extract_rendered_ps1(b"no markers here").is_none());
        assert!(extract_rendered_ps1(b"__NIU_PS1_BEGIN__\n").is_none());
    }

    #[test]
    fn preview_lines_strip_ignore_markers_keep_colors_and_cap() {
        // \x01/\x02 are the readline ignore markers the engine emits with
        // line editing on; literal \[ \] are the PS1 spelling of the same.
        let lines = preview_lines("\x01\x1b[32m\x02user\x01\x02@host\n$ ");
        assert_eq!(lines, vec!["\x1b[32muser@host", "$ "]);

        let escaped = preview_lines("\\[\\e[35m\\]x ");
        assert_eq!(escaped, vec!["\\e[35mx ".to_string()]);

        let tall = preview_lines("1\n2\n3\n4\n5\n6\n");
        assert_eq!(tall.len(), MAX_PREVIEW_LINES);
        assert!(tall[3].ends_with('…'), "{tall:?}");
    }

    #[test]
    fn cache_starts_lazy_and_caches_after_first_render() {
        let renders = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&renders);
        let cache = GalleryPreviews::with_renderer(Arc::new(move |_request| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            PreviewOutcome::Rendered(vec!["face $ ".to_string()])
        }));
        // First ask: within the grace window the render (40ms) lands, so the
        // caller sees the ready lines — one render started, lazily.
        let lines = cache.lines_for("oh-my-bash", "robbyrussell", Duration::from_secs(2));
        assert_eq!(lines, vec!["face $ "]);
        // Second ask: served from the cache, the renderer must not run again.
        let again = cache.lines_for("oh-my-bash", "robbyrussell", Duration::from_secs(2));
        assert_eq!(again, vec!["face $ "]);
        assert_eq!(renders.load(Ordering::SeqCst), 1);
        // A different theme is its own cache entry.
        let _ = cache.lines_for("oh-my-bash", "agnoster", Duration::from_secs(2));
        assert_eq!(renders.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cache_shows_placeholder_within_bounded_grace_when_render_is_slow() {
        let cache = GalleryPreviews::with_renderer(Arc::new(|_request| {
            std::thread::sleep(Duration::from_millis(250));
            PreviewOutcome::Rendered(vec!["slow face".to_string()])
        }));
        let started = Instant::now();
        let lines = cache.lines_for("s", "slow", Duration::from_millis(30));
        let elapsed = started.elapsed();
        assert_eq!(lines, vec![RENDERING_PLACEHOLDER]);
        assert!(
            elapsed < Duration::from_millis(200),
            "the grace bound must hold, took {elapsed:?}"
        );
        // The render still lands afterwards.
        std::thread::sleep(Duration::from_millis(400));
        let lines = cache.lines_for("s", "slow", Duration::from_millis(0));
        assert_eq!(lines, vec!["slow face"]);
    }

    #[test]
    fn failure_degrades_to_the_one_line_note() {
        let cache = GalleryPreviews::with_renderer(Arc::new(|request| {
            if request.name == "broken" {
                PreviewOutcome::Unavailable("boom".to_string())
            } else {
                PreviewOutcome::Rendered(vec!["ok".to_string()])
            }
        }));
        let lines = cache.lines_for("s", "broken", Duration::from_secs(2));
        assert_eq!(lines, vec!["(preview unavailable: boom)"]);
        // The failure is cached: same one line, no re-render loop.
        let again = cache.lines_for("s", "broken", Duration::from_secs(2));
        assert_eq!(again, vec!["(preview unavailable: boom)"]);
    }

    #[test]
    fn keys_of_different_sources_do_not_collide() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let cache = GalleryPreviews::with_renderer(Arc::new(move |request| {
            seen2.lock().unwrap().push(request.clone());
            PreviewOutcome::Rendered(vec![request.name.clone()])
        }));
        let a = cache.lines_for("oh-my-bash", "demox", Duration::from_secs(2));
        let b = cache.lines_for("bash-it", "demox", Duration::from_secs(2));
        assert_eq!(a, vec!["demox"]);
        assert_eq!(b, vec!["demox"]);
        assert_eq!(seen.lock().unwrap().len(), 2, "both renders must run");
        assert_eq!(
            seen.lock().unwrap()[0].source_id,
            "oh-my-bash",
            "same-name themes from different sources render separately"
        );
    }

    #[test]
    fn measure_placeholder_is_constant_and_render_free() {
        let renders = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&renders);
        let cache = GalleryPreviews::with_renderer(Arc::new(move |_request| {
            counter.fetch_add(1, Ordering::SeqCst);
            PreviewOutcome::Rendered(vec!["x".to_string()])
        }));
        let height = GalleryPreviews::measure_placeholder().len();
        assert_eq!(
            GalleryPreviews::measure_placeholder().len(),
            height,
            "the pane height must be constant"
        );
        assert_eq!(renders.load(Ordering::SeqCst), 0, "no renders started");
        let _ = cache; // workers stay idle through the whole measure phase
    }

    #[test]
    fn request_identity_round_trips() {
        let req = request("oh-my-bash", "robbyrussell");
        assert_eq!(req, request("oh-my-bash", "robbyrussell"));
        assert_ne!(req, request("oh-my-bash", "agnoster"));
    }
}
