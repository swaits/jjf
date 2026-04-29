jjf() {
    # No args → default to `show` so `jjf` alone is a rev browser.
    if [ $# -eq 0 ]; then
        set -- show
    fi
    # Meta-commands and bare flags bypass the picker and run the binary directly.
    if [ "$1" = "init" ] || [ "${1#-}" != "$1" ]; then
        command jjf "$@"
        return $?
    fi
    local _jjf_cmd
    _jjf_cmd="$(command jjf --emit "$@")" || return $?
    [ -n "$_jjf_cmd" ] || return 130
    # Replace the original `jjf …` history entry with the resolved jj command.
    local _jjf_last_offset
    _jjf_last_offset=$(history 1 2>/dev/null | awk '{print $1}')
    if [ -n "$_jjf_last_offset" ]; then
        history -d "$_jjf_last_offset" 2>/dev/null || true
    fi
    history -s -- "$_jjf_cmd"
    sh -c "$_jjf_cmd"
}
