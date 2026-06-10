//! Shared terminal prompts for CLI commands. The core never prompts; this is the
//! CLI's implementation of that boundary.

use std::io::{self, Write};

pub fn ask(prompt: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    io::stdout().flush().ok();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return default.to_string();
    }
    let line = line.trim();
    if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    }
}

pub fn confirm(prompt: &str, default: bool) -> bool {
    print!("{prompt} ({}): ", if default { "Y/n" } else { "y/N" });
    io::stdout().flush().ok();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return default;
    }
    let line = line.trim().to_ascii_lowercase();
    match line.as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    }
}

/// Expand a leading `~` to the home directory, like the Python tool did.
pub fn clean_path(p: &str) -> String {
    let p = p.trim();
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    if p == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
    }
    p.to_string()
}
