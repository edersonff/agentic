use std::collections::HashSet;

const DESTRUCTIVE_COMMANDS: &[&str] = &[
    "rm",
    "rmdir",
    "shred",
    "dd",
    "mkfs",
    "fdisk",
    "parted",
    "sudo",
    "su",
    "kill",
    "killall",
    "pkill",
    "chmod",
    "chown",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "systemctl",
];

const DESTRUCTIVE_SUBSTRINGS: &[&str] = &[
    "--force",
    "-rf",
    "-r -f",
    "-fr",
    "--no-preserve-root",
    "/dev/sd",
    "/dev/nvme",
    "/dev/disk",
    "--hard",
    "-9 ",
    " -KILL",
];

fn destructive_set() -> HashSet<&'static str> {
    DESTRUCTIVE_COMMANDS.iter().copied().collect()
}

pub fn is_destructive(command: &str, args: &[String]) -> bool {
    let set = destructive_set();
    if set.contains(command) {
        return true;
    }
    if command == "git" {
        if let Some(sub) = args.first() {
            if matches!(sub.as_str(), "push" | "reset" | "clean" | "force-push") {
                return true;
            }
        }
    }
    let joined = format!("{} {}", command, args.join(" "));
    for flag in DESTRUCTIVE_SUBSTRINGS {
        if joined.contains(flag) {
            return true;
        }
    }
    false
}

pub fn explain(command: &str, args: &[String]) -> String {
    format!(
        "the command \"{} {}\" is destructive. rerun with --yes to allow it, or do it yourself",
        command,
        args.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm_is_destructive() {
        assert!(is_destructive("rm", &["-rf".to_string(), "/tmp/x".to_string()]));
    }

    #[test]
    fn ls_is_safe() {
        assert!(!is_destructive("ls", &["-la".to_string()]));
    }

    #[test]
    fn sudo_is_destructive() {
        assert!(is_destructive("sudo", &["apt".to_string(), "upgrade".to_string()]));
    }

    #[test]
    fn git_push_is_destructive() {
        assert!(is_destructive("git", &["push".to_string(), "origin".to_string(), "main".to_string()]));
    }

    #[test]
    fn git_status_is_safe() {
        assert!(!is_destructive("git", &["status".to_string()]));
    }

    #[test]
    fn force_flag_caught_even_on_safe_command() {
        assert!(is_destructive("cargo", &["build".to_string(), "--force".to_string()]));
    }

    #[test]
    fn no_preserve_root_caught() {
        assert!(is_destructive(
            "rm",
            &["--no-preserve-root".to_string(), "/".to_string()]
        ));
    }

    #[test]
    fn cat_is_safe() {
        assert!(!is_destructive("cat", &["file.txt".to_string()]));
    }

    #[test]
    fn explain_names_the_command() {
        let msg = explain("rm", &vec!["-rf".to_string(), "/tmp/x".to_string()]);
        assert!(msg.contains("rm"));
        assert!(msg.contains("--yes"));
    }
}
