//! Architecture scaffold only. No Docker/GC enforcement in this crate yet.
#[derive(Debug, PartialEq)]
pub enum Decision { Allow, Warn, Deny }
pub fn check_command(cmd: &str) -> Decision {
    let s = cmd.to_ascii_lowercase();
    if s.contains("docker volume prune") || s.contains("docker system prune") { return Decision::Deny; }
    if s.contains("docker build") || s.contains("docker compose up") { return Decision::Warn; }
    Decision::Allow
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn protect_volumes() {assert_eq!(check_command("docker volume prune"),Decision::Deny);}
}
