//! `niu doctor`: one-command health check for a Niubash installation.
//!
//! Reuses the same probes the setup wizard uses (winuxcmd discovery,
//! command links, nerd font) and prints a compact report with fix hints.
//! Critical checks decide the trailing count; advisory rows (font, shims,
//! language) never fail the run.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

const OK: &str = "\x1b[1;92m✅\x1b[0m";
const WARN: &str = "\x1b[1;93m⚠️\x1b[0m";
const INFO: &str = "\x1b[1;96mℹ️\x1b[0m";

/// Text of the n/a row for a Windows-only component skipped on another
/// platform (niubash#194). A shared constant so the tests assert the very
/// string the report prints.
const WINDOWS_COMPONENT_NA: &str = "n/a on this platform — Windows component, skipped";

/// Which platform family the doctor is running on (niubash#194).
///
/// The Windows command layer (winuxcmd core, command links, bash/sh shims)
/// does not exist on Linux/macOS; probing it there always reports `not
/// found` and drags the critical tally down falsely. `current()` resolves
/// at compile time via `cfg!(windows)`, but the checks branch on the enum
/// value so the skip logic is unit-testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    Unix,
}

impl Platform {
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Unix
        }
    }

    /// Label shown in the doctor header so the report names its platform.
    fn label(self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::Unix => "unix",
        }
    }

    /// Whether the Windows-only command layer (winuxcmd core, command
    /// links, bash/sh shims) applies on this platform and must be probed.
    fn checks_windows_command_layer(self) -> bool {
        matches!(self, Platform::Windows)
    }
}

/// Run all checks and print the report. Colors are dropped when stdout is
/// not a terminal so piped output stays plain.
///
/// `skill_files` is the launcher-embedded agent skill bundle (empty in
/// builds that embed nothing): the doctor reports install state per named
/// agent skill dir (niubash#188), advisory only — a missing skill install
/// never fails the run.
pub fn run_doctor(skill_files: &[crate::skill::SkillFile]) -> anyhow::Result<()> {
    let color = crate::terminal::stdout_is_terminal();
    let (ok, warn, info) = if color {
        (OK, WARN, INFO)
    } else {
        ("OK", "ADVICE", "INFO")
    };
    let mut out = io::stdout();
    let mut critical_passed = 0usize;
    let mut critical_total = 0usize;
    let platform = Platform::current();

    if color {
        writeln!(
            out,
            "\x1b[1mniubash doctor\x1b[0m — {} ({})",
            env!("CARGO_PKG_VERSION"),
            platform.label()
        )?;
    } else {
        writeln!(
            out,
            "niubash doctor — {} ({})",
            env!("CARGO_PKG_VERSION"),
            platform.label()
        )?;
    }
    writeln!(out)?;

    // ── Critical: the Unix command core (Windows-only layer, #194) ────────
    // winuxcmd is the native-Windows Unix command layer; on Linux/macOS it
    // can never be found, so the row is reported as n/a and stays out of
    // the critical tally instead of failing the run.
    if platform.checks_windows_command_layer() {
        critical_total += 1;
        match crate::winuxcmd::find_winuxcmd() {
            Some(exe) => {
                let version = crate::winuxcmd::version()
                    .map(|v| format!(" (winuxcmd {v})"))
                    .unwrap_or_default();
                writeln!(out, "  {ok} winuxcmd core       {}{version}", display(&exe))?;
                critical_passed += 1;

                // The bash/sh forwarder shims ship beside the winuxcmd tree.
                let shim = exe
                    .parent()
                    .map(|dir| dir.join("bash.exe"))
                    .filter(|p| p.is_file());
                match shim {
                    Some(_) => writeln!(
                        out,
                        "  {info} bash/sh shims       present — `bash` and `sh` forward to niu"
                    )?,
                    None => writeln!(
                        out,
                        "  {info} bash/sh shims       not beside winuxcmd (optional)"
                    )?,
                }
            }
            None => writeln!(
                out,
                "  {warn} winuxcmd core       not found — reinstall Niubash or check PATH"
            )?,
        }
    } else {
        writeln!(out, "  {info} winuxcmd core       {WINDOWS_COMPONENT_NA}")?;
    }

    // ── Critical: command links on PATH (Windows-only layer, #194) ────────
    if platform.checks_windows_command_layer() {
        critical_total += 1;
        if crate::winuxcmd::command_links_ready() {
            let count = crate::winuxcmd::list_commands().len();
            writeln!(
                out,
                "  {ok} command links       {count} commands (ls, cat, grep, …)"
            )?;
            critical_passed += 1;
        } else {
            // Windows-only recovery text: the wpm command-layer verb exists
            // only there, and the `wpm` string must not surface in
            // non-Windows builds — compile-time gate, not a runtime check.
            #[cfg(windows)]
            writeln!(
                out,
                "  {warn} command links       missing (ls/cat/grep) — restart niu or run `wpm links rebuild`"
            )?;
            #[cfg(not(windows))]
            writeln!(
                out,
                "  {warn} command links       missing (ls/cat/grep) — restart niu"
            )?;
        }
    } else {
        writeln!(out, "  {info} command links       {WINDOWS_COMPONENT_NA}")?;
    }

    // ── Advisory: bundled shim shape (niubash#810) ────────────────────────
    // `usr\bin\awk.exe` can sit as a standalone binary where the bundled
    // gawk component ships a winuxcmd shim — an old bundle's direct
    // payload, or a flat `wpm install` receipt claiming the same path. The
    // name then bypasses the forwarder's `-v BINMODE=3` argv adapter and
    // `awk` keeps emitting CRLF while `gawk` works. Startup activation
    // reconciles it on the next start; this row names the file and the
    // repair for machines where the heal could not land yet (a locked
    // file, a not-yet-restarted session).
    if platform.checks_windows_command_layer() {
        if let Some(exe) = crate::winuxcmd::find_winuxcmd() {
            let root = crate::winuxcmd::installation_root(&exe);
            for (path, why) in crate::winuxcmd::bundled_shim_shape_violations(&root) {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                writeln!(out, "  {warn}{}", bundled_shim_violation_row(&name, &why))?;
            }
        }
    }

    // ── Critical: interactive startup rc ───────────────────────────────────
    critical_total += 1;
    let home = crate::path_utils::shell_home_dir().unwrap_or_else(|| PathBuf::from("."));
    let rc = home.join(".niubashrc");
    if rc.is_file() {
        writeln!(out, "  {ok} startup rc          {}", display(&rc))?;
        critical_passed += 1;
    } else {
        writeln!(
            out,
            "  {warn} startup rc          none — run `niu setup` to create ~/.niubashrc"
        )?;
    }

    // ── Advisory: wizard answers files (1.5.0 TOML→JSON) ──────────────────
    // Read-only like every doctor row: the wizard converts a pre-1.5.0
    // `wizard-answers.toml` on its load path, so the doctor only names
    // the states that conversion leaves behind — a legacy file it
    // refused (left untouched, its answers ignored) or an unreadable
    // JSON — instead of letting them fail silently.
    if let Some(problem) = crate::setup_wizard::wizard_answers_problem(&home) {
        if problem.is_warning() {
            writeln!(out, "  {warn} wizard answers      {}", problem.describe())?;
        } else {
            writeln!(out, "  {info} wizard answers      {}", problem.describe())?;
        }
    }

    // ── Advisory: plugin loader / legacy managed blocks (niubash#359) ─────
    // Read-only: the generated loader is the activation channel and the rc
    // is never touched, so a legacy managed block in the rc (the old
    // write-era state) is reported with its exact migration verb — the
    // removal stays the user's own edit, forever.
    let loader_text = crate::plugins::loader::read_loader_text();
    let rc_text_read = std::fs::read_to_string(&rc).ok();
    let rc_legacy_blocks: Option<Vec<_>> = rc_text_read
        .as_deref()
        .and_then(|text| crate::plugins::loader::scan_legacy_blocks(text).ok())
        .filter(|blocks| !blocks.is_empty());
    // CR F2/F2-prime: the doctor reports the guard's OWN evidence, and the
    // "stays OFF" claim is keyed on the guard's verdict (guard_trips), not
    // on the block scan — a held-off loader is never diagnosed as clean,
    // and an inert (comment-only) legacy pair is never claimed to hold the
    // loader off.
    let guard_trips: Vec<(usize, String)> = rc_text_read
        .as_deref()
        .map(crate::plugins::loader::guard_trip_lines)
        .unwrap_or_default();
    let held_off = !guard_trips.is_empty();
    let legacy_seen = rc_legacy_blocks.is_some() || held_off;
    // niubash#586: the doctor judges the loader by the SAME validate()
    // the startup path gates on — a truncated or foreign loader is a
    // warn (it will not be sourced), never an ok, and a file that exists
    // but cannot be read as text is not reported as "none".
    let loader_invalid: Option<String> = loader_text
        .as_deref()
        .and_then(|text| crate::plugins::loader::validate(text).err())
        .map(|err| err.to_string());
    let loader_unreadable =
        loader_text.is_none() && crate::plugins::loader::loader_path().is_file();
    match (&loader_text, legacy_seen) {
        (None, false) => {
            if loader_unreadable {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — exists but could not be read as text; \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else {
                writeln!(
                    out,
                    "  {info} plugin loader      none — plugin sources activate through it once \
                     a verb manages them"
                )?;
            }
        }
        (Some(_), false) => {
            if let Some(reason) = &loader_invalid {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — failed validation ({reason}); \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else {
                writeln!(
                    out,
                    "  {ok} plugin loader      {} (sourced before the rc; user overrides win)",
                    display(&crate::plugins::loader::loader_path())
                )?;
            }
        }
        (loader, true) => {
            let mut lines: Vec<String> = rc_legacy_blocks
                .unwrap_or_default()
                .iter()
                .map(|b| format!("{} (lines {}-{})", b.id, b.start_line, b.end_line))
                .collect();
            // Guard trips the block scan does not attribute are named by
            // line, so the state is always explainable.
            for (line, text) in &guard_trips {
                lines.push(format!("guard trip at line {line}: {text}"));
            }
            // The hold verdict comes from the guard itself (F2-prime).
            let state = if held_off {
                "the generated loader stays OFF while they execute"
            } else {
                "inert (comments only — nothing executes, the loader is unaffected)"
            };
            writeln!(
                out,
                "  {warn} legacy rc blocks   {} — {state}; remove them BY HAND after \
                 reviewing: `niu plugin migrate-rc`",
                lines.join(", ")
            )?;
            if let Some(reason) = &loader_invalid {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — failed validation ({reason}); \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else if loader_unreadable {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — exists but could not be read as text; \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else if loader.is_none() {
                writeln!(
                    out,
                    "  {info} plugin loader      none yet — `niu plugin migrate-rc` publishes \
                     the candidate generation from the current manifest"
                )?;
            }
        }
    }
    if crate::plugins::assets::rc_has_migrated_debris() {
        writeln!(
            out,
            "  {warn} rc moved-out debris stale moved-out remnants of the retired drift guard \
             found in the rc (an older niu's write-era damage, niubash#324/#647) — niu never \
             repairs your rc; review the moved-out sections by hand"
        )?;
    }

    // ── Advisory: PATH-shadowed niu (wt82-L01 V1 class) ──────────────────
    // The rc bootstrap and every bare `niu` typing resolve `niu` through
    // PATH. When PATH finds a *different* niu than the one running right
    // now, startup reconcile runs under that binary's semantics and old
    // vintages print `unknown plugin subcommand` noise on every start.
    match find_niu_on_path() {
        Some(path_niu)
            if !same_executable(&path_niu, &std::env::current_exe().unwrap_or_default())
                && !same_packaged_origin(&path_niu) =>
        {
            writeln!(
                out,
                "  {warn} niu on PATH         {} is NOT this niu ({}) — \
                 bare `niu` and the rc bootstrap run the PATH copy; \
                 update/reinstall it (`niu --self-update` from that copy) or \
                 reorder PATH so the current install wins",
                display(&path_niu),
                std::env::current_exe()
                    .map(|e| display(&e))
                    .unwrap_or_else(|_| "?".to_string()),
            )?;
        }
        _ => {}
    }

    // ── Advisory rows ───────────────────────────────────────────────────────
    if crate::fonts::nerd_font_installed() {
        writeln!(
            out,
            "  {ok} nerd font           detected — icon themes unlocked"
        )?;
    } else {
        writeln!(
            out,
            "  {info} nerd font           not found — icon themes need one: `niu font`"
        )?;
    }

    // ── Advisory: release-bundled tools (niubash#230 follow-up) ───────────
    // The release ships gawk/niugit/ripgrep/fd inside the winuxcmd opt/ tree
    // (preinstall manifest). The doctor reports them with their true source
    // so a bundled component is never mistaken for a system install — and
    // never reported as missing. The bundled layout is a Windows-release
    // concept (#194): on other platforms the winuxcmd tree is absent, so
    // the row is labeled n/a rather than reading as "nothing installed".
    let bundled = crate::setup_wizard::bundled_components();
    writeln!(out, "  {info}{}", bundled_tools_row(&bundled, platform))?;

    let terminal = match platform {
        Platform::Windows => {
            if std::env::var_os("WT_SESSION").is_some() {
                "Windows Terminal".to_string()
            } else {
                "console host".to_string()
            }
        }
        // WT_SESSION is a Windows Terminal variable; on unix fall back to
        // TERM so the row stays informative instead of always "console host".
        Platform::Unix => std::env::var("TERM")
            .map(|term| format!("unix terminal ({term})"))
            .unwrap_or_else(|_| "unix terminal".to_string()),
    };
    writeln!(out, "  {info} terminal            {terminal}")?;

    // ── Advisory: elevation channel (niubash#429, decision A) ────────────
    // Elevation is delegated, never built in: the Rubash `sudo` builtin
    // stays disabled unless NIU_ENABLE_RUBASH_SUDO=1, and an external
    // provider owns UAC — Microsoft sudo (in-box from Windows build
    // 26045) or gsudo. The doctor names the provider this machine
    // actually has so a user without either (the common Windows 10 case)
    // learns why `sudo` does not elevate here instead of hitting a
    // silently disabled builtin. Windows-only layer (#194): on other
    // platforms the row is n/a and the probes never run.
    let provider = if platform.checks_windows_command_layer() {
        elevation_provider(
            windows_build_number(),
            find_exe_on_path("sudo").is_some(),
            find_exe_on_path("gsudo").is_some(),
        )
    } else {
        ElevationProvider::None
    };
    let (tone, row) = elevation_channel_row(provider, platform);
    let mark = match tone {
        RowTone::Ok => ok,
        RowTone::Warn => warn,
        RowTone::Info => info,
    };
    writeln!(out, "  {mark}{row}")?;

    // ── Advisory: TAB completion shape (2026-10-12 discovery surface) ─────
    // The completion style was invisible config before 1.4.1 — the wizard
    // never asked and the env variable was hidden knowledge. The doctor
    // states the effective value (file/env/default precedence applied) and
    // names every switch, so the five shapes are discoverable.
    let (style, source) = crate::config::resolve_completion_style();
    let origin = match source {
        crate::config::SettingSource::Env => " (NIU_COMPLETION_STYLE override)",
        crate::config::SettingSource::Inputrc => " (inputrc)",
        crate::config::SettingSource::Default => " (default)",
    };
    writeln!(
        out,
        "  {info} completion style    {}{origin} — change with: \
         `export NIU_COMPLETION_STYLE=ide|column|list|inline|gnu` \
         in the environment or ~/.niubashrc",
        style.as_str()
    )?;

    // ── Advisory: TAB completion interaction (niubash#631) ──────────────
    // The second completion axis rides beside the style row: how TAB
    // drives the line while candidates are shown, with the same
    // precedence display (env > config > default).
    let (interaction, interaction_source) = crate::config::resolve_completion_interaction();
    let interaction_origin = match interaction_source {
        crate::config::SettingSource::Env => " (NIU_COMPLETION_INTERACTION override)",
        crate::config::SettingSource::Inputrc => " (inputrc)",
        crate::config::SettingSource::Default => " (default)",
    };
    writeln!(
        out,
        "  {info} completion interact {}{interaction_origin} — change with: \
         `export NIU_COMPLETION_INTERACTION=insert|cycle` \
         in the environment or ~/.niubashrc",
        interaction.as_str()
    )?;

    // ── Advisory: key bindings (niubash#630) ─────────────────────────────
    // The doctor is the standing face of the unified keymap registry:
    // the effective table itself is the help topic's job, but the
    // dropped records (with one-line fixes) live here permanently, so
    // a bind that never landed is discoverable without waiting for a
    // startup reminder. The snapshot is the last interactive build's;
    // doctor never sources rc files to rebuild it.
    // niubash#843: no home directory → no state path → the None arm
    // ("no interactive session has built a keymap yet" wording is
    // the closest honest face; doctor never fabricates a cwd path).
    match crate::keymap_registry::state_path()
        .and_then(|path| crate::keymap_registry::load_state_from(&path))
    {
        Some(state) => {
            writeln!(
                out,
                "  {info} key bindings      {} effective ({} mode, registry generation {}), \
                 {} dropped — full table and fixes: `{}`",
                state.entries.len(),
                state.editor_mode,
                state.registry_generation,
                state.dropped.len(),
                crate::keymap_registry::keymap_help_command()
            )?;
            for bind in &state.dropped {
                writeln!(
                    out,
                    "  {info}   dropped '{}' ({}) — {}; fix: {}",
                    bind.keyseq,
                    bind.target_name.as_deref().unwrap_or(""),
                    bind.reason.why(),
                    bind.session_fix()
                )?;
            }
        }
        None => {
            writeln!(
                out,
                "  {info} key bindings      no interactive session has built a keymap yet \
                 — `{}` explains the binding layers",
                crate::keymap_registry::keymap_help_command()
            )?;
        }
    }

    // ── Advisory: timing scripts (niubash#201) ────────────────────────────
    // `date` is an external program: each `$(date +%s%N)` pays Windows
    // process creation (~10-30ms). The bash 5 builtin `$EPOCHREALTIME`
    // expands in microseconds and drives millisecond-timeline scripts
    // (frame schedulers, benchmarks) three orders of magnitude faster.
    writeln!(
        out,
        "  {info} timing              use $EPOCHREALTIME for timing scripts — \
         `$(date +%s%N)` spawns date.exe (~30ms/call), `$EPOCHREALTIME` is a \
         builtin (~µs, microsecond format like 1699999999.123456)"
    )?;

    // ── Advisory: external plugin sources (iron law 2: fallback explicit) ──
    // Sources whose tree is gone must be *visible*: the guarded loaders
    // fell back to the niubash defaults at startup and the user deserves
    // to know.
    let sources = crate::plugins::sources::list_sources();
    // niubash#178: an unparsable registry silently reads as "none installed"
    // everywhere, so the doctor must call it out explicitly — naming the bad
    // line (via the parse error) and the fix — instead of letting the empty
    // view stand.
    if let Some(corruption) = crate::plugins::sources::registry_corruption() {
        writeln!(
            out,
            "  {warn} plugin registry     UNPARSABLE ({}) — every registry \
             record in it is UNLOADED and sync/install are refused until you \
             fix or remove the file; the original is preserved at {}",
            corruption.error.lines().next().unwrap_or("parse error"),
            crate::plugins::sources::corrupt_registry_sidecar_path().display(),
        )?;
    }
    if sources.is_empty() {
        writeln!(
            out,
            "  {info} plugin sources      none installed — browse with `niu plugin discover`"
        )?;
    } else {
        let ready = sources.iter().filter(|s| !s.missing).count();
        let missing = sources.iter().filter(|s| s.missing).count();
        writeln!(
            out,
            "  {info} plugin sources      {ready} ready, {missing} missing \
             (`niu plugin list` for assets)"
        )?;
        for status in sources.iter().filter(|s| s.missing) {
            writeln!(
                out,
                "  {warn} source missing      '{}' tree missing — its assets stay unloaded; \
                 repair with `niu plugin restore {}`",
                status.record.id, status.record.id
            )?;
        }
    }

    // ── Advisory: declarative spec reconciliation (§14.6.3) ──────────────
    match crate::plugins::spec::load_spec() {
        Err(err) => {
            writeln!(out, "  {warn} plugin spec         unreadable: {err}")?;
        }
        Ok(None) => {
            writeln!(
                out,
                "  {info} plugin spec         none — imperative mode \
                 (`niu plugin add <target>` creates ~/.niubash/plugins.json)"
            )?;
        }
        Ok(Some(spec)) => {
            let registry = crate::plugins::sources::read_source_registry();
            let declared: Vec<String> = spec
                .sources
                .iter()
                .filter_map(crate::plugins::spec::resolve_entry_id)
                .collect();
            let missing: Vec<String> = declared
                .iter()
                .filter(|id| !registry.iter().any(|record| record.id == **id))
                .cloned()
                .collect();
            let missing: Vec<&str> = missing.iter().map(String::as_str).collect();
            let undeclared = registry
                .iter()
                .filter(|record| !declared.contains(&record.id))
                .count();
            let missing_note = if missing.is_empty() {
                String::new()
            } else {
                format!("; missing: {} (run `niu plugin sync`)", missing.join(", "))
            };
            let undeclared_note = if undeclared == 0 {
                String::new()
            } else {
                format!("; {undeclared} installed-but-undeclared (`niu plugin sync` to review)")
            };
            writeln!(
                out,
                "  {info} plugin spec         {} declared source(s){missing_note}{undeclared_note}",
                spec.sources.len()
            )?;
        }
    }

    // ── Advisory: git mirroring (§14.8, China-network comfort) ──
    // Report-only — the product never auto-switches a mirror; the user
    // pastes their own mirror URL (`niu plugin mirror set <url>`). niu has
    // no HTTP transport to probe (download retraction 2026-10-04), so the
    // row states the active channel instead of probing GitHub.
    match crate::plugins::mirrors::load_mirror_config() {
        Err(err) => {
            writeln!(
                out,
                "  {warn} git mirror          {} unreadable ({err}) — git fetches go direct",
                crate::plugins::mirrors::mirrors_path().display()
            )?;
        }
        Ok(_) => {
            let mirror = crate::plugins::mirrors::resolve_active_mirror();
            if !mirror.is_none() {
                let channel = mirror
                    .git_instead_of_base
                    .clone()
                    .unwrap_or_else(|| "(no insteadOf base set)".to_string());
                writeln!(
                    out,
                    "  {info} git mirror          custom active ({channel}) — \
                     `niu plugin mirror set none` to disable"
                )?;
            } else {
                writeln!(
                    out,
                    "  {info} git mirror          direct (no mirror configured)"
                )?;
                writeln!(
                    out,
                    "  {info}                     if GitHub fetches stall (e.g. mainland China), \
                     point git at a mirror of your choice: `niu plugin mirror set <url>`"
                )?;
            }
        }
    }

    if crate::setup_wizard::wizard_lang_is_chinese() {
        writeln!(
            out,
            "  {info} language            zh (wizard follows it; override with NIU_LANG)"
        )?;
    }

    // ── Advisory: agent skill bundle (niubash#188) ────────────────────────
    // AI hosts (Claude/ZCode/Cursor) only know niubash's capability surface
    // if the bundle is installed into their skill dirs. Report per named
    // target; the generic `--target <dir>` installs are visible via
    // `niu skill status --target <dir>`.
    if !skill_files.is_empty() {
        let mut any_installed = false;
        let mut any_outdated = false;
        let mut installed_count = 0usize;
        let mut resolvable = 0usize;
        for (_, skills_root) in crate::skill::NAMED_TARGETS {
            let Some(dir) = crate::path_utils::shell_home_dir()
                .map(|home| home.join(skills_root).join(crate::skill::SKILL_DIR_NAME))
            else {
                continue;
            };
            resolvable += 1;
            let status = crate::skill::check_target(skill_files, &dir);
            if !status.installed {
                continue;
            }
            any_installed = true;
            installed_count += 1;
            if !status.up_to_date {
                any_outdated = true;
            }
        }
        if !any_installed {
            writeln!(
                out,
                "  {info} agent skill         not installed — `niu skill install` \
                 teaches AI hosts this shell's capability surface"
            )?;
        } else if any_outdated {
            writeln!(
                out,
                "  {warn} agent skill         outdated in an installed target — \
                 refresh with `niu skill install` (details: `niu skill status`)"
            )?;
        } else {
            writeln!(
                out,
                "  {ok} agent skill         current at {installed_count}/{resolvable} \
                 named targets (`niu skill status` for paths)"
            )?;
        }
    }

    // ── Advisory: Claude Code's shell (niubash#1031) ─────────────────────
    // Claude Code picks its Bash with a probe that runs before the git on
    // PATH: with Git for Windows installed it runs Git Bash, and a
    // CLAUDE_CODE_GIT_BASH_PATH that names no file is ignored silently.
    // The row names the shell the host will actually run, so the fallback
    // is visible before the agent writes commands for the wrong shell.
    // Windows-only and only when a Claude Code config dir exists — the
    // variable has no meaning elsewhere, and without the config dir there
    // is nothing to classify.
    if let Some(facts) = gather_claude_shell_facts(&home) {
        if let Some((tone, row)) = claude_shell_row(&facts, platform) {
            let mark = match tone {
                RowTone::Ok => ok,
                RowTone::Warn => warn,
                RowTone::Info => info,
            };
            writeln!(out, "  {mark}{row}")?;
        }
    }

    writeln!(out)?;
    if critical_passed == critical_total {
        writeln!(
            out,
            "  {ok} {critical_passed}/{critical_total} critical checks passed"
        )?;
    } else {
        writeln!(
            out,
            "  {warn} {critical_passed}/{critical_total} critical checks passed — fix the rows above"
        )?;
    }
    out.flush()?;
    Ok(())
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// One `niu doctor` advisory row for a bundled-shim shape violation
/// (niubash#810): `usr\bin\<name>` is a standalone binary where the
/// bundled component ships a winuxcmd shim, so the name misses the
/// forwarder's argv adapters (`awk` keeps emitting CRLF while `gawk`
/// works). Shared with the tests so the printed repair hint stays pinned.
fn bundled_shim_violation_row(name: &str, why: &str) -> String {
    format!(
        "bundled shim {name:<12} {why} — run `niu --relink-bundled-shims --force` \
         (old bytes kept as {name}.bak-<date>)"
    )
}

/// The `bundled tools` advisory row (without the leading two-space indent;
/// the caller prepends the info marker). Honest either way: the names of
/// what the release actually ships, or an explicit "none". On non-Windows
/// platforms (#194) the winuxcmd opt/ layout the bundle lives in does not
/// exist, so an empty bundle reads as n/a, not as an empty install.
fn bundled_tools_row(bundled: &[&str], platform: Platform) -> String {
    if !bundled.is_empty() {
        format!(
            " bundled tools       {} (shipped with the release, not system installs)",
            bundled.join(" ")
        )
    } else if platform.checks_windows_command_layer() {
        " bundled tools       none — this install ships no pre-installed packages".to_string()
    } else {
        " bundled tools       n/a — bundled tools ship in the Windows release layout".to_string()
    }
}

/// Severity of a doctor row that carries text produced by a helper (the
/// report loop owns the colored/plain markers; helpers stay pure so the
/// row wording is unit-testable without capturing stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowTone {
    Ok,
    Warn,
    Info,
}

/// Which external provider answers elevation on this machine
/// (niubash#429, decision A). Niubash itself never elevates: the Rubash
/// `sudo` builtin is opt-in only and UAC belongs to one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElevationProvider {
    /// Microsoft sudo: in-box `sudo.exe`, ships from Windows build 26045
    /// (Windows 11 24H2).
    MsSudo,
    /// gsudo on PATH (the WPM package).
    Gsudo,
    /// Neither provider detected.
    None,
}

/// First Windows build carrying Microsoft sudo in-box (Windows 11 24H2).
const MS_SUDO_MIN_BUILD: u32 = 26045;

/// Classify the elevation channel from the raw probes. Microsoft sudo
/// wins when both providers exist — it is the in-box channel, and gsudo
/// stays reachable by its own name either way. The build floor is part
/// of the test: a stray `sudo.exe` on an older build is not Microsoft
/// sudo and must not be reported as one.
fn elevation_provider(
    build: Option<u32>,
    ms_sudo_on_path: bool,
    gsudo_on_path: bool,
) -> ElevationProvider {
    if ms_sudo_on_path && matches!(build, Some(b) if b >= MS_SUDO_MIN_BUILD) {
        ElevationProvider::MsSudo
    } else if gsudo_on_path {
        ElevationProvider::Gsudo
    } else {
        ElevationProvider::None
    }
}

/// The `elevation channel` advisory row (without the leading two-space
/// indent; the caller prepends the tone marker). Non-Windows platforms
/// reuse the shared n/a wording (#194): the MS-sudo/gsudo layer is a
/// Windows component, and the row must never read as "no provider
/// found" there.
fn elevation_channel_row(provider: ElevationProvider, platform: Platform) -> (RowTone, String) {
    if !platform.checks_windows_command_layer() {
        return (
            RowTone::Info,
            format!(" elevation channel   {WINDOWS_COMPONENT_NA}"),
        );
    }
    match provider {
        ElevationProvider::MsSudo => (
            RowTone::Ok,
            " elevation channel   Microsoft sudo detected (build >= 26045, sudo.exe on PATH) \
             — elevation is delegated to Windows; niu's builtin sudo stays off \
             (opt-in: NIU_ENABLE_RUBASH_SUDO=1)"
                .to_string(),
        ),
        ElevationProvider::Gsudo => (
            RowTone::Ok,
            " elevation channel   gsudo detected on PATH — elevation is delegated to \
             gsudo; niu's builtin sudo stays off (opt-in: NIU_ENABLE_RUBASH_SUDO=1)"
                .to_string(),
        ),
        ElevationProvider::None => (
            RowTone::Warn,
            " elevation channel   none found — install Microsoft sudo (Windows 11 24H2, \
             build >= 26045) or gsudo (`wpm install gsudo`) for elevated commands; \
             niu's builtin sudo is opt-in only (NIU_ENABLE_RUBASH_SUDO=1)"
                .to_string(),
        ),
    }
}

/// Where a `CLAUDE_CODE_GIT_BASH_PATH` value came from (niubash#1031):
/// the `env` table of `~/.claude/settings.json` wins over the process
/// environment, and the host ignores a value that names no file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeShellValueSource {
    SettingsJson,
    ProcessEnv,
    Unset,
}

impl ClaudeShellValueSource {
    /// Short label for the row text, naming the exact place to fix.
    fn label(self) -> &'static str {
        match self {
            ClaudeShellValueSource::SettingsJson => "settings.json env",
            ClaudeShellValueSource::ProcessEnv => "the process environment",
            ClaudeShellValueSource::Unset => "unset",
        }
    }
}

/// Facts the `claude code shell` row classifies (niubash#1031). Gathered
/// from the filesystem in `run_doctor`; the classifier over them is pure
/// so the row wording is unit-testable without a Claude Code install.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaudeShellFacts {
    /// Where the value came from.
    source: ClaudeShellValueSource,
    /// The raw value of `CLAUDE_CODE_GIT_BASH_PATH`, if set anywhere.
    value: Option<String>,
    /// The value names an existing file.
    names_file: bool,
    /// The value's file name is one Claude Code accepts
    /// (`bash.exe`/`sh.exe`/`bash`/`sh`).
    accepted_name: bool,
    /// The value names niu — the same file or a byte-identical copy of
    /// `niu.exe`, a byte copy of the staged forwarding-shim payload
    /// (which re-execs niu), or a value the behavior probe confirmed
    /// runs niu.
    names_niu: bool,
    /// Git for Windows is installed, so Claude Code's Program Files
    /// probe (1) runs before the git on PATH.
    git_for_windows_present: bool,
    /// The `git` on PATH resolves to a `bin\bash.exe` that is niu.
    path_git_bash_is_niu: bool,
    /// The niu `bash.exe` to suggest when the value is missing or wrong
    /// (`winuxcmd\bin\bash.exe` beside this install).
    niu_bash_path: Option<PathBuf>,
    /// The Git Bash path Claude Code falls back to, for the advice text.
    git_bash_path: Option<PathBuf>,
}

/// An unrendered template/expansion token inside a
/// `CLAUDE_CODE_GIT_BASH_PATH` value (niubash#1031): the host reads the
/// value literally, so such a value names no file. Returns a short
/// description of the token shape, if any.
fn claude_shell_unrendered_token(value: &str) -> Option<&'static str> {
    if value.contains("@@") {
        Some("an unrendered template token (`@@...@@`)")
    } else if value.contains("{{") {
        Some("an unrendered template token (`{{...}}`)")
    } else if value.starts_with('~') {
        Some("a leading `~`, never expanded by the host")
    } else if value.contains('$') {
        Some("a `$` expansion the host reads literally")
    } else if value.contains('%') {
        Some("a `%...%` expansion the host reads literally")
    } else {
        None
    }
}

/// What Claude Code runs when the explicit variable does not land on
/// niu (niubash#1031): Git Bash when probe (1) finds it, niu via the
/// git on PATH when that git's bash is niu, another bash or PowerShell
/// otherwise.
fn claude_shell_fallback(facts: &ClaudeShellFacts) -> String {
    if facts.git_for_windows_present {
        match &facts.git_bash_path {
            Some(path) => format!(
                "Git Bash ({}; MSYS argv rules: `\\\\` collapses to `\\`, `/`, `/c/...` and /dev/* are path-converted)",
                display(path)
            ),
            None => "Git Bash (MSYS argv rules: `\\\\` collapses to `\\`, `/`, `/c/...` and /dev/* are path-converted)".to_string(),
        }
    } else if facts.path_git_bash_is_niu {
        "niu via the git on PATH".to_string()
    } else {
        "another bash, or PowerShell".to_string()
    }
}

/// Classify the bash Claude Code will run (niubash#1031). Pure over the
/// gathered facts so the row wording is unit-testable on any host.
/// Returns `None` off Windows: `CLAUDE_CODE_GIT_BASH_PATH` is a
/// Windows-only variable, so the row never prints there.
fn claude_shell_row(facts: &ClaudeShellFacts, platform: Platform) -> Option<(RowTone, String)> {
    if !platform.checks_windows_command_layer() {
        return None;
    }
    let label = " claude code shell   ";
    // The suggested path is pasted into settings.json, so it stays in
    // native backslash form — never through `display()`, which renders
    // forward slashes.
    let suggest = facts
        .niu_bash_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| r"...\winuxcmd\bin\bash.exe".to_string());
    let fix = format!("set env.CLAUDE_CODE_GIT_BASH_PATH in ~/.claude/settings.json to {suggest}");
    let row = match (&facts.value, facts.source) {
        (Some(value), source) if !facts.names_file => {
            let token = claude_shell_unrendered_token(value)
                .map(|t| format!(" ({t}: the host reads it literally)"))
                .unwrap_or_default();
            (
                RowTone::Warn,
                format!(
                    "{label}CLAUDE_CODE_GIT_BASH_PATH from {} = {value} names no file{token} — Claude Code ignores it and runs {} — {fix}",
                    source.label(),
                    claude_shell_fallback(facts)
                ),
            )
        }
        (Some(value), source) if !facts.accepted_name => (
            RowTone::Warn,
            format!(
                "{label}CLAUDE_CODE_GIT_BASH_PATH from {} = {value} — Claude Code only accepts bash.exe/sh.exe/bash/sh, so it is ignored and runs {} — {fix}",
                source.label(),
                claude_shell_fallback(facts)
            ),
        ),
        (Some(value), source) if facts.names_niu => (
            RowTone::Ok,
            format!(
                "{label}niu ({value}, via CLAUDE_CODE_GIT_BASH_PATH from {})",
                source.label()
            ),
        ),
        (Some(value), source) => (
            RowTone::Info,
            format!(
                "{label}{value} (via CLAUDE_CODE_GIT_BASH_PATH from {}) — Claude Code runs that bash, not niu",
                source.label()
            ),
        ),
        (None, _) => {
            if facts.git_for_windows_present {
                (
                    RowTone::Warn,
                    format!(
                        "{label}CLAUDE_CODE_GIT_BASH_PATH is unset — Git for Windows is installed, so Claude Code's Program Files probe wins over the git on PATH and runs {} — {fix}",
                        claude_shell_fallback(facts)
                    ),
                )
            } else if facts.path_git_bash_is_niu {
                (
                    RowTone::Ok,
                    format!(
                        "{label}unset — the git on PATH resolves to niu ({suggest}), so Claude Code runs niu"
                    ),
                )
            } else {
                (
                    RowTone::Info,
                    format!(
                        "{label}unset and no Git for Windows detected — Claude Code falls back to the git on PATH or PowerShell; {fix} for explicit control"
                    ),
                )
            }
        }
    };
    Some(row)
}

/// Claude Code's config dir (niubash#1031): `CLAUDE_CONFIG_DIR` wins
/// over `~/.claude`. `None` when neither names an existing directory —
/// the row stays hidden where the host never runs.
fn claude_config_dir(home: &Path) -> Option<PathBuf> {
    let override_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    claude_config_dir_with_override(home, override_dir.as_deref())
}

fn claude_config_dir_with_override(home: &Path, env_override: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = env_override {
        if dir.is_dir() {
            return Some(dir.to_path_buf());
        }
    }
    let dir = home.join(".claude");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Read `CLAUDE_CODE_GIT_BASH_PATH` from `settings.json`'s `env` table
/// (niubash#1031). `None` when the file is missing, unparsable, or the
/// key is absent — the caller falls back to the process environment.
fn claude_shell_value_from_settings(config_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_dir.join("settings.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    parsed
        .get("env")?
        .get("CLAUDE_CODE_GIT_BASH_PATH")?
        .as_str()
        .map(|s| s.to_string())
}

/// Whether `path` is niu (niubash#1031): the same file as a known niu
/// binary, or a byte-identical copy of one. The canonical-path compare
/// covers the same install; the byte compare covers hardlinked faces
/// (`winuxcmd\bin\bash.exe` is niu under another name) and copies.
fn claude_shell_names_niu(path: &Path, niu_binaries: &[PathBuf]) -> bool {
    niu_binaries
        .iter()
        .any(|niu| same_executable(path, niu) || same_file_bytes(path, niu))
}

/// Byte-identical copy test (niubash#1031): same length and same bytes.
/// A size mismatch short-circuits, so a foreign bash is rejected
/// without reading either file.
fn same_file_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(ba), Ok(bb)) = (std::fs::read(a), std::fs::read(b)) else {
        return false;
    };
    ba == bb
}

/// Git for Windows as Claude Code's probe (1) sees it (niubash#1031):
/// `bash.exe` under the hardcoded `C:\Program Files\Git` or
/// `C:\Program Files (x86)\Git`. Returns the bash path that exists.
fn git_for_windows_bash() -> Option<PathBuf> {
    [
        PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// The `bash.exe` the `git` on PATH resolves to (niubash#1031): beside
/// the git binary, or under `..\bin` when git lives in `cmd`. `None`
/// when no such bash exists.
fn path_git_bash() -> Option<PathBuf> {
    let git = find_exe_on_path("git")?;
    let dir = git.parent()?;
    let beside = dir.join("bash.exe");
    if beside.is_file() {
        return Some(beside);
    }
    let up_bin = dir.parent()?.join("bin").join("bash.exe");
    if up_bin.is_file() {
        return Some(up_bin);
    }
    None
}

/// Whether `path` is a niu face via the forwarding shim (niubash#1031):
/// byte-identical to the staged shim payload of this installation. The
/// shim re-execs niu, so the session it starts IS niu even though its
/// bytes differ from niu.exe (niubash#464: faces fall back to a byte copy
/// of the payload where a hardlink cannot exist).
fn claude_shell_is_shim_face(path: &Path, shim_payload: Option<&Path>) -> bool {
    shim_payload.is_some_and(|payload| same_file_bytes(path, payload))
}

/// Whether `path` runs niu (niubash#1031): execute it with `-c` printing
/// `$NIU_SHELL` and see whether it answers with a niu path. Only used
/// when the static identity checks already failed, so a foreign bash is
/// probed at most once per doctor run — a shim face from another build
/// (bytes unlike both niu.exe and the staged payload) is still niu by
/// behavior. `NIU_SHELL` is scrubbed from the child's environment so an
/// inherited value cannot fake a niu answer; a hang is killed after the
/// timeout.
fn probe_shell_reports_niu(path: &Path) -> bool {
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let mut child = match std::process::Command::new(path)
        .arg("-c")
        .arg("printf '%s' \"$NIU_SHELL\"")
        .env_remove("NIU_SHELL")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    // Drain stdout on its own thread so a chatty value cannot fill the
    // pipe and stall the child; the caller keeps the handle so a hang
    // can be killed on timeout.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout.take() {
            use std::io::Read;
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break None,
        }
    };
    let output = reader.join().unwrap_or_default();
    if !status.is_some_and(|s| s.success()) {
        return false;
    }
    let text = String::from_utf8_lossy(&output);
    let text = text.trim();
    !text.is_empty()
        && std::path::Path::new(text)
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("niu.exe") || n.eq_ignore_ascii_case("niu"))
}

/// The niu `bash.exe` to suggest (niubash#1031): `bin\bash.exe` beside
/// the winuxcmd tree of this install.
fn niu_bash_path() -> Option<PathBuf> {
    let exe = crate::winuxcmd::find_winuxcmd()?;
    let root = crate::winuxcmd::installation_root(&exe);
    let bash = root.join("bin").join("bash.exe");
    if bash.is_file() {
        Some(bash)
    } else {
        None
    }
}

/// Gather the facts the `claude code shell` row classifies (niubash#1031).
/// `None` when there is no Claude Code config dir — the row stays hidden
/// where the host never runs.
fn gather_claude_shell_facts(home: &Path) -> Option<ClaudeShellFacts> {
    let config_dir = claude_config_dir(home)?;
    let (source, value) = match claude_shell_value_from_settings(&config_dir) {
        Some(value) => (ClaudeShellValueSource::SettingsJson, Some(value)),
        None => match std::env::var("CLAUDE_CODE_GIT_BASH_PATH") {
            Ok(value) if !value.is_empty() => (ClaudeShellValueSource::ProcessEnv, Some(value)),
            _ => (ClaudeShellValueSource::Unset, None),
        },
    };
    let value_path = value.as_ref().map(PathBuf::from);
    let names_file = value_path.as_ref().is_some_and(|p| p.is_file());
    let accepted_name = value_path.as_ref().is_some_and(|p| {
        p.file_name().is_some_and(|n| {
            matches!(
                n.to_string_lossy().to_ascii_lowercase().as_str(),
                "bash.exe" | "sh.exe" | "bash" | "sh"
            )
        })
    });
    // Known niu binaries: this process, `niu` on PATH, and the installed
    // niu.exe beside the winuxcmd tree (the installed face — a release
    // install and a debug build of the same source are different bytes,
    // so the running binary alone cannot recognise the installed face).
    // Plus the staged forwarding-shim payload: where a hardlink cannot
    // exist the faces are byte copies of it, and the shim re-execs niu,
    // so a shim face IS niu even though its bytes differ from niu.exe.
    let mut niu_binaries: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        niu_binaries.push(exe);
    }
    if let Some(niu) = find_exe_on_path("niu") {
        niu_binaries.push(niu);
    }
    let mut shim_payload: Option<PathBuf> = None;
    if let Some(exe) = crate::winuxcmd::find_winuxcmd() {
        let install_root = crate::shell_links::find_installation_root(&exe);
        let installed = install_root.join("niu.exe");
        if installed.is_file() {
            niu_binaries.push(installed);
        }
        let payload = crate::shell_links::shim_payload_path(&install_root);
        if payload.is_file() {
            shim_payload = Some(payload);
        }
    }
    let niu_bash = niu_bash_path();
    let mut names_niu = names_file
        && value_path.as_ref().is_some_and(|p| {
            claude_shell_names_niu(p, &niu_binaries)
                || claude_shell_is_shim_face(p, shim_payload.as_deref())
        });
    // Last resort: a shim face from another build is byte-unlike both
    // niu.exe and the staged payload, so ask the value itself — run it
    // and see whether it reports niu. Only for an existing file under an
    // accepted name that the static checks did not recognise.
    if !names_niu && names_file && accepted_name {
        if let Some(p) = value_path.as_ref() {
            names_niu = probe_shell_reports_niu(p);
        }
    }
    let git_bash = git_for_windows_bash();
    let path_git_bash = path_git_bash();
    let path_git_bash_is_niu = path_git_bash
        .as_ref()
        .is_some_and(|p| claude_shell_names_niu(p, &niu_binaries));
    Some(ClaudeShellFacts {
        source,
        value,
        names_file,
        accepted_name,
        names_niu,
        git_for_windows_present: git_bash.is_some(),
        path_git_bash_is_niu,
        niu_bash_path: niu_bash,
        git_bash_path: git_bash,
    })
}

/// Windows OS build number, read from the registry
/// (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\CurrentBuildNumber`).
/// `None` when unreadable — callers treat an unknown build as below any
/// floor rather than guessing.
#[cfg(windows)]
fn windows_build_number() -> Option<u32> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let mut key: HKEY = std::ptr::null_mut();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            to_wide(KEY).as_ptr(),
            0,
            KEY_READ,
            &mut key,
        )
    };
    if opened != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u16; 64];
    let mut len = (buf.len() * std::mem::size_of::<u16>()) as u32;
    let mut ty = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            to_wide("CurrentBuildNumber").as_ptr(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr() as *mut u8,
            &mut len,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS || ty != REG_SZ {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..chars]);
    text.trim_end_matches('\0').trim().parse().ok()
}

#[cfg(not(windows))]
fn windows_build_number() -> Option<u32> {
    None
}

/// What `command -v niu` resolves to on this machine. `None` = nothing
/// on PATH answers to `niu`.
fn find_niu_on_path() -> Option<PathBuf> {
    find_exe_on_path("niu")
}

/// The first PATH entry that carries an executable `name` (`.exe`/
/// `.bat`/`.cmd`/`.com`, plus the extension-less form some POSIX-side
/// PATHs expose). `None` = nothing on PATH answers to `name`.
fn find_exe_on_path(name: &str) -> Option<PathBuf> {
    const EXTS: &[&str] = &[".exe", ".bat", ".cmd", ".com", ""];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| {
            EXTS.iter()
                .map(|ext| dir.join(format!("{name}{ext}")))
                .find(|candidate| candidate.is_file())
        })
}

/// Loose same-file test for the doctor row: canonicalize when possible
/// (resolves case, `\\?\` prefixes and symlinks), else fall back to a
/// case-insensitive textual compare (Windows paths are case-insensitive).
fn same_executable(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canon(a), canon(b));
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// niubash#495: under MSIX the PATH copy is the AppExecutionAlias stub —
/// `%LOCALAPPDATA%\Microsoft\WindowsApps\niu.exe`, a reparse point that
/// never canonicalizes to the package's real `niu.exe` under
/// `C:\Program Files\WindowsApps`. Both names start this exact package, so
/// the shadow warning must not fire on them.
fn same_packaged_origin(path_niu: &Path) -> bool {
    crate::msix::is_packaged() && crate::msix::is_app_execution_alias_path(path_niu)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// niubash#810: the violation row names the file, the shape, and the
    /// repair command with the backup promise.
    #[test]
    fn bundled_shim_violation_row_names_the_repair() {
        let row = bundled_shim_violation_row(
            "awk.exe",
            "standalone binary where the bundled gawk component ships a winuxcmd shim",
        );
        assert!(row.contains("bundled shim awk.exe"), "{row}");
        assert!(row.contains("standalone binary"), "{row}");
        assert!(row.contains("gawk"), "{row}");
        assert!(row.contains("--relink-bundled-shims --force"), "{row}");
        assert!(row.contains("awk.exe.bak-"), "{row}");
    }

    /// niubash#230 follow-up: the bundled-tools row names exactly what the
    /// release ships and labels it as bundled, never as a system install.
    #[test]
    fn bundled_tools_row_labels_the_release_source() {
        let row = bundled_tools_row(&["gawk", "niugit", "ripgrep", "fd"], Platform::Windows);
        assert!(row.contains("gawk niugit ripgrep fd"), "{row}");
        assert!(row.contains("shipped with the release"), "{row}");
        assert!(row.contains("not system installs"), "{row}");
    }

    #[test]
    fn bundled_tools_row_is_honest_when_empty_on_windows() {
        let row = bundled_tools_row(&[], Platform::Windows);
        assert!(row.contains("none"), "{row}");
        assert!(!row.contains("shipped"), "{row}");
    }

    // ── niubash#194: platform gating of the Windows command layer ──────────

    /// The Windows command layer (winuxcmd core, command links, shims) must
    /// be probed on Windows and skipped on unix — the skip decision is the
    /// same code path `run_doctor` branches on, exercised here with an
    /// injected platform so it holds regardless of the host OS.
    #[test]
    fn windows_command_layer_is_checked_only_on_windows() {
        assert!(Platform::Windows.checks_windows_command_layer());
        assert!(!Platform::Unix.checks_windows_command_layer());
        // The runtime default must agree with the compile-time target.
        assert_eq!(
            Platform::current().checks_windows_command_layer(),
            cfg!(windows)
        );
    }

    /// On unix the skipped rows must read as n/a (not as a broken install),
    /// and the bundled-tools row must not imply an empty install. Asserts
    /// the exact shared constant the report prints.
    #[test]
    fn unix_skipped_rows_read_as_na_not_failure() {
        assert!(WINDOWS_COMPONENT_NA.contains("n/a"));
        assert!(WINDOWS_COMPONENT_NA.contains("Windows component"));
        assert!(!WINDOWS_COMPONENT_NA.contains("reinstall"));
        assert!(!WINDOWS_COMPONENT_NA.contains("not found"));

        // And the bundled row on unix is explicitly n/a about the layout.
        let bundled = bundled_tools_row(&[], Platform::Unix);
        assert!(bundled.contains("n/a"), "{bundled}");
        assert!(bundled.contains("Windows release layout"), "{bundled}");
        assert!(!bundled.contains("reinstall"), "{bundled}");
    }

    /// The header names the platform so users can tell which checks apply.
    #[test]
    fn platform_labels_are_distinct() {
        assert_ne!(Platform::Windows.label(), Platform::Unix.label());
        assert_eq!(Platform::Windows.label(), "windows");
        assert_eq!(Platform::Unix.label(), "unix");
    }

    // ── niubash#429 (decision A): the elevation-channel advisory ──────────

    /// Microsoft sudo counts only at or above the 24H2 build floor, and
    /// only with `sudo.exe` actually on PATH; an unknown build never
    /// counts as new enough.
    #[test]
    fn ms_sudo_needs_the_build_floor_and_the_binary() {
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD), true, false),
            ElevationProvider::MsSudo
        );
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD - 1), true, false),
            ElevationProvider::None
        );
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD), false, false),
            ElevationProvider::None
        );
        assert_eq!(
            elevation_provider(None, true, false),
            ElevationProvider::None
        );
    }

    /// gsudo is the fallback channel; Microsoft sudo (the in-box one)
    /// wins when both are present.
    #[test]
    fn gsudo_is_the_fallback_channel() {
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD - 1), true, true),
            ElevationProvider::Gsudo
        );
        assert_eq!(
            elevation_provider(None, false, true),
            ElevationProvider::Gsudo
        );
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD), true, true),
            ElevationProvider::MsSudo
        );
        assert_eq!(
            elevation_provider(None, false, false),
            ElevationProvider::None
        );
    }

    /// On Windows the row names the detected provider, and the
    /// no-provider row is a warning that names both install routes and
    /// the opt-in builtin variable.
    #[test]
    fn elevation_row_reports_the_windows_channel() {
        let (tone, row) = elevation_channel_row(ElevationProvider::MsSudo, Platform::Windows);
        assert_eq!(tone, RowTone::Ok);
        assert!(row.contains("Microsoft sudo"), "{row}");

        let (tone, row) = elevation_channel_row(ElevationProvider::Gsudo, Platform::Windows);
        assert_eq!(tone, RowTone::Ok);
        assert!(row.contains("gsudo"), "{row}");

        let (tone, row) = elevation_channel_row(ElevationProvider::None, Platform::Windows);
        assert_eq!(tone, RowTone::Warn);
        assert!(row.contains("none found"), "{row}");
        assert!(row.contains("26045"), "{row}");
        assert!(row.contains("gsudo"), "{row}");
        assert!(row.contains("NIU_ENABLE_RUBASH_SUDO"), "{row}");
    }

    /// Non-Windows platforms never report a missing provider: the layer
    /// is a Windows component and the row reads n/a whatever the probes
    /// would have said.
    #[test]
    fn elevation_row_is_na_off_windows() {
        for provider in [
            ElevationProvider::MsSudo,
            ElevationProvider::Gsudo,
            ElevationProvider::None,
        ] {
            let (tone, row) = elevation_channel_row(provider, Platform::Unix);
            assert_eq!(tone, RowTone::Info);
            assert!(row.contains(WINDOWS_COMPONENT_NA), "{row}");
            assert!(!row.contains("none found"), "{row}");
        }
    }

    /// The build probe is compile-time honest: off Windows it reports
    /// unknown rather than inventing a build.
    #[test]
    fn windows_build_number_is_unknown_off_windows() {
        if !cfg!(windows) {
            assert_eq!(windows_build_number(), None);
        }
    }

    // ── niubash#1031: the `claude code shell` row ─────────────────────────

    /// Base facts for the classifier tests: a settings.json value, no
    /// Git for Windows, a known niu bash path to suggest.
    fn claude_facts() -> ClaudeShellFacts {
        ClaudeShellFacts {
            source: ClaudeShellValueSource::SettingsJson,
            value: None,
            names_file: false,
            accepted_name: false,
            names_niu: false,
            git_for_windows_present: false,
            path_git_bash_is_niu: false,
            niu_bash_path: Some(PathBuf::from(
                r"C:\Users\test\AppData\Local\Programs\Niubash\winuxcmd\bin\bash.exe",
            )),
            git_bash_path: Some(PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")),
        }
    }

    /// The issue's own case: a settings.json value naming no file, with
    /// an unrendered template token. ADVICE names the token, the fallback
    /// the host will take, and the exact niu path to set.
    #[test]
    fn claude_shell_dangling_value_is_advice_with_token_note() {
        let mut facts = claude_facts();
        facts.value = Some(
            r"@@windows.home@@\AppData\Local\Programs\Niubash\winuxcmd\bin\bash.exe".to_string(),
        );
        facts.git_for_windows_present = true;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Warn);
        assert!(row.contains("names no file"), "{row}");
        assert!(row.contains("unrendered template token"), "{row}");
        assert!(row.contains("Git Bash"), "{row}");
        assert!(row.contains("MSYS argv rules"), "{row}");
        assert!(row.contains("set env.CLAUDE_CODE_GIT_BASH_PATH"), "{row}");
        assert!(row.contains(r"winuxcmd\bin\bash.exe"), "{row}");
    }

    /// A value naming an existing file under a rejected name (`niu.exe`)
    /// is ignored by the host just as silently as a dangling value.
    #[test]
    fn claude_shell_rejected_name_is_advice() {
        let mut facts = claude_facts();
        facts.value = Some(r"C:\Users\test\niu.exe".to_string());
        facts.names_file = true;
        facts.git_for_windows_present = true;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Warn);
        assert!(
            row.contains("only accepts bash.exe/sh.exe/bash/sh"),
            "{row}"
        );
        assert!(row.contains("Git Bash"), "{row}");
    }

    /// A value naming niu is OK and says so in one line.
    #[test]
    fn claude_shell_niu_value_is_ok() {
        let mut facts = claude_facts();
        facts.value =
            Some(r"C:\Users\test\AppData\Local\Programs\Niubash\winuxcmd\bin\bash.exe".to_string());
        facts.names_file = true;
        facts.accepted_name = true;
        facts.names_niu = true;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Ok);
        assert!(row.contains("niu ("), "{row}");
        assert!(row.contains("settings.json env"), "{row}");
    }

    /// A value naming another bash is INFO: the host runs it, not niu.
    #[test]
    fn claude_shell_other_bash_is_info() {
        let mut facts = claude_facts();
        facts.value = Some(r"C:\Program Files\Git\bin\bash.exe".to_string());
        facts.names_file = true;
        facts.accepted_name = true;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Info);
        assert!(row.contains("not niu"), "{row}");
    }

    /// Unset with Git for Windows installed: probe (1) wins over the git
    /// on PATH, so the row is ADVICE even though nothing is mis-set.
    #[test]
    fn claude_shell_unset_with_git_for_windows_is_advice() {
        let mut facts = claude_facts();
        facts.source = ClaudeShellValueSource::Unset;
        facts.git_for_windows_present = true;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Warn);
        assert!(row.contains("unset"), "{row}");
        assert!(row.contains("Program Files probe wins"), "{row}");
        assert!(row.contains("Git Bash"), "{row}");
    }

    /// Unset and the PATH git's bash is niu: the probe lands on niu, OK.
    #[test]
    fn claude_shell_unset_with_path_git_niu_is_ok() {
        let mut facts = claude_facts();
        facts.source = ClaudeShellValueSource::Unset;
        facts.path_git_bash_is_niu = true;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Ok);
        assert!(row.contains("git on PATH resolves to niu"), "{row}");
    }

    /// Unset with neither Git for Windows nor a niu git on PATH: INFO,
    /// with the explicit-control fix.
    #[test]
    fn claude_shell_unset_without_git_is_info() {
        let mut facts = claude_facts();
        facts.source = ClaudeShellValueSource::Unset;
        let (tone, row) = claude_shell_row(&facts, Platform::Windows).unwrap();
        assert_eq!(tone, RowTone::Info);
        assert!(row.contains("no Git for Windows detected"), "{row}");
        assert!(row.contains("set env.CLAUDE_CODE_GIT_BASH_PATH"), "{row}");
    }

    /// The row is Windows-only: no facts print it on unix.
    #[test]
    fn claude_shell_row_hidden_off_windows() {
        let mut facts = claude_facts();
        facts.value = Some(r"C:\nope\bash.exe".to_string());
        assert!(claude_shell_row(&facts, Platform::Unix).is_none());
    }

    /// The token detector names each unrendered shape and stays quiet
    /// on a plain path.
    #[test]
    fn claude_shell_token_detector_names_the_shapes() {
        assert!(claude_shell_unrendered_token(r"@@windows.home@@\bash.exe")
            .is_some_and(|t| t.contains("@@")));
        assert!(
            claude_shell_unrendered_token(r"{{home}}\bash.exe").is_some_and(|t| t.contains("{{"))
        );
        assert!(claude_shell_unrendered_token(r"~\bash.exe").is_some_and(|t| t.contains("~")));
        assert!(
            claude_shell_unrendered_token(r"C:\$USER\bash.exe").is_some_and(|t| t.contains("$"))
        );
        assert!(claude_shell_unrendered_token(r"%USERPROFILE%\bash.exe")
            .is_some_and(|t| t.contains("%")));
        assert_eq!(
            claude_shell_unrendered_token(r"C:\Users\test\bash.exe"),
            None
        );
    }

    /// The config dir resolves from `~/.claude` and honours the env
    /// override; a missing dir hides the row.
    #[test]
    fn claude_shell_config_dir_resolves_from_home_and_override() {
        let base = std::env::temp_dir().join(format!("niu-doctor-1031-{}", std::process::id()));
        let home = base.join("home");
        let claude = home.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        assert_eq!(
            claude_config_dir_with_override(&home, None),
            Some(claude.clone())
        );
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(
            claude_config_dir_with_override(&home, Some(&other)),
            Some(other)
        );
        assert_eq!(
            claude_config_dir_with_override(&base.join("nohome"), None),
            None
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// The settings.json read takes `env.CLAUDE_CODE_GIT_BASH_PATH` and
    /// stays silent on a missing file, bad JSON, or a missing key.
    #[test]
    fn claude_shell_value_reads_settings_json_env() {
        let dir =
            std::env::temp_dir().join(format!("niu-doctor-1031-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(
            &settings,
            r#"{"env": {"CLAUDE_CODE_GIT_BASH_PATH": "C:\\x\\bash.exe"}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_shell_value_from_settings(&dir).as_deref(),
            Some(r"C:\x\bash.exe")
        );
        std::fs::write(&settings, r#"{"env": {}}"#).unwrap();
        assert_eq!(claude_shell_value_from_settings(&dir), None);
        std::fs::write(&settings, r#"{"env": {"CLAUDE_CODE_GIT_BASH_PATH": 42}}"#).unwrap();
        assert_eq!(claude_shell_value_from_settings(&dir), None);
        std::fs::write(&settings, "not json").unwrap();
        assert_eq!(claude_shell_value_from_settings(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The byte compare accepts a copy of a known niu binary and rejects
    /// a same-name file with different bytes; a size mismatch never
    /// reads the files.
    #[test]
    fn claude_shell_names_niu_accepts_byte_identical_copy() {
        let dir =
            std::env::temp_dir().join(format!("niu-doctor-1031-bytes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let niu = dir.join("niu.exe");
        let copy = dir.join("bash.exe");
        let other = dir.join("other-bash.exe");
        std::fs::write(&niu, b"fake niu bytes").unwrap();
        std::fs::write(&copy, b"fake niu bytes").unwrap();
        std::fs::write(&other, b"different bash bytes!").unwrap();
        assert!(claude_shell_names_niu(&copy, &[niu]));
        assert!(!claude_shell_names_niu(&other, &[niu]));
        assert!(!claude_shell_names_niu(&dir.join("missing.exe"), &[niu]));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A byte copy of the staged shim payload counts as niu: the shim
    /// re-execs niu, so the session it starts is niu.
    #[test]
    fn claude_shell_shim_face_counts_as_niu() {
        let dir = std::env::temp_dir().join(format!("niu-doctor-1031-shim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let payload = dir.join("niu-shim.exe");
        let face = dir.join("bash.exe");
        let other = dir.join("other.exe");
        std::fs::write(&payload, b"fake shim bytes").unwrap();
        std::fs::write(&face, b"fake shim bytes").unwrap();
        std::fs::write(&other, b"something else").unwrap();
        assert!(claude_shell_is_shim_face(&face, Some(&payload)));
        assert!(!claude_shell_is_shim_face(&other, Some(&payload)));
        assert!(!claude_shell_is_shim_face(&face, None));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The behavior probe stays quiet on a missing file and on an
    /// existing file that is not a shell (the positive face — a shim
    /// reporting niu — is pinned by the integration stem, which owns a
    /// real built shim).
    #[test]
    fn claude_shell_probe_is_quiet_off_shells() {
        assert!(!probe_shell_reports_niu(Path::new(
            "C:\\definitely\\not\\here\\bash.exe"
        )));
        let dir =
            std::env::temp_dir().join(format!("niu-doctor-1031-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("bash.exe");
        std::fs::write(&text, b"not a shell at all").unwrap();
        assert!(!probe_shell_reports_niu(&text));
        std::fs::remove_dir_all(&dir).ok();
    }
}
