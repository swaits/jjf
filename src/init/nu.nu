def --env jjf [...args] {
    # Meta-commands and bare flags bypass the picker and run the binary directly.
    if ($args | is-empty) {
        ^jjf
        return
    }
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
    ^sh -c $cmd
}
