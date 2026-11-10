//! niubash#971 — closing the Windows Terminal tab must flush the
//! session history. On Windows the tab close is a CTRL_CLOSE_EVENT;
//! the console handler used to return 0 for it, so the default
//! handler killed the process before `end_repl_session` ran and the
//! whole session's history was lost. The fix routes the event to the
//! session-end funnel (EXIT trap + history flush) on the main thread.
//!
//! This file used to hold one chain cell: the unix analogue of the
//! tab close (SIGHUP to the session leader at the prompt), driving
//! the same session-end funnel through a real signal-driven exit.
//! That cell was re-scoped out of this PR by the captain review
//! (item F2 of the review record for this change): the
//! SIGHUP-at-prompt shape is the unix signal face, which the scope
//! ruling assigns to niubash#974, and this change does not touch the
//! unix signal path — the cell was red on this head for exactly that
//! reason. Its base-side fail-first record (0 passed / 1 failed on
//! the pre-change master, a true defect reproduction) is preserved
//! in the review record and in the lane REPORT. The cell now lives
//! with the #974 pin set; do not re-add it here, and do not park it
//! behind an ignore attribute.
//!
//! What pins this change in-tree: the console-event classification
//! and close-state unit tests in
//! `crates/niubash-runtime/src/ctrl_c.rs` (literal Win32 event codes,
//! identical on both platforms) and the session-end flush handle
//! unit test in `crates/niubash-runtime/src/history.rs`. The Windows
//! end-to-end shape (ConPTY tab close) cannot be driven from a test
//! host here and is a MANUAL verification item in the lane REPORT.
