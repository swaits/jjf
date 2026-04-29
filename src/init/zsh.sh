jjf() {
    # Meta-commands and bare flags bypass the picker and run the binary directly.
    if [ $# -eq 0 ] || [ "$1" = "init" ] || [ "${1#-}" != "$1" ]; then
        command jjf "$@"
        return $?
    fi
    local _jjf_cmd
    _jjf_cmd="$(command jjf --emit "$@")" || return $?
    [ -n "$_jjf_cmd" ] || return 130
    print -s -- "$_jjf_cmd"
    sh -c "$_jjf_cmd"
}

# Suppress zsh from recording `jjf …` invocations; jjf appends the resolved
# jj command itself via `print -s`.
autoload -Uz add-zsh-hook 2>/dev/null
_jjf_zshaddhistory() {
    case "$1" in
        jjf\ *|jjf$'\n'*) return 1 ;;
    esac
    return 0
}
if typeset -f add-zsh-hook >/dev/null; then
    add-zsh-hook zshaddhistory _jjf_zshaddhistory
fi
