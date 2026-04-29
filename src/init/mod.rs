pub const BASH: &str = include_str!("bash.sh");
pub const ZSH: &str = include_str!("zsh.sh");
pub const FISH: &str = include_str!("fish.fish");
pub const NU: &str = include_str!("nu.nu");

pub fn snippet(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH),
        "zsh" => Some(ZSH),
        "fish" => Some(FISH),
        "nu" | "nushell" => Some(NU),
        _ => None,
    }
}
