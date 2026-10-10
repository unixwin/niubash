//! niubash#201: `$(date +%s%N)` costs ~31ms per call on Windows (external
//! date.exe process creation), while the GNU bash 5 builtin `$EPOCHREALTIME`
//! expands in ~microseconds. The engine (rubash) already implements both
//! `$EPOCHREALTIME` and `$EPOCHSECONDS`; these tests pin the contract the
//! issue's advice relies on:
//!
//! 1. both variables are set and match GNU's format
//!    (`EPOCHREALTIME` = `SECONDS.microseconds`, e.g. `1699999999.123456`;
//!    `EPOCHSECONDS` = plain integer);
//! 2. they agree with the external `date +%s` at second granularity;
//! 3. `$EPOCHREALTIME` advances monotonically across expansions (it is
//!    live, not a startup snapshot) with microsecond resolution;
//! 4. `$EPOCHREALTIME` expansion is orders of magnitude cheaper than an
//!    external `$(date)` command substitution.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niu" });
    fallback
}

fn run_niu(script: &str) -> String {
    let output = Command::new(niu_binary())
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("spawn niu: {err}"));
    assert!(
        output.status.success(),
        "niu -c failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// `EPOCHREALTIME` is `<integer>.<6-digit-fraction>` like GNU bash 5
/// (variables.c: initialize_dynamic_variables / get_epochrealtime);
/// `EPOCHSECONDS` is a plain integer.
#[test]
fn epoch_variables_match_gnu_format() {
    let out = run_niu(
        r#"case $EPOCHREALTIME in
             [0-9]*.[0-9][0-9][0-9][0-9][0-9][0-9]) echo "RT_OK:$EPOCHREALTIME" ;;
             *) echo "RT_BAD:$EPOCHREALTIME" ;;
           esac
           case $EPOCHSECONDS in
             ''|*[!0-9]*) echo "SEC_BAD:$EPOCHSECONDS" ;;
             *) echo "SEC_OK:$EPOCHSECONDS" ;;
           esac"#,
    );
    let mut lines = out.lines();
    let rt = lines.next().expect("EPOCHREALTIME line");
    let sec = lines.next().expect("EPOCHSECONDS line");
    assert!(rt.starts_with("RT_OK"), "bad EPOCHREALTIME: {rt}");
    assert!(sec.starts_with("SEC_OK"), "bad EPOCHSECONDS: {sec}");

    // Integer part of EPOCHREALTIME equals EPOCHSECONDS.
    let rt_val = rt.trim_start_matches("RT_OK:");
    let int_part = rt_val.split('.').next().expect("integer part");
    assert_eq!(
        int_part,
        sec.trim_start_matches("SEC_OK:"),
        "EPOCHREALTIME integer part must equal EPOCHSECONDS"
    );
}

/// Both variables agree with the external `date +%s` within one second
/// (they read the same system clock; GNU-compatible semantics).
#[test]
fn epoch_seconds_agrees_with_external_date() {
    let out = run_niu("echo $((EPOCHSECONDS - $(date +%s)))");
    let diff: i64 = out.parse().expect("integer diff vs date +%s");
    assert!(
        diff.abs() <= 1,
        "EPOCHSECONDS diverges from date +%s by {diff}s"
    );
    // And with the test host's own clock.
    let secs: u64 = run_niu("echo $EPOCHSECONDS")
        .parse()
        .expect("EPOCHSECONDS as u64");
    let now = unix_seconds_now();
    assert!(
        secs.abs_diff(now) <= 5,
        "EPOCHSECONDS {secs} vs host clock {now}"
    );
}

/// `$EPOCHREALTIME` is live: two expansions at least one sleep apart are
/// strictly increasing, and a tight pair differs at microsecond resolution
/// (the `${EPOCHREALTIME/.}` microsecond-integer idiom from the issue).
#[test]
fn epochrealtime_is_live_and_monotonic() {
    let out = run_niu(
        r#"a=${EPOCHREALTIME/.}
           sleep 0.05
           b=${EPOCHREALTIME/.}
           if (( b > a )); then echo "MONO_OK:delta=$(( b - a ))"; else echo "MONO_BAD:a=$a b=$b"; fi"#,
    );
    assert!(
        out.starts_with("MONO_OK"),
        "EPOCHREALTIME not monotonic: {out}"
    );
    // 50ms sleep => delta in (30000, 5_000_000) microseconds; loose bounds
    // keep the assertion scheduler-tolerant while proving sub-second ticks.
    let delta: u64 = out
        .split("delta=")
        .nth(1)
        .and_then(|d| d.parse().ok())
        .expect("delta microseconds");
    assert!(
        (30_000..5_000_000).contains(&delta),
        "unexpected delta {delta}µs for a 50ms sleep"
    );
}

/// The issue's core claim: `$EPOCHREALTIME` is orders of magnitude cheaper
/// than an external `$(date +%s%N)` command substitution. Asserting the
/// exact microsecond figures would be flaky on loaded CI, so we require
/// only a 50x gap over 40 iterations each (issue measured ~500x; debug
/// builds land well above 50x).
#[test]
fn epochrealtime_beats_external_date_for_timing() {
    let out = run_niu(
        r#"t0=${EPOCHREALTIME/.}
           for i in {1..40}; do x=$(date +%s%N); done
           t1=${EPOCHREALTIME/.}
           date_us=$(( t1 - t0 ))
           t0=${EPOCHREALTIME/.}
           for i in {1..40}; do x=$EPOCHREALTIME; done
           t1=${EPOCHREALTIME/.}
           epoch_us=$(( t1 - t0 ))
           echo "date_us=$date_us epoch_us=$epoch_us""#,
    );
    let mut date_us: Option<u64> = None;
    let mut epoch_us: Option<u64> = None;
    for token in out.split_whitespace() {
        if let Some(v) = token.strip_prefix("date_us=") {
            date_us = v.parse().ok();
        }
        if let Some(v) = token.strip_prefix("epoch_us=") {
            epoch_us = v.parse().ok();
        }
    }
    let (Some(date_us), Some(epoch_us)) = (date_us, epoch_us) else {
        panic!("unparseable timing output: {out}");
    };
    assert!(
        epoch_us > 0,
        "epoch loop recorded zero time (clock not ticking?)"
    );
    let ratio = date_us as f64 / epoch_us as f64;
    assert!(
        ratio >= 50.0,
        "EPOCHREALTIME not fast enough: date={date_us}µs epoch={epoch_us}µs (x{ratio:.1})"
    );
}
