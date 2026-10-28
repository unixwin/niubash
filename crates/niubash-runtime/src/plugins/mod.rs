//! Plugin ecosystem surface.
//!
//! The built-in plugin/theme stack is retired (niubash#145, owner decision
//! 2026-09-28): the host no longer ships a pack inventory, an oh-my-niu
//! bundle loader, native plugin presets, or built-in themes. NIU_PLUGINS,
//! NIU_THEME, NIU_THEME_PLUGIN and NIU_DISABLE_DEFAULT_PLUGINS lines in an
//! existing rc stay legal shell assignments - nothing reads them any more.
//!
//! The plugin system is the external bash ecosystem as first-class
//! content (owner rulings 2026-10-02): oh-my-bash, bash-it and
//! bash-completion install over git clone on explicit user command
//! ([`sources`], curated by [`catalog`]), and expose their real themes /
//! plugins / aliases / completions to `niu plugin enable/disable` through
//! each manager's own selection mechanism ([`assets`]). No vendoring: the
//! license stays between the user and upstream.
//!
//! Recipe-shaped access (lazy.nvim/mason conventions): [`recipes`] is the
//! data index over the ecosystem and [`distros`] the collection manifests
//! (LazyVim extras pattern), with [`ui`] the menu-level view over the same
//! verbs. The former direct-binary download driver is retired (owner
//! ruling 2026-10-04, download retraction): the shell carries zero
//! network/HTTP responsibility, `niu plugin add <git-url>` via `git clone`
//! is the only extension installation entry, and executable-tool recipes
//! are catalog rows whose install prints a package-manager recommendation
//! (winget/scoop on Windows, apt/dnf/yum/brew elsewhere). [`mirrors`] is
//! the transport-layer China-network mirroring (§14.8): git-only
//! `insteadOf` rewriting of git fetches, never the recorded origins.

pub mod assets;
pub mod catalog;
pub mod descriptors;
pub mod distros;
pub mod loader;
pub mod mirrors;
pub mod recipes;
pub mod sources;
pub mod spec;
pub mod sync;
pub mod theme_preview;
pub mod ui;
