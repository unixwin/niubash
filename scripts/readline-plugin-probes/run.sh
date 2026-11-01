#!/bin/sh
# Repeatable acceptance harness for the in-process readline ABI / plugin
# surface (niubash#775, ruline#7).
#
# Usage:
#   scripts/readline-plugin-probes/run.sh [options]
#
# Options:
#   --niu PATH       niu executable to probe (default: target/debug/niu)
#   --build          cargo build --locked --bin niu before probing
#   --probe NAME     run one probe (see --list)
#   --timeout SEC    per-PTY-expect timeout (default: 20)
#   --keep           keep the work directory (transcripts, logs, .so)
#   --list           list probe names and exit
#
# Exit status: 0 when every probe is PASS or SKIP, 1 when any probe FAILs.
# SKIP is only for a genuinely absent external prerequisite (no niu binary,
# no fixture, no fzf). A missing feature of ours (for example rubash's
# `enable -f` loader) is a FAIL, never a SKIP: see unixwin/rubash#2440.

set -u

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
probes_dir=$script_dir
fixtures=$probes_dir/fixtures

niu_arg=${NIU_BIN:-}
do_build=0
only=
keep=0
timeout=20
work=

usage() {
    sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
}

probe_names='niu-build readline-abi-exports loadable-builtin-build
loadable-builtin-gnu-bash enable-f loadable-builtin-niu
fzf-bindx-register fzf-bindx-roundtrip bash-completion
inputrc-startup inputrc-reread'

while [ "$#" -gt 0 ]; do
    case "$1" in
        --niu)
            [ "$#" -ge 2 ] || { echo "error: --niu needs a path" >&2; exit 2; }
            niu_arg=$2
            shift 2
            ;;
        --build)
            do_build=1
            shift
            ;;
        --probe)
            [ "$#" -ge 2 ] || { echo "error: --probe needs a name" >&2; exit 2; }
            only=$2
            shift 2
            ;;
        --timeout)
            [ "$#" -ge 2 ] || { echo "error: --timeout needs seconds" >&2; exit 2; }
            timeout=$2
            shift 2
            ;;
        --keep)
            keep=1
            shift
            ;;
        --list)
            printf '%s\n' $probe_names
            exit 0
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option: $1 (try --help)" >&2
            exit 2
            ;;
    esac
done

if [ -n "$only" ]; then
    case " $(printf '%s' "$probe_names" | tr '\n' ' ') " in
        *" $only "*) ;;
        *)
            echo "error: unknown probe '$only' (try --list)" >&2
            exit 2
            ;;
    esac
fi

if [ -n "${NIU_PROBE_OUT:-}" ]; then
    work=$NIU_PROBE_OUT
    mkdir -p "$work"
    work=$(CDPATH= cd -- "$work" && pwd)
else
    work=$(mktemp -d "${TMPDIR:-/tmp}/niu-readline-probes.XXXXXX") || {
        echo "error: cannot create a work directory (set TMPDIR or NIU_PROBE_OUT)" >&2
        exit 2
    }
fi
logs=$work/logs
mkdir -p "$logs"
results=$work/results.tsv
: >"$results"

failures=0
passes=0
skips=0

cleanup() {
    # Never delete evidence of a regression: --keep is implicit on FAIL.
    if [ "$keep" -eq 0 ] && [ "$failures" -eq 0 ]; then
        rm -rf "$work"
    else
        printf 'artifacts kept: %s\n' "$work"
    fi
}
trap cleanup EXIT HUP INT TERM

record() {
    status=$1
    name=$2
    shift 2
    reason=$*
    printf '%s\t%s\t%s\n' "$status" "$name" "$reason" >>"$results"
    printf '%-4s %-30s %s\n' "$status" "$name" "$reason"
    case "$status" in
        PASS) passes=$((passes + 1)) ;;
        FAIL) failures=$((failures + 1)) ;;
        SKIP) skips=$((skips + 1)) ;;
    esac
}
pass() { record PASS "$@"; }
fail() { record FAIL "$@"; }
skip() { record SKIP "$@"; }

should_run() {
    [ -z "$only" ] || [ "$only" = "$1" ]
}

first_line() {
    printf '%s\n' "$1" | sed -n '1p'
}

# ---------------------------------------------------------------------------
# Versions (recorded in the work dir; also the matrix's context)
# ---------------------------------------------------------------------------
versions=$work/versions.txt
{
    printf 'date: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || date)"
    printf 'uname: %s\n' "$(uname -a 2>/dev/null || uname)"
    printf 'sh: %s\n' "$(readlink /proc/$$/exe 2>/dev/null || printf '%s' "$0")"
    if command -v cc >/dev/null 2>&1; then
        printf 'cc: %s\n' "$(cc --version 2>&1 | sed -n '1p')"
    else
        printf 'cc: absent\n'
    fi
    if command -v bash >/dev/null 2>&1; then
        printf 'bash: %s\n' "$(bash --version 2>&1 | sed -n '1p')"
        printf 'bash-completion: %s\n' "$(bash --noprofile --norc -c '
            for f in /usr/share/bash-completion/bash_completion /etc/bash_completion; do
                [ -r "$f" ] && { printf "%s" "$f"; exit 0; }
            done
            printf absent' 2>/dev/null || printf unknown)"
    else
        printf 'bash: absent\n'
    fi
    if command -v fzf >/dev/null 2>&1; then
        printf 'fzf: %s\n' "$(fzf --version 2>&1 | sed -n '1p')"
    else
        printf 'fzf: absent (deterministic fixture/fzf is used)\n'
    fi
    if command -v python3 >/dev/null 2>&1; then
        printf 'python3: %s\n' "$(python3 --version 2>&1)"
    else
        printf 'python3: absent (PTY probes will SKIP)\n'
    fi
} >"$versions" 2>&1

printf '== niu readline plugin acceptance ==\n'
cat "$versions"
printf 'work: %s\n\n' "$work"

# ---------------------------------------------------------------------------
# niu build / binary
# ---------------------------------------------------------------------------
NIU=
if [ -n "$niu_arg" ]; then
    case "$niu_arg" in
        /*) NIU=$niu_arg ;;
        *) NIU=$root/$niu_arg ;;
    esac
elif [ -x "$root/target/debug/niu" ]; then
    NIU=$root/target/debug/niu
fi

if [ "$do_build" -eq 1 ] || [ -z "$NIU" ] || [ ! -x "$NIU" ]; then
    if command -v cargo >/dev/null 2>&1; then
        if (cd "$root" && cargo build --locked --bin niu) >"$logs/niu-build.log" 2>&1; then
            NIU=$root/target/debug/niu
            if should_run niu-build; then
                pass niu-build "cargo build --locked --bin niu -> target/debug/niu"
            fi
        else
            if should_run niu-build; then
                fail niu-build "cargo build failed; see $logs/niu-build.log ($(first_line "$(tail -n 1 "$logs/niu-build.log")"))"
            fi
            NIU=
        fi
    else
        if should_run niu-build; then
            skip niu-build "no cargo on PATH and no target/debug/niu binary"
        fi
        NIU=
    fi
else
    if should_run niu-build; then
        pass niu-build "using existing binary $NIU (pass --build to rebuild)"
    fi
fi

if [ -n "$NIU" ] && [ ! -x "$NIU" ]; then
    NIU=
fi
if [ -n "$NIU" ]; then
    {
        printf 'niu: %s\n' "$NIU"
        "$NIU" --version 2>&1 | sed -n '1p'
    } >>"$versions"
    printf 'niu binary: %s\n' "$NIU"
    "$NIU" --version 2>&1 | sed -n '1p'
else
    printf 'niu binary: absent (niu-dependent probes will SKIP)\n'
fi
printf '\n'

# ---------------------------------------------------------------------------
# readline ABI export audit (existing release gate)
# ---------------------------------------------------------------------------
if should_run readline-abi-exports; then
    if [ -z "$NIU" ]; then
        skip readline-abi-exports "no niu binary"
    else
        if sh "$root/scripts/check-readline-abi-exports.sh" "$NIU" \
            >"$logs/readline-abi-exports.log" 2>&1; then
            pass readline-abi-exports "$(tr '\n' ';' <"$logs/readline-abi-exports.log" | sed 's/;$//')"
        else
            rc=$?
            if [ "$rc" -eq 2 ]; then
                skip readline-abi-exports "no binary to inspect ($NIU)"
            else
                fail readline-abi-exports "dynamic symbols missing; see $logs/readline-abi-exports.log ($(first_line "$(tail -n 1 "$logs/readline-abi-exports.log")"))"
            fi
        fi
    fi
fi

# ---------------------------------------------------------------------------
# C loadable-builtin fixture (build once; probes below consume it)
# ---------------------------------------------------------------------------
SO=
if command -v "${CC:-cc}" >/dev/null 2>&1; then
    if SO=$(sh "$probes_dir/build-builtin.sh" "$work/build" 2>"$logs/build-builtin.log"); then
        if should_run loadable-builtin-build; then
            pass loadable-builtin-build "cc -> $SO"
        fi
    else
        SO=
        if should_run loadable-builtin-build; then
            fail loadable-builtin-build "C fixture failed to build; see $logs/build-builtin.log ($(first_line "$(tail -n 1 "$logs/build-builtin.log")"))"
        fi
    fi
else
    if should_run loadable-builtin-build; then
        skip loadable-builtin-build "no C compiler (${CC:-cc}); install cc or set CC"
    fi
fi

# ---------------------------------------------------------------------------
# GNU bash oracle: the same .so against a live readline buffer
# ---------------------------------------------------------------------------
if should_run loadable-builtin-gnu-bash; then
    if [ -z "$SO" ]; then
        skip loadable-builtin-gnu-bash "fixture not built"
    elif ! command -v bash >/dev/null 2>&1; then
        skip loadable-builtin-gnu-bash "bash not on PATH"
    elif command -v python3 >/dev/null 2>&1; then
        log=$logs/loadable-builtin-gnu-bash.log
        python3 "$probes_dir/pty_probes.py" builtin \
            --bash "$(command -v bash)" --builtin "$SO" \
            --work "$work/pty" --transcript "$work/builtin.transcript" \
            >"$log" 2>&1
        rc=$?
        reason=$(sed -n 's/^REASON=//p' "$log" | tail -n 1)
        case "$rc" in
            0) pass loadable-builtin-gnu-bash "$reason" ;;
            77) skip loadable-builtin-gnu-bash "$reason" ;;
            *) fail loadable-builtin-gnu-bash "$reason (see $log)" ;;
        esac
    else
        out=$(bash --noprofile --norc -c "enable -f '$SO' rl_probe; rl_probe" 2>&1)
        if printf '%s\n' "$out" | grep -q '^RL_PROBE .*result=ok'; then
            pass loadable-builtin-gnu-bash "non-PTY GNU bash oracle ($(printf '%s\n' "$out" | grep '^RL_PROBE ' | head -n 1))"
        else
            fail loadable-builtin-gnu-bash "GNU bash could not load/run the fixture: $(first_line "$out")"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# enable -f against niu (known rubash loader blocker) and the builtin run
# ---------------------------------------------------------------------------
if should_run enable-f; then
    if [ -z "$NIU" ]; then
        skip enable-f "no niu binary"
    elif [ -z "$SO" ]; then
        skip enable-f "fixture not built"
    else
        log=$logs/enable-f.log
        out=$("$NIU" -c "enable -f '$SO' rl_probe; rl_probe" 2>&1)
        rc=$?
        printf '%s\n' "$out" >"$log"
        printf 'command: %s -c "enable -f %s rl_probe; rl_probe"\nrc: %s\n' \
            "$NIU" "$SO" "$rc" >>"$log"
        if printf '%s\n' "$out" | grep -q 'dynamic loading.*not supported'; then
            diagnostic=$(printf '%s\n' "$out" | grep 'dynamic loading' | head -n 1)
            fail enable-f "rubash#2440: loadable-builtin loader refuses dlopen: $diagnostic (command: $NIU -c \"enable -f $SO rl_probe; rl_probe\")"
        elif printf '%s\n' "$out" | grep -q '^RL_PROBE '; then
            pass enable-f "enable -f loaded rl_probe (command: $NIU -c \"enable -f $SO rl_probe; rl_probe\")"
        else
            fail enable-f "unexpected enable -f result rc=$rc: $(first_line "$out") (see $log)"
        fi
    fi
fi

if should_run loadable-builtin-niu; then
    if [ -z "$NIU" ]; then
        skip loadable-builtin-niu "no niu binary"
    elif [ -z "$SO" ]; then
        skip loadable-builtin-niu "fixture not built"
    else
        log=$logs/loadable-builtin-niu.log
        out=$("$NIU" -c "enable -f '$SO' rl_probe; rl_probe" 2>&1)
        printf '%s\n' "$out" >"$log"
        if printf '%s\n' "$out" | grep -q 'dynamic loading.*not supported'; then
            fail loadable-builtin-niu "rubash#2440: depends on enable -f: $(printf '%s\n' "$out" | grep 'dynamic loading' | head -n 1)"
        elif printf '%s\n' "$out" | grep -q '^RL_PROBE '; then
            line=$(printf '%s\n' "$out" | grep '^RL_PROBE ' | head -n 1)
            if printf '%s\n' "$line" | grep -q 'result=ok'; then
                if printf '%s\n' "$line" | grep -q 'line_buffer=ok'; then
                    pass loadable-builtin-niu "$line"
                else
                    fail loadable-builtin-niu "live editor buffer absent in -c (ABI core works): $line"
                fi
            else
                fail loadable-builtin-niu "builtin loaded but the live readline ABI is incomplete: $line"
            fi
        else
            fail loadable-builtin-niu "unexpected result: $(first_line "$out") (see $log)"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# fzf bash integration: bind -x registration + PTY round-trip
# ---------------------------------------------------------------------------
if should_run fzf-bindx-register; then
    if [ -z "$NIU" ]; then
        skip fzf-bindx-register "no niu binary"
    else
        log=$logs/fzf-bindx-register.log
        out=$("$NIU" -c "bind -x '\"\\C-r\": __probe_fzf'; bind -X" 2>&1)
        printf '%s\n' "$out" >"$log"
        if printf '%s\n' "$out" | grep -q '^"\\C-r" "__probe_fzf"$'; then
            pass fzf-bindx-register "bind -x + bind -X round-trip: $(printf '%s\n' "$out" | grep '^"\\C-r"')"
        else
            fail fzf-bindx-register "bind -X did not list the bind -x entry: $(first_line "$out") (see $log)"
        fi
    fi
fi

if should_run fzf-bindx-roundtrip; then
    if [ -z "$NIU" ]; then
        skip fzf-bindx-roundtrip "no niu binary"
    elif ! command -v python3 >/dev/null 2>&1; then
        skip fzf-bindx-roundtrip "python3 not available for the PTY driver"
    else
        chmod +x "$fixtures/fzf" 2>/dev/null || true
        log=$logs/fzf-bindx-roundtrip.log
        python3 "$probes_dir/pty_probes.py" fzf \
            --niu "$NIU" --fzf-rc "$fixtures/fzf.rc" --fake-fzf "$fixtures/fzf" \
            --work "$work/pty" --transcript "$work/fzf.transcript" \
            --timeout "$timeout" >"$log" 2>&1
        rc=$?
        reason=$(sed -n 's/^REASON=//p' "$log" | tail -n 1)
        case "$rc" in
            0) pass fzf-bindx-roundtrip "$reason" ;;
            77) skip fzf-bindx-roundtrip "$reason" ;;
            *) fail fzf-bindx-roundtrip "$reason (transcript: $work/fzf.transcript)" ;;
        esac
    fi
fi

# ---------------------------------------------------------------------------
# bash-completion portable subset: complete -F, COMPREPLY, compgen
# ---------------------------------------------------------------------------
if should_run bash-completion; then
    if [ -z "$NIU" ]; then
        skip bash-completion "no niu binary"
    elif ! command -v bash >/dev/null 2>&1; then
        skip bash-completion "bash not on PATH for the GNU oracle"
    else
        completion_fixture=$fixtures/completion.bash
        script=". '$completion_fixture'
complete -p probe-tool
COMP_WORDS=(probe-tool --col); COMP_CWORD=1; _probe_tool; printf 'A=%s\\n' \"\${COMPREPLY[@]}\"
COMP_WORDS=(probe-tool --color a); COMP_CWORD=2; _probe_tool; printf 'B=%s\\n' \"\${COMPREPLY[@]}\"
printf 'C=%s\\n' \"\$(compgen -W 'alpha beta' -- b)\"
printf 'WORDBREAKS=%s\\n' \"\$COMP_WORDBREAKS\""
        niu_log=$logs/bash-completion-niu.log
        bash_log=$logs/bash-completion-bash.log
        "$NIU" -c "$script" >"$niu_log" 2>&1
        niu_rc=$?
        bash --noprofile --norc -c "$script" >"$bash_log" 2>&1
        bash_rc=$?
        if [ "$niu_rc" -ne 0 ]; then
            fail bash-completion "niu exited $niu_rc; see $niu_log ($(first_line "$(tail -n 1 "$niu_log")"))"
        elif [ "$bash_rc" -ne 0 ]; then
            skip bash-completion "GNU bash oracle failed ($bash_rc); see $bash_log"
        elif ! cmp -s "$niu_log" "$bash_log"; then
            fail bash-completion "niu output differs from GNU bash; see $niu_log and $bash_log"
        elif ! grep -qx 'A=--color' "$niu_log" ||
            ! grep -qx 'B=auto' "$niu_log" ||
            ! grep -qx 'B=always' "$niu_log" ||
            ! grep -qx 'C=beta' "$niu_log"; then
            fail bash-completion "COMPREPLY/compgen output missing expected candidates; see $niu_log"
        else
            pass bash-completion "complete -p/COMPREPLY/compgen match GNU bash for the local fixture ($(grep '^WORDBREAKS=' "$niu_log" | head -n 1))"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# inputrc startup (set variables, function binding, macro, $include)
# ---------------------------------------------------------------------------
if should_run inputrc-startup; then
    if [ -z "$NIU" ]; then
        skip inputrc-startup "no niu binary"
    else
        inputrc=$work/inputrc
        sed "s|@EXTRA@|$fixtures/extra.inputrc|" "$fixtures/inputrc" >"$inputrc"
        log=$logs/inputrc-startup.log
        INPUTRC=$inputrc "$NIU" -c 'bind -v; bind -p; bind -s' >"$log" 2>&1
        missing=
        grep -Fq 'set completion-ignore-case on' "$log" || missing="$missing completion-ignore-case"
        grep -Fq 'set keyseq-timeout 77' "$log" || missing="$missing keyseq-timeout"
        grep -Fq '"\C-t": transpose-chars' "$log" || missing="$missing C-t-function"
        grep -Fq '"\C-x\C-a": beginning-of-line' "$log" || missing="$missing include-C-x-C-a"
        grep -Fq '"\C-g": "echo macro-ran' "$log" || missing="$missing C-g-macro"
        if [ -n "$missing" ]; then
            fail inputrc-startup "missing:$missing; see $log"
        else
            pass inputrc-startup "set vars + \"\\C-t\" function + \"\\C-g\" macro + \$include (\\C-x\\C-a) applied at startup"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# inputrc after C-x C-r (PTY): rc reload verified, inputrc gap recorded
# ---------------------------------------------------------------------------
if should_run inputrc-reread; then
    if [ -z "$NIU" ]; then
        skip inputrc-reread "no niu binary"
    elif ! command -v python3 >/dev/null 2>&1; then
        skip inputrc-reread "python3 not available for the PTY driver"
    else
        log=$logs/inputrc-reread.log
        python3 "$probes_dir/pty_probes.py" inputrc \
            --niu "$NIU" --work "$work/pty" \
            --transcript "$work/inputrc.transcript" \
            --timeout "$timeout" >"$log" 2>&1
        rc=$?
        reason=$(sed -n 's/^REASON=//p' "$log" | tail -n 1)
        case "$rc" in
            0) pass inputrc-reread "$reason" ;;
            77) skip inputrc-reread "$reason" ;;
            *) fail inputrc-reread "$reason (transcript: $work/inputrc.transcript)" ;;
        esac
    fi
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
printf '\n== matrix ==\n'
printf '%-4s %-30s %s\n' STATUS PROBE REASON
awk -F '\t' '{ printf "%-4s %-30s %s\n", $1, $2, $3 }' "$results"
printf '\nPASS=%s FAIL=%s SKIP=%s\n' "$passes" "$failures" "$skips"
if [ "$failures" -ne 0 ]; then
    printf 'result: FAIL (%s regression(s))\n' "$failures"
    printf 'logs: %s\n' "$logs"
    exit 1
fi
printf 'result: OK (no regressions; %s known blocker(s) skipped)\n' "$skips"
exit 0
