def --env jjf [...args] {
    # No args → default to `show` so `jjf` alone is a rev browser.
    let args = if ($args | is-empty) { ["show"] } else { $args }
    # Meta-commands and bare flags bypass the picker and run the binary directly.
    let first = $args.0
    if $first == "init" or ($first | str starts-with "-") {
        ^jjf ...$args
        return
    }
    let result = (^jjf --emit ...$args | complete)
    if $result.exit_code != 0 { exit $result.exit_code }
    let cmd = ($result.stdout | str trim)
    if ($cmd | is-empty) { exit 130 }
    [{command: $cmd, start_timestamp: (date now | format date "%+")}] | history import
    print $"(ansi grey)$(ansi reset) ($cmd)"
    ^sh -c $cmd
}
