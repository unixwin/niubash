# Representative bash-completion definition (no bash-completion package
# needed).  The probe sources this under niu and under GNU bash and compares
# `complete -p`, COMPREPLY and compgen output.
_probe_tool() {
    local cur prev
    COMPREPLY=()
    cur=${COMP_WORDS[COMP_CWORD]}
    prev=${COMP_WORDS[COMP_CWORD - 1]}
    case "$prev" in
        --color) COMPREPLY=($(compgen -W 'auto always never' -- "$cur")); return 0 ;;
        --level) COMPREPLY=($(compgen -W '1 2 3' -- "$cur")); return 0 ;;
    esac
    if [[ "$cur" == -* ]]; then
        COMPREPLY=($(compgen -W '--color --level --verbose --help' -- "$cur"))
    else
        COMPREPLY=($(compgen -W 'start stop status' -- "$cur"))
    fi
}
complete -F _probe_tool probe-tool
