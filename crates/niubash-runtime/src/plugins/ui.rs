//! `niu plugin ui` — the menu-level plugin UI (wt47/lazystudy task ⑤).
//!
//! This is a *view over verbs*, not a separate world (study §10.4): every
//! action the menu offers calls the same `plugins::` functions the CLI
//! verbs call, through one dispatch ([`apply_verb`]); zero UI-only logic.
//! The information architecture is the portable part of lazy.nvim's view
//! (study §4):
//!
//! - **Command table, not key handling** — [`UI_VERBS`] is one data table;
//!   the verb menus and the help footer are generated from it, so UI and
//!   help cannot drift (`lazy/view/config.lua:42 M.commands`).
//! - **Sections by lifecycle state, attention-first** — untrusted sources
//!   come before ready ones, ready before the not-yet-installed recipe
//!   index (`lazy/view/sections.lua:16-120`).
//! - **Row anatomy** `[state] id — hint` (`lazy/view/render.lua:444`).
//! - **Per-item verbs + global verbs** (lazy's `key_plugin` vs `key`).
//!
//! Rendering is menu-level over [`crate::interactive_menu`] — no float, no
//! ncurses; async task streaming stays out of the MVP (study §4 verdict).

use anyhow::bail;
use std::io::IsTerminal;

use super::{distros, recipes, sources};
use crate::interactive_menu::{interactive_choice, interactive_choice_ex, Selection};
use crate::text_style;

/// One verb row of the command table. `applies` decides which row kinds
/// offer it; `verb` is the [`apply_verb`] dispatch id.
pub struct UiVerb {
    pub verb: &'static str,
    pub label: &'static str,
    /// The CLI form shown in help/preview (documentation, not dispatch).
    pub cli: &'static str,
}

/// The command table (lazy.nvim `view/config.lua:42` pattern). Help text
/// and menus are generated from this one table.
pub const UI_VERBS: &[UiVerb] = &[
    UiVerb {
        verb: "trust",
        label: "trust — review checksum and activate",
        cli: "niu plugin trust <id>",
    },
    UiVerb {
        verb: "update",
        label: "update — move this source to its ref tip",
        cli: "niu plugin update <id>",
    },
    UiVerb {
        verb: "rollback",
        label: "rollback — restore the previous source state",
        cli: "niu plugin rollback <id>",
    },
    UiVerb {
        verb: "enable",
        label: "enable — activate (PATH block / asset)",
        cli: "niu plugin enable <id>",
    },
    UiVerb {
        verb: "disable",
        label: "disable — deactivate",
        cli: "niu plugin disable <id>",
    },
    UiVerb {
        verb: "remove-source",
        label: "remove — delete the source tree and record",
        cli: "niu plugin source remove <id>",
    },
    UiVerb {
        verb: "add",
        label: "install — add through the recipe's driver",
        cli: "niu plugin add <id>",
    },
    UiVerb {
        verb: "apply",
        label: "apply — install every recipe in the collection",
        cli: "niu plugin distro apply <name>",
    },
    UiVerb {
        verb: "sync",
        label: "sync — update every source to its ref tip",
        cli: "niu plugin sync",
    },
];

fn verb(verb_id: &str) -> &'static UiVerb {
    UI_VERBS
        .iter()
        .find(|entry| entry.verb == verb_id)
        .expect("UI_VERBS covers every id used by verbs_for/apply_verb")
}

/// What kind of thing a UI row points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRowKind {
    Source {
        trusted: bool,
        degraded: bool,
    },
    Recipe {
        installable: bool,
        installed: bool,
    },
    Collection {
        builtin: bool,
    },
    /// A theme asset from a trusted source (niubash#170): highlight previews
    /// the theme's real PS1, Enter opens the activation verbs.
    Theme {
        source_id: String,
    },
}

impl UiRowKind {
    /// Which command-table verbs apply to this row (per-item verbs; lazy's
    /// `key_plugin` split).
    fn verbs(&self) -> Vec<&'static UiVerb> {
        match self {
            Self::Source { trusted, degraded } => {
                let mut verbs: Vec<&'static UiVerb> = Vec::new();
                if !trusted {
                    verbs.push(verb("trust"));
                }
                verbs.push(verb("update"));
                verbs.push(verb("rollback"));
                verbs.push(verb("disable"));
                if *degraded {
                    verbs.push(verb("remove-source"));
                }
                verbs
            }
            Self::Recipe { installable, .. } => {
                if *installable {
                    vec![verb("add")]
                } else {
                    Vec::new()
                }
            }
            Self::Collection { .. } => vec![verb("apply")],
            Self::Theme { .. } => vec![verb("enable"), verb("disable")],
        }
    }
}

/// One row of a section (`render.lua:444` anatomy: state mark + name +
/// hint).
#[derive(Debug, Clone)]
pub struct UiRow {
    pub id: String,
    pub kind: UiRowKind,
    /// State mark shown in brackets ("ok", "untrusted", "available"...).
    pub state: String,
    /// Right-side hint: one-line summary or diagnostic.
    pub hint: String,
}

impl UiRow {
    /// `[state] id — hint`, state colored attention-first.
    pub fn line(&self) -> String {
        let mark = match self.state.as_str() {
            "ready" | "active" => text_style::green(&self.state),
            "untrusted" => text_style::yellow(&self.state),
            "degraded" => text_style::red(&self.state),
            other => text_style::dim(other),
        };
        format!("[{mark}] {} \u{2014} {}", self.id, self.hint)
    }
}

/// A section of the main menu: lifecycle state first, then content
/// (`lazy/view/sections.lua` — a plugin appears in exactly one section).
pub struct UiSection {
    pub title: String,
    pub rows: Vec<UiRow>,
    /// True when this section is the global-verbs pseudo section.
    pub global: bool,
}

/// The whole inventory the UI renders (rebuilt after every action so the
/// view always shows live state — no caching, lazy's re-render loop).
pub fn inventory() -> Vec<UiSection> {
    let mut sections = Vec::new();

    // Sources, split by state machine: untrusted (needs review) first,
    // then ready; degraded rows join whichever bucket they fall in (the
    // degraded mark is carried in the state string).
    let statuses = sources::list_sources();
    let untrusted: Vec<&sources::SourceStatus> = statuses
        .iter()
        .filter(|status| status.state != "ready")
        .collect();
    let ready: Vec<&sources::SourceStatus> = statuses
        .iter()
        .filter(|status| status.state == "ready")
        .collect();
    let row_of = |status: &sources::SourceStatus| UiRow {
        id: status.record.id.clone(),
        kind: UiRowKind::Source {
            trusted: status.state == "ready",
            degraded: status.degraded,
        },
        state: if status.degraded {
            "degraded".into()
        } else {
            status.state.clone()
        },
        hint: format!("{} · {}", status.record.version, status.adapter_display),
    };
    sections.push(UiSection {
        title: "Needs review — untrusted sources".into(),
        rows: untrusted.iter().map(|status| row_of(status)).collect(),
        global: false,
    });
    sections.push(UiSection {
        title: "Installed & ready — sources".into(),
        rows: ready.iter().map(|status| row_of(status)).collect(),
        global: false,
    });

    // Themes from trusted sources (niubash#170): a browsable section whose
    // highlighted row previews the theme's real PS1 (the same
    // `theme_preview::GalleryPreviews` cache the wizard's gallery uses).
    // Attention-first placement: right after the ready sources, before the
    // recipe index — themes are the asset users pick, not something to
    // install.
    let mut themes = sources::source_theme_entries();
    themes.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    if !themes.is_empty() {
        sections.push(UiSection {
            title: "Themes — highlight to preview, Enter to activate".into(),
            rows: themes
                .into_iter()
                .map(|theme| UiRow {
                    id: theme.name,
                    kind: UiRowKind::Theme {
                        source_id: theme.source_id,
                    },
                    state: "theme".into(),
                    hint: theme.adapter_display,
                })
                .collect(),
            global: false,
        });
    }

    // Recipes not installed yet, grouped by category (the mason-style
    // index; only installable or notable rows are listed to keep the menu
    // a menu — the full 498-row index stays behind `niu plugin recipe
    // list`). Categories in the order users think in: managers, prompts,
    // plugins, completions, themes, aliases. Executable-tool rows ride
    // here too: `add` prints their package-manager recommendation
    // (download retraction 2026-10-04).
    let installed_sources: Vec<String> = sources::read_source_registry()
        .into_iter()
        .map(|record| record.id)
        .collect();
    for (category, title) in [
        (
            recipes::RecipeCategory::Manager,
            "Recipes — managers (not installed)",
        ),
        (
            recipes::RecipeCategory::Prompt,
            "Recipes — prompts (not installed)",
        ),
        (
            recipes::RecipeCategory::Plugin,
            "Recipes — plugins (not installed)",
        ),
    ] {
        let rows: Vec<UiRow> = recipes::recipes()
            .iter()
            .filter(|recipe| recipe.category == category)
            .filter(|recipe| !installed_sources.contains(&recipe.id))
            .filter(|recipe| recipe.driver.is_some())
            .map(|recipe| UiRow {
                id: recipe.id.clone(),
                kind: UiRowKind::Recipe {
                    installable: true,
                    installed: false,
                },
                state: "available".into(),
                hint: recipe.summary.clone(),
            })
            .collect();
        if !rows.is_empty() {
            sections.push(UiSection {
                title: title.into(),
                rows,
                global: false,
            });
        }
    }

    // Collections: built-in + imported.
    sections.push(UiSection {
        title: "Collections — apply installs, never auto-trusts".into(),
        rows: distros::collections()
            .into_iter()
            .map(|listing| UiRow {
                id: listing.collection.name.clone(),
                kind: UiRowKind::Collection {
                    builtin: listing.origin == distros::CollectionOrigin::Builtin,
                },
                state: match listing.origin {
                    distros::CollectionOrigin::Builtin => "built-in".into(),
                    distros::CollectionOrigin::Imported { .. } => "imported".into(),
                },
                hint: listing.collection.description.clone(),
            })
            .collect(),
        global: false,
    });

    // Global verbs (lazy's `key` split: sync acts on everything).
    sections.push(UiSection {
        title: "Global verbs".into(),
        rows: vec![UiRow {
            id: "sync".into(),
            kind: UiRowKind::Collection { builtin: false },
            state: "global".into(),
            hint: "update every installed source to its ref tip".into(),
        }],
        global: true,
    });

    sections
}

/// Dispatch one verb for one target. This is the *only* place the UI
/// mutates anything, and it calls exactly the functions the CLI verbs
/// call — the menu is a view, not an engine (study §10.4).
pub fn apply_verb(verb_id: &str, target: &str) -> anyhow::Result<String> {
    match verb_id {
        "trust" => {
            sources::verify_source(target)?;
            let trusted = sources::trust_source(target)?;
            Ok(format!(
                "trusted '{}' — its themes/assets join the catalog; restart niu to load them",
                trusted.id
            ))
        }
        "update" => {
            let summary = sources::update_source(target, sources::SourceInstallRequest::default())?;
            Ok(format!(
                "updated '{}' to {} ({})",
                summary.id, summary.version, summary.checksum_sha256
            ))
        }
        "rollback" => {
            let summary = sources::rollback_source(target)?;
            Ok(format!(
                "rolled back '{}' to {}",
                summary.id, summary.version
            ))
        }
        "enable" => {
            let outcome = recipes::enable(target)?;
            Ok(outcome.summary)
        }
        "disable" => {
            let outcome = recipes::disable(target)?;
            Ok(outcome.summary)
        }
        "remove-source" => {
            let path = sources::remove_source(target)?;
            Ok(format!("removed source '{target}' ({})", path.display()))
        }
        "add" => {
            let report = recipes::install(target)?;
            Ok(format!(
                "{}; next: {}",
                report.summary,
                report.next.join(" ")
            ))
        }
        "apply" => {
            let outcome = distros::apply(target)?;
            let mut lines = vec![format!("collection '{}':", outcome.name)];
            lines.extend(
                outcome
                    .reports
                    .iter()
                    .map(|report| format!("  - {}", report.summary)),
            );
            lines.extend(
                outcome
                    .failures
                    .iter()
                    .map(|(recipe, error)| format!("  failed {recipe}: {error}")),
            );
            Ok(lines.join("\n"))
        }
        "sync" => {
            let outcomes = sources::sync_sources();
            if outcomes.is_empty() {
                return Ok("(no sources installed)".into());
            }
            Ok(outcomes
                .iter()
                .map(|outcome| format!("{} {}", outcome.id, outcome.detail))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        other => bail!("no such UI verb '{other}'"),
    }
}

/// The help footer, generated from the command table (one table = help
/// and menu can never drift, `view/render.lua:185 M:help`).
pub fn help_footer() -> String {
    let forms: Vec<&str> = UI_VERBS.iter().map(|entry| entry.cli).collect();
    format!("CLI equivalents: {}", forms.join(" · "))
}

/// `niu plugin ui` entry: the menu loop. Menu-level, not full-screen: each
/// level is one `interactive_menu` block; Esc backs out a level.
pub fn run_ui() -> anyhow::Result<()> {
    // The menu reads console keys through crossterm; a piped stdin would
    // block forever on the first `event::read`. Refuse up front,
    // health-style (name the object and the alternative, study §10.1).
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        let forms: Vec<&str> = UI_VERBS.iter().map(|entry| entry.cli).collect();
        anyhow::bail!(
            "niu plugin ui needs an interactive terminal; \
             the same verbs exist on the CLI: {}",
            forms.join(" · ")
        );
    }
    loop {
        let sections = inventory();
        let mut options: Vec<String> = sections
            .iter()
            .map(|section| {
                if section.rows.is_empty() {
                    format!("{}  (none)", section.title)
                } else {
                    format!("{}  [{}]", section.title, section.rows.len())
                }
            })
            .collect();
        options.push("Quit".into());
        let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
        let pick = interactive_choice("Plugins — pick a section", &option_refs, 0, &help_footer());
        let index = match pick {
            Selection::Confirmed(index) => index,
            Selection::UseDefault | Selection::Abort => return Ok(()),
        };
        if index >= sections.len() {
            return Ok(());
        }
        // Sections with no rows still open (the empty menu explains itself
        // and offers nothing destructive).
        open_section(&sections[index])?;
    }
}

/// One section submenu: rows, then per-row verbs, then back. Returns on
/// Esc/back; actions re-enter the loop with fresh state. The theme section
/// paints a live preview of the highlighted theme below the menu
/// (niubash#170) through the shared [`theme_preview::GalleryPreviews`] cache.
fn open_section(section: &UiSection) -> anyhow::Result<()> {
    use crate::interactive_menu::PreviewPhase;
    use crate::plugins::theme_preview::GalleryPreviews;

    if section.global {
        // The global section is a verb list, not a row list.
        return run_global_verbs();
    }
    loop {
        let rows = &section.rows;
        if rows.is_empty() {
            println!();
            println!("  (nothing here — this section is empty)");
            println!("  install something: niu plugin add <id>, or niu plugin discover");
            return Ok(());
        }
        let options: Vec<String> = rows.iter().map(UiRow::line).collect();
        let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
        let is_theme_section = !rows.is_empty()
            && rows
                .iter()
                .all(|row| matches!(row.kind, UiRowKind::Theme { .. }));
        let pick = if is_theme_section {
            let previews = GalleryPreviews::new();
            interactive_choice_ex(
                &section.title,
                &option_refs,
                0,
                "Enter opens the verb menu for the highlighted row · Esc goes back",
                Some(&|phase, index| match phase {
                    // Layout sweep: constants only, no renders.
                    PreviewPhase::Measure => GalleryPreviews::measure_placeholder(),
                    PreviewPhase::Draw => {
                        let row = &rows[index];
                        let UiRowKind::Theme { source_id } = &row.kind else {
                            return vec![row.hint.clone()];
                        };
                        // Queue the neighbor so arrow-key browsing finds its
                        // preview already rendered (never waits, niubash#170).
                        if let Some(next) = rows.get(index + 1) {
                            if let UiRowKind::Theme {
                                source_id: next_source,
                            } = &next.kind
                            {
                                previews.prefetch(next_source, &next.id);
                            }
                        }
                        let mut lines = vec![format!("{} · {}", row.id, row.hint)];
                        // Zero grace: instant callback; the menu's async pane
                        // pickup repaints when the render lands.
                        lines.extend(
                            previews
                                .lines_for(source_id, &row.id, std::time::Duration::ZERO)
                                .into_iter()
                                .take(crate::plugins::theme_preview::MAX_PREVIEW_LINES),
                        );
                        lines
                    }
                }),
            )
        } else {
            interactive_choice(
                &section.title,
                &option_refs,
                0,
                "Enter opens the verb menu for the highlighted row · Esc goes back",
            )
        };
        let index = match pick {
            Selection::Confirmed(index) => index,
            Selection::UseDefault | Selection::Abort => return Ok(()),
        };
        run_row_verbs(&rows[index])?;
    }
}

/// The verb menu for one row: per-item verbs from the command table,
/// filtered by the row kind (lazy `key_plugin`), plus Back.
fn run_row_verbs(row: &UiRow) -> anyhow::Result<()> {
    let mut options: Vec<String> = row
        .kind
        .verbs()
        .iter()
        .map(|entry| entry.label.to_string())
        .collect();
    if options.is_empty() {
        println!();
        println!(
            "  '{}' is info-only — see `niu plugin recipe show {}`",
            row.id, row.id
        );
        return Ok(());
    }
    options.push("Back".into());
    let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let preview = format!("[{}] {} — {}", row.state, row.id, row.hint);
    let pick = interactive_choice(&preview, &option_refs, 0, &help_footer());
    let index = match pick {
        Selection::Confirmed(index) => index,
        Selection::UseDefault | Selection::Abort => return Ok(()),
    };
    if index >= row.kind.verbs().len() {
        return Ok(());
    }
    let entry = row.kind.verbs()[index];
    print!("\r  {label} … ", label = entry.label);
    match apply_verb(entry.verb, &row.id) {
        Ok(detail) => println!("{}", text_style::green(&detail)),
        Err(err) => println!("{} {err:#}", text_style::red("failed:")),
    }
    println!(
        "  {}",
        text_style::dim("state refreshed below (Esc backs out one level)")
    );
    // Return to the section level so the re-render shows live state.
    Ok(())
}

/// The global section: verbs that act on everything (lazy `key` split).
fn run_global_verbs() -> anyhow::Result<()> {
    let verbs: Vec<&UiVerb> = UI_VERBS.iter().collect();
    let mut options: Vec<String> = verbs.iter().map(|v| v.label.to_string()).collect();
    options.push("Back".into());
    let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
    let pick = interactive_choice(
        "Global verbs — act on every installed source",
        &option_refs,
        0,
        &help_footer(),
    );
    let index = match pick {
        Selection::Confirmed(index) => index,
        Selection::UseDefault | Selection::Abort => return Ok(()),
    };
    if index >= verbs.len() {
        return Ok(());
    }
    // Every global verb today is `sync`; dispatch through the same table.
    let entry = verbs[index];
    match apply_verb(entry.verb, "") {
        Ok(detail) => println!("{}", text_style::green(&detail)),
        Err(err) => println!("{} {err:#}", text_style::red("failed:")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_table_has_no_duplicate_verbs() {
        let mut ids: Vec<&str> = UI_VERBS.iter().map(|entry| entry.verb).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), UI_VERBS.len());
    }

    #[test]
    fn help_footer_lists_every_cli_form() {
        let footer = help_footer();
        for entry in UI_VERBS {
            assert!(footer.contains(entry.cli), "missing {:#?}", entry.cli);
        }
    }

    #[test]
    fn row_line_matches_the_lazy_anatomy() {
        let row = UiRow {
            id: "oh-my-bash".into(),
            kind: UiRowKind::Source {
                trusted: false,
                degraded: false,
            },
            state: "untrusted".into(),
            hint: "abc123 · oh-my-bash".into(),
        };
        let line = row.line();
        assert!(line.contains("[untrusted]"), "{line}");
        assert!(line.contains("oh-my-bash — "), "{line}");
        // Piped output must stay escape-free (text_style is TTY-gated).
        assert!(!line.contains('\x1b'), "{line:?}");
    }

    #[test]
    fn verbs_for_source_respects_the_trust_gate() {
        let untrusted = UiRowKind::Source {
            trusted: false,
            degraded: false,
        };
        let labels: Vec<&str> = untrusted.verbs().iter().map(|entry| entry.verb).collect();
        assert!(labels.contains(&"trust"), "{labels:?}");
        let ready = UiRowKind::Source {
            trusted: true,
            degraded: false,
        };
        let labels: Vec<&str> = ready.verbs().iter().map(|entry| entry.verb).collect();
        assert!(!labels.contains(&"trust"), "{labels:?}");
        assert!(labels.contains(&"update"), "{labels:?}");
    }

    #[test]
    fn info_only_recipes_offer_no_verbs() {
        let kind = UiRowKind::Recipe {
            installable: false,
            installed: false,
        };
        assert!(kind.verbs().is_empty());
    }

    #[test]
    fn inventory_orders_sections_attention_first() {
        let sections = inventory();
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        let needs = titles
            .iter()
            .position(|t| t.starts_with("Needs review"))
            .expect("untrusted section exists");
        let ready = titles
            .iter()
            .position(|t| t.starts_with("Installed & ready"))
            .expect("ready section exists");
        let collections = titles
            .iter()
            .position(|t| t.starts_with("Collections"))
            .expect("collections section exists");
        let global = titles
            .iter()
            .position(|t| t.starts_with("Global"))
            .expect("global section exists");
        assert!(needs < ready, "{titles:?}");
        assert!(ready < collections, "{titles:?}");
        assert!(collections < global, "{titles:?}");
    }

    #[test]
    fn apply_verb_rejects_unknown_verbs() {
        let err = apply_verb("explode", "x").unwrap_err().to_string();
        assert!(err.contains("no such UI verb"), "{err}");
    }

    #[test]
    fn theme_rows_offer_activation_verbs() {
        let theme = UiRowKind::Theme {
            source_id: "oh-my-bash".into(),
        };
        let labels: Vec<&str> = theme.verbs().iter().map(|entry| entry.verb).collect();
        assert!(labels.contains(&"enable"), "{labels:?}");
        assert!(labels.contains(&"disable"), "{labels:?}");
    }

    /// niubash#170: a trusted theme-bearing source contributes a Themes
    /// section whose rows are theme rows (the preview pane's subjects), one
    /// per theme, sorted by name.
    #[test]
    fn inventory_lists_a_themes_section_for_trusted_theme_sources() {
        use crate::test_support::PROCESS_STATE_LOCK;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        let _env_lock = PROCESS_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp =
            std::env::temp_dir().join(format!("ui-theme-section-{}-{nanos}", std::process::id()));
        let _sources = std::env::set_var(
            "NIU_PLUGIN_SOURCES_ROOT",
            &temp.join("sources").to_string_lossy().to_string(),
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::plugins::sources::add_source(crate::plugins::sources::SourceInstallRequest {
                adapter: None,
                origin: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/sources/oh-my-bash")
                    .to_string_lossy()
                    .into_owned(),
                ref_name: None,
                commit: None,
                expected_checksum: None,
                id: None,
                entry: None,
                fetch_budget: None,
                credential_guard: false,
            })
            .expect("fixture source add must succeed");
            crate::plugins::sources::trust_source("oh-my-bash").expect("fixture trust");

            let sections = inventory();
            let themes = sections
                .iter()
                .find(|section| section.title.starts_with("Themes"))
                .expect("trusted theme source must contribute a Themes section");
            assert!(!themes.rows.is_empty(), "{:?}", themes.rows);
            let names: Vec<&str> = themes.rows.iter().map(|row| row.id.as_str()).collect();
            assert!(
                names.iter().any(|name| *name == "robbyrussell"),
                "{names:?}"
            );
            assert!(
                names.windows(2).all(|pair| pair[0] <= pair[1]),
                "theme rows must be sorted by name: {names:?}"
            );
            for row in &themes.rows {
                let UiRowKind::Theme { source_id } = &row.kind else {
                    panic!("theme section row must be a Theme row: {row:?}");
                };
                assert_eq!(source_id, "oh-my-bash", "{row:?}");
            }
        }));
        let _ = std::env::remove_var("NIU_PLUGIN_SOURCES_ROOT");
        let _ = crate::plugins::sources::remove_source("oh-my-bash");
        let _ = std::fs::remove_dir_all(&temp);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
