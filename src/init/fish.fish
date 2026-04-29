function jjf
    set -e _jjf_pending_swap
    # No args → default to `show` so `jjf` alone is a rev browser.
    if test (count $argv) -eq 0
        set argv show
    end
    # Meta-commands and bare flags bypass the picker and run the binary directly.
    if test "$argv[1]" = init; or string match -q -- '-*' $argv[1]
        command jjf $argv
        return $status
    end
    # Arm the postexec hook *before* invoking the picker so the original
    # `jjf …` entry is removed from history on any exit path (cancel, error,
    # or success). Fish records function invocations *after* return.
    set -g _jjf_pending_swap 1
    set -l _jjf_cmd (command jjf --emit $argv)
    set -l _jjf_status $status
    if test $_jjf_status -ne 0
        return $_jjf_status
    end
    if test -z "$_jjf_cmd"
        return 130
    end
    builtin history append -- $_jjf_cmd
    sh -c $_jjf_cmd
end

function _jjf_postexec --on-event fish_postexec
    if test "$_jjf_pending_swap" = "1"
        builtin history delete --exact --case-sensitive -- $argv[1]
        set -e _jjf_pending_swap
    end
end
