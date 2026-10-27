//! niubash#384 — platform-support.md must describe the measured
//! behavior, not the MSYS assumption it corrects.
//!
//! The page's behavioral-boundary table was rewritten after live
//! measurement: `/` is winuxcmd's installation root (not the current
//! drive's root), and POSIX-form argv IS rewritten for native
//! children (the earlier "arguments arrive verbatim" framing was
//! wrong — only the native command processor keeps its operands
//! verbatim). These guards pin the corrected text so a doc edit
//! cannot silently revert to the pre-measurement claims.

use std::path::PathBuf;

fn platform_support() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/src/platform-support.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn root_row_names_winuxcmd_installation_root() {
    let text = platform_support();
    assert!(
        text.contains("`/` is winuxcmd's installation root"),
        "the root row must state that / is winuxcmd's installation root"
    );
    assert!(
        !text.contains("`/` is the current drive's root"),
        "the pre-measurement claim must not return"
    );
}

#[test]
fn argv_row_states_rewriting_for_native_children() {
    let text = platform_support();
    assert!(
        text.contains("It does — `/c/Users/cmx` reaches a native child as"),
        "the argv row must state the measured rewriting behavior"
    );
    assert!(
        !text.contains("Arguments arrive **verbatim**"),
        "the pre-measurement verbatim claim must not return"
    );
}
