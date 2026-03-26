pub mod commands;
pub mod config;

/// Prompt the user for confirmation. Returns true if they type "y" or "yes".
/// If `yes` is true, skips the prompt and returns true immediately.
pub fn confirm(msg: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    eprint!("{msg} [y/N] ");
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}
