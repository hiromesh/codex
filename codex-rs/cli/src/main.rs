use clap::Args;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::Shell;
use clap_complete::generate;
use codex_app_server_daemon::BootstrapOptions as AppServerBootstrapOptions;
use codex_app_server_daemon::LifecycleCommand as AppServerLifecycleCommand;
use codex_app_server_daemon::RemoteControlMode as AppServerRemoteControlMode;
use codex_arg0::Arg0DispatchPaths;
use codex_arg0::arg0_dispatch_or_else;
use codex_chatgpt::apply_command::ApplyCommand;
use codex_chatgpt::apply_command::run_apply_command;
use codex_cli::cloud_config;
use codex_cli::read_access_token_from_stdin;
use codex_cli::read_api_key_from_stdin;
use codex_cli::run_login_status;
use codex_cli::run_login_with_access_token;
use codex_cli::run_login_with_api_key;
use codex_cli::run_login_with_browser;
use codex_cli::run_login_with_device_code;
use codex_cli::run_logout;
use codex_exec::Cli as ExecCli;
use codex_exec::Command as ExecCommand;
use codex_exec::ReviewArgs;
use codex_execpolicy::ExecPolicyCheckCommand;
use codex_rollout_trace::REDUCED_STATE_FILE_NAME;
use codex_rollout_trace::replay_bundle;
use codex_state::StateRuntime;
use codex_tui::AppExitInfo;
use codex_tui::Cli as TuiCli;
use codex_tui::ExitReason;
use codex_tui::UpdateAction;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_cli::CliConfigOverrides;
use codex_utils_cli::ProfileV2Name;
use codex_utils_cli::SharedCliOptions;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use supports_color::Stream;

#[cfg(all(
    target_os = "linux",
    target_env = "musl",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(target_os = "macos")]
mod app_cmd;
mod config_override_args;
mod daemon_install;
mod daemon_telemetry;
#[cfg(target_os = "macos")]
mod desktop_app;
mod doctor;
#[cfg(test)]
#[path = "exec_args_tests.rs"]
mod exec_args_tests;
#[cfg(test)]
#[path = "exec_server_args_tests.rs"]
mod exec_server_args_tests;
mod marketplace_cmd;
mod mcp_cmd;
mod mcp_login;
mod migrate_nfs_db;
mod migrate_rollouts;
mod plugin_cmd;
mod queue_cmd;
mod remote_control_cmd;
mod sandbox_uninstall;
mod state_db_recovery;
mod wsl_paths;

use crate::mcp_cmd::McpCli;
use crate::plugin_cmd::PluginCli;
use crate::plugin_cmd::PluginSubcommand;
use crate::queue_cmd::QueueCommand;
use crate::remote_control_cmd::RemoteControlCommand;
use codex_exec_server_cli::ExecServerCommand;
use doctor::DoctorCommand;
use state_db_recovery as local_state_db;

use codex_config::LoaderOverrides;
use codex_core::build_models_manager;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_core::config::ConfigOverrides;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config::find_codex_home;
use codex_core::config::resolve_profile_v2_config_path;
use codex_features::FEATURES;
use codex_features::Stage;
use codex_features::is_known_feature_key;
use codex_home::CodexHomeUserInstructionsProvider;
use codex_login::AuthManager;
use codex_memories_write::clear_memory_roots_contents;
use codex_models_manager::bundled_models_response;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::user_input::UserInput;
use codex_terminal_detection::TerminalName;

/// Codex CLI
///
/// If no subcommand is specified, options will be forwarded to the interactive CLI.
#[derive(Debug, Parser)]
#[clap(
    author,
    version,
    // If a sub‑command is given, ignore requirements of the default args.
    subcommand_negates_reqs = true,
    // The executable is sometimes invoked via a platform‑specific name like
    // `codex-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `codex` command name that users run.
    bin_name = "codex",
    override_usage = "codex [OPTIONS] [PROMPT]\n       codex [OPTIONS] <COMMAND> [ARGS]"
)]
struct MultitoolCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    pub feature_toggles: FeatureToggles,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    interactive: TuiCli,

    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    /// Open the agent command center to browse and switch sessions.
    Agents(AgentsCommand),

    /// Run Codex non-interactively.
    #[clap(visible_alias = "e")]
    Exec(ExecCli),

    /// Run a code review non-interactively.
    Review(ReviewCommand),

    /// Manage login.
    Login(LoginCommand),

    /// Remove stored authentication credentials.
    Logout(LogoutCommand),

    /// Manage external MCP servers for Codex.
    Mcp(McpCli),

    /// Manage Codex plugins.
    Plugin(PluginCli),

    /// [experimental] Run the app server or related tooling.
    AppServer(AppServerCommand),

    /// [experimental] Manage the app-server daemon with remote control enabled.
    RemoteControl(RemoteControlCommand),

    /// Launch the Desktop app (opens the app installer if missing).
    #[cfg(target_os = "macos")]
    App(app_cmd::AppCommand),

    /// Generate shell completion scripts.
    Completion(CompletionCommand),

    /// Update Codex to the latest version.
    Update,

    /// Diagnose local Codex installation, config, auth, and runtime health.
    Doctor(DoctorCommand),

    /// Run commands within a Codex-provided sandbox.
    Sandbox(SandboxCommand),

    /// Debugging tools.
    Debug(DebugCommand),

    /// Execpolicy tooling.
    #[clap(hide = true)]
    Execpolicy(ExecpolicyCommand),

    /// Apply the latest diff produced by Codex agent as a `git apply` to your local working tree.
    #[clap(visible_alias = "a")]
    Apply(ApplyCommand),

    /// Resume a previous interactive session (picker by default; use --last to continue the most recent).
    Resume(ResumeCommand),

    /// Queue a message for an existing session.
    Queue(QueueCommand),

    /// Archive a saved session by id or session name.
    Archive(SessionArchiveCommand),

    /// Permanently delete a saved session by id or session name.
    Delete(DeleteCommand),

    /// Inspect or migrate legacy local sessions to paginated thread history.
    MigrateRollouts(migrate_rollouts::MigrateRolloutsCommand),

    /// [experimental] Migrate configured SQLite databases to DELETE mode in place.
    MigrateNfsDb(migrate_nfs_db::MigrateNfsDbCommand),

    /// Unarchive a saved session by id or session name.
    Unarchive(SessionArchiveCommand),

    /// Fork a previous interactive session (picker by default; use --last to fork the most recent).
    Fork(ForkCommand),

    /// [EXPERIMENTAL] Run the standalone exec-server service.
    ExecServer(ExecServerCommand),

    /// Inspect feature flags.
    Features(FeaturesCli),
}

#[derive(Debug, Parser)]
struct CompletionCommand {
    /// Shell to generate completions for
    #[clap(value_enum, default_value_t = Shell::Bash)]
    shell: Shell,
}

#[derive(Debug, Parser)]
struct DebugCommand {
    #[command(subcommand)]
    subcommand: DebugSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugSubcommand {
    /// Render the raw model catalog as JSON.
    Models(DebugModelsCommand),

    /// Render the model-visible prompt input list as JSON.
    PromptInput(DebugPromptInputCommand),

    /// Replay a rollout trace bundle and write reduced state JSON.
    #[clap(hide = true)]
    TraceReduce(DebugTraceReduceCommand),

    /// Internal: reset local memory state for a fresh start.
    #[clap(hide = true)]
    ClearMemories,
}

#[derive(Debug, Parser)]
struct DebugPromptInputCommand {
    /// Optional user prompt to append after session context.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// Optional image(s) to attach to the user prompt.
    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    images: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
struct DebugModelsCommand {
    /// Skip refresh and dump only the bundled catalog shipped with this binary.
    #[arg(long = "bundled", default_value_t = false)]
    bundled: bool,
}

#[derive(Debug, Parser)]
struct ReviewCommand {
    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[clap(flatten)]
    args: ReviewArgs,
}

#[derive(Debug, Parser)]
struct DebugTraceReduceCommand {
    /// Trace bundle directory containing manifest.json and trace.jsonl.
    #[arg(value_name = "TRACE_BUNDLE")]
    trace_bundle: PathBuf,

    /// Output path for reduced RolloutTrace JSON. Defaults to TRACE_BUNDLE/state.json.
    #[arg(long = "output", short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AgentsCommand {
    /// Run the agents overview with an embedded app server.
    #[arg(long)]
    no_daemon: bool,
    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    /// Use this directory for new tasks on a remote server.
    #[arg(long = "cd", short = 'C', value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Disable alternate screen mode.
    #[arg(long = "no-alt-screen", default_value_t = false)]
    no_alt_screen: bool,
}

#[derive(Debug, Parser)]
struct ResumeCommand {
    /// Session id (UUID) or session name. UUIDs take precedence if it parses.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Continue the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    /// Include non-interactive sessions in the resume picker and --last selection.
    #[arg(long = "include-non-interactive", default_value_t = false)]
    include_non_interactive: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionTuiCli,
}

#[derive(Debug, Parser)]
struct SessionArchiveCommand {
    /// Session id (UUID) or session name. UUIDs take precedence if it parses.
    #[arg(value_name = "SESSION")]
    target: String,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionArchiveConfigOverrides,
}

#[derive(Debug, Args, Clone, Default)]
struct SessionArchiveConfigOverrides {
    #[clap(flatten)]
    shared: SharedCliOptions,

    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    #[clap(flatten)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Args)]
struct DeleteCommand {
    #[clap(flatten)]
    session: SessionArchiveCommand,

    /// Delete without prompting. SESSION must be a UUID.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Parser)]
struct ForkCommand {
    /// Conversation/session id (UUID). When provided, forks this session.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    session_id: Option<String>,

    /// Fork the most recent session without showing the picker.
    #[arg(long = "last", default_value_t = false)]
    last: bool,

    /// Show all sessions (disables cwd filtering and shows CWD column).
    #[arg(long = "all", default_value_t = false)]
    all: bool,

    #[clap(flatten)]
    remote: InteractiveRemoteOptions,

    #[clap(flatten)]
    config_overrides: SessionTuiCli,
}

/// TUI arguments for session commands where a parsed prompt implies an explicit session id.
///
/// This keeps `--last PROMPT` valid while rejecting `--last SESSION_ID PROMPT`.
#[derive(Debug)]
struct SessionTuiCli(TuiCli);

impl Args for SessionTuiCli {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        TuiCli::augment_args(cmd).mut_arg("prompt", |arg| arg.conflicts_with("last"))
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        TuiCli::augment_args_for_update(cmd).mut_arg("prompt", |arg| arg.conflicts_with("last"))
    }
}

impl clap::FromArgMatches for SessionTuiCli {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        TuiCli::from_arg_matches(matches).map(Self)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)
    }
}

#[derive(Debug, Parser)]
struct SandboxCommand {
    #[command(flatten)]
    host: HostSandboxArgs,

    #[command(subcommand)]
    subcommand: Option<SandboxSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum SandboxSubcommand {
    Uninstall(sandbox_uninstall::SandboxUninstallCommand),
}

#[cfg(target_os = "macos")]
type HostSandboxArgs = codex_cli::SeatbeltCommand;
#[cfg(target_os = "linux")]
type HostSandboxArgs = codex_cli::LandlockCommand;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
type HostSandboxArgs = UnsupportedSandboxArgs;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
#[derive(Debug, Parser)]
struct UnsupportedSandboxArgs {
    /// Layer $CODEX_HOME/<name>.config.toml on top of the base user config.
    #[arg(long = "profile", short = 'p')]
    pub config_profile: Option<ProfileV2Name>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Full command args to run under the host sandbox.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Parser)]
struct ExecpolicyCommand {
    #[command(subcommand)]
    sub: ExecpolicySubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum ExecpolicySubcommand {
    /// Check execpolicy files against a command.
    #[clap(name = "check")]
    Check(ExecPolicyCheckCommand),
}

#[derive(Debug, Parser)]
struct LoginCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[arg(
        long = "with-api-key",
        help = "Read the API key from stdin (e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`)"
    )]
    with_api_key: bool,

    #[arg(
        long = "with-access-token",
        help = "Read the access token from stdin (e.g. `printenv CODEX_ACCESS_TOKEN | codex login --with-access-token`)"
    )]
    with_access_token: bool,

    #[arg(
        long = "api-key",
        num_args = 0..=1,
        default_missing_value = "",
        value_name = "API_KEY",
        help = "(deprecated) Previously accepted the API key directly; now exits with guidance to use --with-api-key",
        hide = true
    )]
    api_key: Option<String>,

    #[arg(long = "device-auth")]
    use_device_code: bool,

    /// EXPERIMENTAL: Use custom OAuth issuer base URL (advanced)
    /// Override the OAuth issuer base URL (advanced)
    #[arg(long = "experimental_issuer", value_name = "URL", hide = true)]
    issuer_base_url: Option<String>,

    /// EXPERIMENTAL: Use custom OAuth client ID (advanced)
    #[arg(long = "experimental_client-id", value_name = "CLIENT_ID", hide = true)]
    client_id: Option<String>,

    #[command(subcommand)]
    action: Option<LoginSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum LoginSubcommand {
    /// Show login status.
    Status,
}

#[derive(Debug, Parser)]
struct LogoutCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Parser)]
struct AppServerCommand {
    #[command(flatten)]
    startup_config: codex_app_server::StartupConfigArgs,

    /// Load frozen cloud skills from a JSON snapshot instead of a live skill service.
    #[arg(long, value_name = "PATH")]
    cloud_skills_snapshot: Option<PathBuf>,
    /// Omit to run the app server; specify a subcommand for tooling.
    #[command(subcommand)]
    subcommand: Option<AppServerSubcommand>,

    /// Error out when config.toml contains fields that are not recognized by this version of Codex.
    #[arg(long = "strict-config", default_value_t = false)]
    strict_config: bool,

    /// Transport endpoint URL. Supported values: `stdio://` (default),
    /// `unix://`, `unix://PATH`, `ws://IP:PORT`, `off`.
    #[arg(
        long = "listen",
        value_name = "URL",
        default_value = codex_app_server::AppServerTransport::DEFAULT_LISTEN_URL
    )]
    listen: codex_app_server::AppServerTransport,

    /// Use stdio as the transport (equivalent to `--listen stdio://`).
    #[arg(long = "stdio", conflicts_with = "listen")]
    stdio: bool,

    /// Enable remote control for this app-server process without changing persistence.
    #[arg(long = "remote-control", hide = true)]
    remote_control: bool,

    /// Save loaded threads during managed daemon shutdown.
    #[arg(long, hide = true)]
    managed_daemon: bool,

    /// Controls whether analytics are enabled by default.
    ///
    /// Analytics are disabled by default for app-server. Users have to explicitly opt in
    /// via the `analytics` section in the config.toml file.
    ///
    /// However, for first-party use cases like the VSCode IDE extension, we default analytics
    /// to be enabled by default by setting this flag. Users can still opt out by setting this
    /// in their config.toml:
    ///
    /// ```toml
    /// [analytics]
    /// enabled = false
    /// ```
    ///
    /// See https://developers.openai.com/codex/config-advanced/#metrics for more details.
    #[arg(long = "analytics-default-enabled")]
    analytics_default_enabled: bool,

    #[command(flatten)]
    auth: codex_websocket_auth::WebsocketAuthArgs,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::enum_variant_names)]
enum AppServerSubcommand {
    /// Manage the local app-server daemon.
    Daemon(AppServerDaemonCommand),

    /// [experimental] Generate TypeScript bindings for the app server protocol.
    GenerateTs(GenerateTsCommand),

    /// [experimental] Generate JSON Schema for the app server protocol.
    GenerateJsonSchema(GenerateJsonSchemaCommand),

    /// [internal] Generate internal JSON Schema artifacts for Codex tooling.
    #[clap(hide = true)]
    GenerateInternalJsonSchema(GenerateInternalJsonSchemaCommand),
}

#[derive(Debug, Args)]
struct AppServerDaemonCommand {
    #[command(subcommand)]
    subcommand: AppServerDaemonSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum AppServerDaemonSubcommand {
    /// Install durable local app-server management for SSH-driven use.
    Bootstrap(AppServerBootstrapCommand),

    /// Start the local app server daemon if it is not already running.
    Start,

    /// Restart the local app server daemon.
    Restart,

    /// Update the daemon package (may interrupt running work).
    Update {
        /// Copy and pin this CLI package.
        #[arg(long)]
        from_cli: bool,
        /// Confirm replacing the daemon package without an interactive prompt.
        #[arg(short = 'y', long, requires = "from_cli")]
        yes: bool,
    },

    /// Enable remote control for future starts and a currently running managed daemon.
    EnableRemoteControl,

    /// Disable remote control for future starts and a currently running managed daemon.
    DisableRemoteControl,

    /// Stop the local app server daemon.
    Stop,

    /// Print local CLI and running app-server versions as JSON.
    Version,

    /// [internal] Run the detached pid-backed standalone updater loop.
    #[clap(hide = true)]
    PidUpdateLoop {
        /// Check support for daemon-owned packages without starting the updater.
        #[arg(long, hide = true)]
        check_package_ownership: bool,
        /// Authorize one production restoration of this selected release.
        #[arg(long, hide = true)]
        restore_release: Option<String>,
    },
}

#[derive(Debug, Args)]
struct AppServerBootstrapCommand {
    /// Launch the managed app-server with remote control enabled.
    #[arg(long = "remote-control")]
    remote_control: bool,
}

#[derive(Debug, Args)]
struct GenerateTsCommand {
    /// Output directory where .ts files will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Optional path to the Prettier executable to format generated files
    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateJsonSchemaCommand {
    /// Output directory where the schema bundle will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Include experimental methods and fields in the generated output
    #[arg(long = "experimental", default_value_t = false)]
    experimental: bool,
}

#[derive(Debug, Args)]
struct GenerateInternalJsonSchemaCommand {
    /// Output directory where internal JSON Schema artifacts will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,
}

/// Handle the app exit and print the results. Optionally run the update action.
fn handle_app_exit(
    exit_info: AppExitInfo,
    cli_executable: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let is_fatal = match &exit_info.exit_reason {
        ExitReason::Fatal(message) => {
            eprintln!("ERROR: {message}");
            true
        }
        ExitReason::UserRequested | ExitReason::TurnInterrupted => false,
    };

    let update_action = exit_info.update_action;
    if !matches!(update_action, Some(UpdateAction::Daemon(_))) {
        let color_enabled = supports_color::on(Stream::Stdout).is_some();
        for line in exit_info.format_exit_messages(color_enabled) {
            println!("{line}");
        }
    }
    if is_fatal {
        std::io::stdout().flush()?;
        std::process::exit(1);
    }
    if let Some(action) = update_action {
        run_update_action(action, cli_executable)?;
    }
    Ok(())
}

/// Run the update action and print the result.
fn run_update_action(
    action: UpdateAction,
    cli_executable: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    if let UpdateAction::Daemon(source) = action {
        let executable = cli_executable
            .ok_or_else(|| anyhow::anyhow!("Cannot locate the launching Codex CLI"))?;
        println!("Updating the local background server...");
        let status = std::process::Command::new(executable)
            .args(source.command_args())
            .env(codex_app_server_daemon::telemetry::HANDOFF_ENV, "1")
            .status()?;
        anyhow::ensure!(
            status.success(),
            "Daemon update failed with status {status}"
        );
        println!("Relaunch Codex to reconnect.");
        return Ok(());
    }
    println!();
    let cmd_str = action.command_str();
    println!("Updating Codex via `{cmd_str}`...");
    let status = {
        {
            let (cmd, args) = action.command_args();
            let command_path = crate::wsl_paths::normalize_for_wsl(cmd);
            let normalized_args: Vec<String> = args
                .iter()
                .map(crate::wsl_paths::normalize_for_wsl)
                .collect();
            std::process::Command::new(&command_path)
                .args(&normalized_args)
                // Package managers search parent directories for project config and hooks.
                .current_dir("/")
                .status()?
        }
    };
    if !status.success() {
        anyhow::bail!("`{cmd_str}` failed with status {status}");
    }
    println!("\n🎉 Update ran successfully! Please restart Codex.");
    Ok(())
}

