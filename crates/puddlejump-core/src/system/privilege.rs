//! How a command that needs root gets it.

/// Whether commands that need root run directly or through `sudo`.
///
/// Decided once by the caller and passed in, so no package manager has its own
/// idea of how to get root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    /// The process already runs as root; run commands directly.
    AlreadyRoot,
    /// Run commands through `sudo`.
    UseSudo,
}

impl Privilege {
    /// Decides how to get root: directly if the process is already root,
    /// through `sudo` if it is available, otherwise not at all.
    pub fn resolve(is_root: bool, sudo_available: bool) -> Option<Self> {
        if is_root {
            Some(Privilege::AlreadyRoot)
        } else if sudo_available {
            Some(Privilege::UseSudo)
        } else {
            None
        }
    }

    /// The program and arguments to actually run so that `program` with `args`
    /// executes as root.
    pub fn wrap(self, program: &str, args: &[String]) -> (String, Vec<String>) {
        match self {
            Privilege::AlreadyRoot => (program.to_string(), args.to_vec()),
            Privilege::UseSudo => (
                "sudo".to_string(),
                std::iter::once(program.to_string())
                    .chain(args.iter().cloned())
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn root_uses_no_sudo_even_if_it_is_available() {
        assert_eq!(Privilege::resolve(true, true), Some(Privilege::AlreadyRoot));
        assert_eq!(
            Privilege::resolve(true, false),
            Some(Privilege::AlreadyRoot)
        );
    }

    #[test]
    fn a_normal_user_uses_sudo_when_it_is_available() {
        assert_eq!(Privilege::resolve(false, true), Some(Privilege::UseSudo));
    }

    #[test]
    fn a_normal_user_without_sudo_cannot_get_root() {
        assert_eq!(Privilege::resolve(false, false), None);
    }

    #[test]
    fn already_root_runs_the_command_unchanged() {
        let (program, arguments) = Privilege::AlreadyRoot.wrap("ln", &args(&["-s", "a", "b"]));

        assert_eq!(program, "ln");
        assert_eq!(arguments, args(&["-s", "a", "b"]));
    }

    #[test]
    fn use_sudo_puts_the_command_after_sudo() {
        let (program, arguments) = Privilege::UseSudo.wrap("ln", &args(&["-s", "a", "b"]));

        assert_eq!(program, "sudo");
        assert_eq!(arguments, args(&["ln", "-s", "a", "b"]));
    }

    #[test]
    fn use_sudo_with_no_arguments_still_names_the_program() {
        let (program, arguments) = Privilege::UseSudo.wrap("true", &[]);

        assert_eq!(program, "sudo");
        assert_eq!(arguments, args(&["true"]));
    }
}
