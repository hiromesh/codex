use super::ExecApprovalRequest;
use super::ExecPolicyManager;
use super::commands_for_exec_policy_for_platform;
use crate::shell::Shell;
use crate::tools::sandboxing::ExecApprovalRequirement;
use std::path::Path;

impl ExecPolicyManager {
    pub(crate) async fn create_exec_approval_requirement_for_shell(
        &self,
        mut request: ExecApprovalRequest<'_>,
        configured_shell: &Shell,
    ) -> ExecApprovalRequirement {
        let command = request.command;
        let executable = shell_approval_command(command, configured_shell);
        if executable.len() == command.len() {
            return self
                .create_exec_approval_requirement_for_command_platform(request)
                .await;
        }

        let mut policy_commands = commands_for_exec_policy_for_platform(command);

        // Evaluate the executable alongside its apparent commands. Inner
        // commands can add restrictions, but cannot grant the executable trust.
        policy_commands.commands.insert(0, executable.to_vec());
        request.command = executable;
        self.create_exec_approval_requirement_for_parsed_commands(request, policy_commands)
            .await
    }
}

fn shell_approval_command<'a>(command: &'a [String], configured_shell: &Shell) -> &'a [String] {
    let Some(executable) = command.first() else {
        return command;
    };
    let executable_path = Path::new(executable);

    let is_system_shell = executable_path
        .parent()
        .is_some_and(|parent| parent == Path::new("/bin") || parent == Path::new("/usr/bin"));

    let is_configured_shell = executable_path == configured_shell.shell_path.as_path();

    if is_configured_shell || is_system_shell {
        command
    } else {
        // An unfamiliar executable can ignore its arguments, so evaluate the
        // executable separately from any restrictions on its apparent command.
        std::slice::from_ref(executable)
    }
}

#[cfg(test)]
#[path = "executable_identity_tests.rs"]
mod tests;
