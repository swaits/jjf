_jjf_drop_last_history() {
    local _off
    _off=$(history 1 2>/dev/null | awk '{print $1}')
    [ -n "$_off" ] && history -d "$_off" 2>/dev/null || true
}

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
    _jjf_cmd="$(command jjf --emit "$@")"
    local _jjf_rc=$?
    # Always drop the `jjf …` entry from history; on cancel/error there's
    # nothing to replace it with, on success we add the resolved command below.
    _jjf_drop_last_history
    if [ $_jjf_rc -ne 0 ] || [ -z "$_jjf_cmd" ]; then
        return $_jjf_rc
    fi
    history -s -- "$_jjf_cmd"
    sh -c "$_jjf_cmd"
}
