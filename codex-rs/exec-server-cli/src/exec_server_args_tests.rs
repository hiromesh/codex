#![cfg(test)]

use assert_matches::assert_matches;
use clap::error::ErrorKind;
use pretty_assertions::assert_eq;

use super::*;
use crate::ExecServerCli;

fn exec_server_from_args(args: &[&str]) -> ExecServerCommand {
    let cli = ExecServerCli::try_parse_from(
        ["codex-exec-server"]
            .into_iter()
            .chain(args.iter().copied()),
    )
    .expect("parse executor arguments");
    cli.command
}

#[test]
fn exec_server_defaults_preserve_local_and_noise_modes() {
    for args in [
        vec![],
        vec!["--listen", "stdio"],
        vec![
            "--remote",
            "https://registry.example.com",
            "--environment-id",
            "env-1",
        ],
    ] {
        let command = exec_server_from_args(&args);
        assert!(!command.executor_credential_stdin);
    }
}

#[test]
fn remote_forwarder_skips_cloud_config() {
    let command = exec_server_from_args(&[
        "forward",
        "--connect",
        "ws://localhost:8765",
        "--remote",
        "https://registry.example.com",
        "--environment-id",
        "env-1",
    ]);
    assert!(!command.should_load_cloud_config());
}

#[test]
fn exec_server_parses_executor_credential_options() {
    let args = [
        "--remote",
        "https://registry.example.com",
        "--environment-id",
        "env-1",
    ];
    for options in [
        vec!["--executor-credential-stdin"],
        vec!["--executor-credential-stdin", "--exit-on-stdin-close"],
    ] {
        let options: Vec<_> = args.iter().copied().chain(options).collect();
        let command = exec_server_from_args(&options);
        assert!(command.executor_credential_stdin);
    }
}

#[test]
fn exec_server_executor_credential_option_requires_registration_arguments() {
    let error = ExecServerCli::try_parse_from(
        ["codex-exec-server", "--executor-credential-stdin"].into_iter(),
    )
    .expect_err("require remote URL and environment ID");
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn exec_server_forward_parses_shared_remote_options() {
    let cli = ExecServerCli::try_parse_from([
        "codex-exec-server",
        "forward",
        "--connect",
        "ws://127.0.0.1:8765",
        "--remote",
        "https://example.openai.com",
        "--environment-id",
        "env-1",
        "--name",
        "forwarded",
        "--strict-config",
    ])
    .expect("parse forward");
    assert_matches!(cli.command, ExecServerCommand {
        command: Some(ExecServerSubcommand::Forward { connect }),
        remote: Some(remote),
        environment_id: Some(environment_id),
        name: Some(name),
        strict_config: true,
        ..
    } if connect == "ws://127.0.0.1:8765" && remote == "https://example.openai.com" && environment_id == "env-1" && name == "forwarded");
}

#[test]
fn standalone_help_documents_executor_options() {
    use clap::CommandFactory;
    let help = ExecServerCli::command()
        .term_width(80)
        .mut_arg("exit_on_stdin_close", |arg| arg.hide_env_values(true))
        .mut_arg("proxy_private_ips_via_upstream", |arg| {
            arg.hide_env_values(true)
        })
        .render_long_help()
        .to_string()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(help);
}
