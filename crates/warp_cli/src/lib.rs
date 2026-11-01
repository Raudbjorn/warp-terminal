#![cfg_attr(target_family = "wasm", allow(dead_code))]

use std::path::Path;
use std::{env, fmt};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use url::Url;
use warp_core::channel::ChannelState;
use warp_core::features::FeatureFlag;

use crate::agent::OutputFormat;

#[cfg(windows)]
mod process_handle;

pub mod artifact;
pub mod scope;
pub mod skill;
mod sort_order;
pub use sort_order::SortOrderArg;

pub mod agent;
pub mod api_key;
pub mod completions;
pub mod config_file;
mod date_time;
pub mod environment;
pub mod environment_checkout;
pub mod federate;
pub mod harness_support;
pub mod integration;
pub mod json_filter;
pub mod local_control;
pub mod mcp;
pub mod memory_store;
pub mod model;
pub mod provider;
pub mod runner;
pub mod schedule;
pub mod secret;
pub mod share;
pub mod task;
// Each of these variables is injected under both its `OZ_` and its `WARP_` name, carrying the
// identical value. Read sites still use the `OZ_` names.
pub const OZ_RUN_ID_ENV: &str = "OZ_RUN_ID";
pub const WARP_RUN_ID_ENV: &str = "WARP_RUN_ID";
pub const OZ_PARENT_RUN_ID_ENV: &str = "OZ_PARENT_RUN_ID";
pub const WARP_PARENT_RUN_ID_ENV: &str = "WARP_PARENT_RUN_ID";
pub const OZ_CLI_ENV: &str = "OZ_CLI";
pub const WARP_CLI_ENV: &str = "WARP_CLI";
pub const OZ_HARNESS_ENV: &str = "OZ_HARNESS";
pub const WARP_HARNESS_ENV: &str = "WARP_HARNESS";
pub const SERVER_ROOT_URL_OVERRIDE_ENV: &str = "WARP_SERVER_ROOT_URL";
pub const WS_SERVER_URL_OVERRIDE_ENV: &str = "WARP_WS_SERVER_URL";
pub const SESSION_SHARING_SERVER_URL_OVERRIDE_ENV: &str = "WARP_SESSION_SHARING_SERVER_URL";
pub const PREWARMED_SANDBOX_CONTROL_URL_OVERRIDE_ENV: &str = "WARP_PREWARMED_SANDBOX_CONTROL_URL";
pub const WARP_PREWARMED_SANDBOX_ID_ENV: &str = "WARP_PREWARMED_SANDBOX_ID";

/// Options related to the parent process that spawned this Warp instance.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct ParentOpts {
    /// The ID of the Warp process that spawned this one.
    ///
    /// Used by codepaths that attempt to detect when the parent Warp process
    /// has terminated. Guaranteed to be [`None`] when this is the initial
    /// Warp process, but may also be [`None`] for Warp child processes if the
    /// child process doesn't need to keep track of its parent.
    #[arg(long = "parent-pid", hide = true)]
    pub pid: Option<u32>,

    /// A handle to our parent process.
    ///
    /// Used on Windows for crash recovery instead of parent_pid, as process
    /// IDs can be reused, so a process handle is more robust.
    #[cfg(windows)]
    #[arg(long = "parent-handle", hide = true)]
    pub handle: Option<process_handle::ProcessHandle>,
}

/// Returns whether an argument requests one of Warp's hidden worker modes.
pub fn is_worker_invocation(arg: &str) -> bool {
    let command = WorkerCommand::augment_subcommands(clap::Command::new("worker"));
    command.find_subcommand(arg).is_some()
        || arg.strip_prefix("--").is_some_and(|long_flag| {
            command
                .get_subcommands()
                .any(|subcommand| subcommand.get_long_flag() == Some(long_flag))
        })
}

/// Hidden worker args used to scope remote-server proxy/daemon sockets by
/// Warp identity without exposing credentials.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct RemoteServerIdentityArgs {
    /// Non-secret identity partition key for the remote-server daemon.
    #[arg(long = "identity-key", hide = true)]
    pub identity_key: String,
}

/// Global options that apply to all CLI commands.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct GlobalOptions {
    /// API key for server authentication.
    #[arg(
        long = "api-key",
        global = true,
        env = "WARP_API_KEY",
        hide_env_values = true
    )]
    pub api_key: Option<String>,

    /// Set the output format.
    #[arg(
        long = "output-format",
        global = true,
        value_enum,
        default_value_t = OutputFormat::Pretty,
        env = "WARP_OUTPUT_FORMAT"
    )]
    pub output_format: OutputFormat,
}

/// Normal argument parser for the shared Warp executable across all channels.
///
/// Oz commands are subcommands of this parser, so invoking an `oz` symlink does
/// not require a mode flag. Warp Control uses its separate [`local_control::ControlArgs`]
/// parser, selected before this parser sees the arguments.
#[derive(Debug, Default, Parser, Clone)]
#[command(
    name = "oz",
    display_name = "Oz",
    about = r#"The orchestration platform for cloud agents

The Oz CLI is a tool for running, managing, and orchestrating coding agents at scale.
Use the CLI to:
* Launch and inspect cloud agents
* Schedule cloud agents to run in the future
* Manage the environments that cloud agents run in
* Upload secrets to Oz's secure storage"#
)]
#[clap(subcommand_precedence_over_arg = true)]
pub struct Args {
    #[clap(flatten)]
    global_options: GlobalOptions,

    /// Enable debug mode.
    #[arg(long = "debug", global = true, help = "Enable debug logging")]
    debug: bool,

    /// Override the server root URL.
    #[arg(
        long = "server-root-url",
        global = true,
        hide = true,
        env = "WARP_SERVER_ROOT_URL"
    )]
    server_root_url: Option<String>,

    /// Override the websocket server URL.
    #[arg(
        long = "ws-server-url",
        global = true,
        hide = true,
        env = "WARP_WS_SERVER_URL"
    )]
    ws_server_url: Option<String>,

    /// Override the session sharing server URL.
    #[arg(
        long = "session-sharing-server-url",
        global = true,
        hide = true,
        env = "WARP_SESSION_SHARING_SERVER_URL"
    )]
    session_sharing_server_url: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,

    #[clap(flatten)]
    args: AppArgs,
}

/// Flags for the Warp application. Additional binaries, like test runners, may use this type
/// along with their own flags, or convert their flags into an `AppArgs` value.
#[derive(Debug, Default, clap::Args, Clone)]
pub struct AppArgs {
    /// True if this instance of Warp was launched at the end of the auto-update process.
    #[arg(long = "finish-update", hide = true)]
    pub finish_update: bool,

    /// Crash recovery mechanism to use if we detect the parent process terminated.
    #[cfg(enable_crash_recovery)]
    #[arg(long = "crash-recovery-mechanism", value_enum, requires = "ParentOpts")]
    pub crash_recovery_mechanism: Option<RecoveryMechanism>,

    /// Options related to the parent process that spawned this Warp instance.
    #[clap(flatten)]
    pub parent: ParentOpts,

    /// URLs to open in Warp.
    #[arg(hide = true)]
    pub urls: Vec<Url>,
}

impl Args {
    /// Parses command-line arguments from the operating environment. May exit early if arguments
    /// are incorrectly specified.
    pub fn from_env() -> Self {
        cfg_if::cfg_if! {
            // wasm doesn't have any concept of an environment, so skip parsing and return defaults
            if #[cfg(target_family = "wasm")] {
                Args::default()
            } else {
                use clap::FromArgMatches as _;

                let args: Vec<String> = env::args().collect();
                let command = Self::clap_command();
                let disabled = disabled_subcommands();

                // Check for disabled commands before parsing to prevent help from showing (e.g.
                // `warp environment` should not return help text).
                exit_if_disabled_subcommand(&command, &args, &disabled);

                let matches = command.try_get_matches().unwrap_or_else(|err| {
                    // We attach a console to ensure help and error messages are printed
                    // when using the CLI.
                    #[cfg(windows)]
                    warp_util::windows::attach_to_parent_console();
                    err.exit()
                });

                // The scan above only exists to keep help for a disabled subcommand from printing.
                // clap's own reading of the arguments has the last word on what runs, so a scan
                // that disagrees with it can show help but never lets a disabled command through.
                if let Some(name) = parsed_disabled_subcommand(&matches, &disabled) {
                    exit_unrecognized_subcommand(name);
                }

                Self::from_arg_matches(&matches).unwrap_or_else(|err| {
                    #[cfg(windows)]
                    warp_util::windows::attach_to_parent_console();
                    err.exit()
                })
            }
        }
    }

    /// Construct the [`clap::Command`] that backs `Args`.
    ///
    /// IMPORTANT: use this instead of [`CommandFactory::command`], since we customize the command at runtime.
    pub fn clap_command() -> clap::Command {
        let mut command = <Args as CommandFactory>::command();

        if ChannelState::is_local_only() {
            command = apply_local_only_help(command);
        }

        // Hide the environment subcommands and --environment flags from help text
        if !FeatureFlag::CloudEnvironments.is_enabled() {
            command = command.mut_subcommand("environment", |c| c.hide(true));
            command = command.mut_subcommand("agent", |agent_cmd| {
                agent_cmd
                    .mut_subcommand("run", |run_cmd| {
                        run_cmd.mut_arg("environment", |arg| arg.hide(true))
                    })
                    .mut_subcommand("run-cloud", |cloud_cmd| {
                        cloud_cmd.mut_arg("environment", |arg| arg.hide(true))
                    })
            });
        }

        // Hide the --conversation flag from help text
        if !FeatureFlag::CloudConversations.is_enabled() {
            command = command.mut_subcommand("agent", |agent_cmd| {
                agent_cmd
                    .mut_subcommand("run", |run_cmd| {
                        run_cmd.mut_arg("conversation", |arg| arg.hide(true))
                    })
                    .mut_subcommand("run-cloud", |cloud_cmd| {
                        cloud_cmd.mut_arg("conversation", |arg| arg.hide(true))
                    })
            });
        }

        if !FeatureFlag::AmbientAgentsCommandLine.is_enabled() {
            command = command.mut_subcommand("agent", |agent_cmd| {
                agent_cmd.mut_subcommand("run-cloud", |c| c.hide(true))
            });
        }

        // Hide the third-party harness flags on `run-cloud` when the harness
        // feature is off, so `--help` matches the runtime gating (a non-oz
        // `--harness` is rejected unless AgentHarness is enabled).
        if !FeatureFlag::AgentHarness.is_enabled() {
            command = command.mut_subcommand("agent", |agent_cmd| {
                agent_cmd.mut_subcommand("run-cloud", |cloud_cmd| {
                    cloud_cmd
                        .mut_arg("harness", |arg| arg.hide(true))
                        .mut_arg("claude_auth_secret", |arg| arg.hide(true))
                        .mut_arg("codex_auth_secret", |arg| arg.hide(true))
                })
            });
        }

        // Hide the provider subcommand from help text
        if !FeatureFlag::ProviderCommand.is_enabled() {
            command = command.mut_subcommand("provider", |c| c.hide(true));
        }

        // Hide the integration subcommand from help text
        if !FeatureFlag::IntegrationCommand.is_enabled() {
            command = command.mut_subcommand("integration", |c| c.hide(true));
        }

        // Hide the schedule subcommand from help text.
        if !FeatureFlag::ScheduledAmbientAgents.is_enabled() {
            command = command.mut_subcommand("schedule", |c| c.hide(true));
        }

        // Hide the secret subcommand from help text.
        if !FeatureFlag::WarpManagedSecrets.is_enabled() {
            command = command.mut_subcommand("secret", |c| c.hide(true));
        }

        // Hide the federate subcommand from help text.
        if !FeatureFlag::OzIdentityFederation.is_enabled() {
            command = command.mut_subcommand("federate", |c| c.hide(true));
        }

        // Hide the harness-support subcommand from help text.
        if !FeatureFlag::AgentHarness.is_enabled() {
            command = command.mut_subcommand("harness-support", |c| c.hide(true));
        }

        // Hide the conversation subcommand and --conversation flag from help text.
        if !FeatureFlag::ConversationApi.is_enabled() {
            command = command.mut_subcommand("run", |run_cmd| {
                run_cmd
                    .mut_subcommand("conversation", |c| c.hide(true))
                    .mut_subcommand("get", |get_cmd| {
                        get_cmd.mut_arg("conversation", |arg| arg.hide(true))
                    })
            });
        }

        // Hide the artifact subcommand from help text.
        if !FeatureFlag::ArtifactCommand.is_enabled() {
            command = command.mut_subcommand("artifact", |c| c.hide(true));
        }

        // Hide the api-key subcommand from help text.
        if !FeatureFlag::APIKeyManagement.is_enabled() {
            command = command.mut_subcommand("api-key", |c| c.hide(true));
        }

        // Hide the runner subcommand from help text.
        if !FeatureFlag::CloudAgentRunners.is_enabled() {
            command = command.mut_subcommand("runner", |c| c.hide(true));
        }

        // Wire up `--version` / `-V` using the same version metadata used elsewhere in the
        // app, so the CLI reports the build's release tag.
        command = command.version(version_string());

        // Substitute the actual binary name into help output. Ideally clap would do this for us.
        let bin_name =
            binary_name().unwrap_or_else(|| ChannelState::channel().cli_command_name().to_string());
        command = if ChannelState::is_local_only() {
            command.after_help(color_print::cformat!(
                r#"<bold><underline>Examples:</underline></bold>

  <dim>$</dim> <bold>{bin_name} mcp list</bold>

  <dim>$</dim> <bold>{bin_name} completions powershell</bold>

<bold><underline>Learn more:</underline></bold>
* Use <bold>{bin_name} help</bold> to learn more about available local commands
"#
            ))
        } else {
            command.after_help(color_print::cformat!(
                r#"<bold><underline>Examples:</underline></bold>

  <dim>$</dim> <bold>{bin_name} agent run --prompt "Build anything"</bold>

  <dim>$</dim> <bold>{bin_name} mcp list</bold>

<bold><underline>Learn more:</underline></bold>
* Use <bold>{bin_name} help</bold> to learn more about each command
* Read the documentation at https://docs.warp.dev/reference/cli
"#
            ))
        };

        command
    }

    /// The requested subcommand, if any.
    pub fn command(&self) -> Option<&Command> {
        self.command.as_ref()
    }

    /// Args for the main Warp application, if not running a subcommand.
    pub fn app_args(&self) -> &AppArgs {
        &self.args
    }

    /// Extract the main Warp application args.
    pub fn into_app_args(self) -> AppArgs {
        self.args
    }

    /// Returns the global options.
    pub fn global_options(&self) -> &GlobalOptions {
        &self.global_options
    }

    /// Returns the API key if provided.
    pub fn api_key(&self) -> Option<&String> {
        self.global_options.api_key.as_ref()
    }

    /// Returns the output format.
    pub fn output_format(&self) -> OutputFormat {
        self.global_options.output_format
    }

    /// Returns true if debug logging is enabled.
    pub fn debug(&self) -> bool {
        self.debug
    }

    pub fn server_root_url(&self) -> Option<&str> {
        self.server_root_url.as_deref()
    }

    pub fn ws_server_url(&self) -> Option<&str> {
        self.ws_server_url.as_deref()
    }

    pub fn session_sharing_server_url(&self) -> Option<&str> {
        self.session_sharing_server_url.as_deref()
    }
}

/// Subcommands that need Warp's online services, hidden from `--help` and refused in local-only
/// mode. These are canonical names: [`clap::Command::mut_subcommand`] panics on an alias, and an
/// alias typed by the user (`task` for `run`) is resolved to its canonical name when gating.
const LOCAL_ONLY_DISABLED_SUBCOMMANDS: &[&str] = &[
    "agent",
    "environment",
    "run",
    "model",
    "login",
    "logout",
    "whoami",
    "provider",
    "integration",
    "schedule",
    "secret",
    "federate",
    "harness-support",
    "artifact",
];

/// Rewrites the help text of `command` for local-only mode, hiding the subcommands that need
/// Warp online services.
fn apply_local_only_help(mut command: clap::Command) -> clap::Command {
    command = command
        .about("Local terminal CLI")
        .long_about("Local terminal CLI for commands that do not require Warp online services.");
    for subcommand in LOCAL_ONLY_DISABLED_SUBCOMMANDS {
        command = command.mut_subcommand(subcommand, |c| c.hide(true));
    }
    command
}

/// Top-level subcommands this build refuses to run, by canonical name: all of
/// [`LOCAL_ONLY_DISABLED_SUBCOMMANDS`] in local-only mode, plus every subcommand whose feature
/// flag is off.
#[cfg(not(target_family = "wasm"))]
fn disabled_subcommands() -> Vec<&'static str> {
    let mut disabled = Vec::new();
    if ChannelState::is_local_only() {
        disabled.extend_from_slice(LOCAL_ONLY_DISABLED_SUBCOMMANDS);
    }
    let flagged = [
        ("environment", FeatureFlag::CloudEnvironments),
        ("provider", FeatureFlag::ProviderCommand),
        ("integration", FeatureFlag::IntegrationCommand),
        ("schedule", FeatureFlag::ScheduledAmbientAgents),
        ("secret", FeatureFlag::WarpManagedSecrets),
        ("federate", FeatureFlag::OzIdentityFederation),
        ("artifact", FeatureFlag::ArtifactCommand),
        ("api-key", FeatureFlag::APIKeyManagement),
        ("runner", FeatureFlag::CloudAgentRunners),
    ];
    disabled.extend(
        flagged
            .into_iter()
            .filter(|(_, flag)| !flag.is_enabled())
            .map(|(name, _)| name),
    );
    disabled
}

/// Whether `command` has a top-level option named `--name` that consumes the next argument as its
/// value. An option that requires `=` never does.
#[cfg(not(target_family = "wasm"))]
fn long_option_takes_next_arg(command: &clap::Command, name: &str) -> bool {
    command.get_arguments().any(|arg| {
        arg.get_action().takes_values()
            && !arg.is_require_equals_set()
            && (arg.get_long() == Some(name)
                || arg
                    .get_all_aliases()
                    .is_some_and(|aliases| aliases.contains(&name)))
    })
}

/// Same as [`long_option_takes_next_arg`], for a short option `-flag`.
#[cfg(not(target_family = "wasm"))]
fn short_option_takes_next_arg(command: &clap::Command, flag: char) -> bool {
    command.get_arguments().any(|arg| {
        arg.get_action().takes_values()
            && !arg.is_require_equals_set()
            && (arg.get_short() == Some(flag)
                || arg
                    .get_all_short_aliases()
                    .is_some_and(|aliases| aliases.contains(&flag)))
    })
}

/// The first `limit` arguments of `args` that are neither options nor option values, up to `--`.
///
/// This reads argv before clap does, so that nothing clap would print for a disabled subcommand
/// (help, usage in an error) is shown. Which options consume a separate value comes from
/// `command`, so a new option cannot fall out of step with this scan. Every top-level option takes
/// zero or one value; an option with more would need handling here.
#[cfg(not(target_family = "wasm"))]
fn leading_positionals<'a>(
    command: &clap::Command,
    args: &'a [String],
    limit: usize,
) -> Vec<&'a str> {
    let mut found = Vec::new();
    // `args[0]` is the program name.
    let mut index = 1;
    while index < args.len() && found.len() < limit {
        let arg = args[index].as_str();
        index += 1;
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            // `--name=value` carries its value; `--name value` takes the next argument.
            if !long.contains('=') && long_option_takes_next_arg(command, long) {
                index += 1;
            }
        } else if let Some(cluster) = arg.strip_prefix('-').filter(|rest| !rest.is_empty()) {
            // A cluster of short options such as `-vk KEY`: the first one that takes a value
            // ends the cluster. It takes whatever follows it in the same argument (`-kKEY`), or
            // the next argument when nothing does.
            if let Some((offset, flag)) = cluster
                .char_indices()
                .find(|(_, flag)| short_option_takes_next_arg(command, *flag))
                && offset + flag.len_utf8() == cluster.len()
            {
                index += 1;
            }
        } else {
            found.push(arg);
        }
    }
    found
}

/// The subcommand `args` asks for, as typed, if it is one of `disabled` (canonical names). `help
/// <subcommand>` asks for that subcommand's help, so it counts as asking for the subcommand.
#[cfg(not(target_family = "wasm"))]
fn disabled_subcommand<'a>(
    command: &clap::Command,
    args: &'a [String],
    disabled: &[&str],
) -> Option<&'a str> {
    let requested = match leading_positionals(command, args, 2).as_slice() {
        ["help", target, ..] => *target,
        [name, ..] => *name,
        [] => return None,
    };
    // `find_subcommand` also matches aliases, which are not in `disabled`.
    let canonical = command
        .find_subcommand(requested)
        .map_or(requested, |subcommand| subcommand.get_name());
    disabled.contains(&canonical).then_some(requested)
}

/// The subcommand clap parsed, if it is one of `disabled`. `ArgMatches` names a subcommand
/// by its canonical name whichever alias was typed.
#[cfg(not(target_family = "wasm"))]
fn parsed_disabled_subcommand<'a>(
    matches: &'a clap::ArgMatches,
    disabled: &[&str],
) -> Option<&'a str> {
    matches
        .subcommand_name()
        .filter(|name| disabled.contains(name))
}

#[cfg(not(target_family = "wasm"))]
fn exit_if_disabled_subcommand(command: &clap::Command, args: &[String], disabled: &[&str]) {
    if let Some(name) = disabled_subcommand(command, args, disabled) {
        exit_unrecognized_subcommand(name);
    }
}

#[cfg(not(target_family = "wasm"))]
fn exit_unrecognized_subcommand(name: &str) -> ! {
    eprintln!("error: unrecognized subcommand '{name}'\n");
    eprintln!("For more information, try '--help'");
    std::process::exit(2);
}

/// Warp may spawn several worker processes - mostly servers that support the main application.
///
/// These subcommands run those worker processes, which are bundled into the Warp binary.
#[derive(Debug, Clone, Subcommand)]
pub enum WorkerCommand {
    /// Run the terminal server.
    #[clap(hide = true)]
    #[cfg(unix)]
    TerminalServer(TerminalServerArgs),

    /// oh-my-warp: attach to (or create) a durable warpkeep session for a shell.
    #[clap(hide = true)]
    #[cfg(unix)]
    Warpkeep(WarpkeepArgs),

    /// oh-my-warp: run the detached warpkeep session master. Not invoked directly.
    #[clap(hide = true)]
    #[cfg(unix)]
    WarpkeepMaster(WarpkeepMasterArgs),

    /// Run this process as the plugin host rather than the main app.
    #[cfg(feature = "plugin_host")]
    #[clap(long_flag = "plugin-host")]
    PluginHost {
        #[clap(flatten)]
        parent: ParentOpts,
    },

    /// Run the remote development server proxy over SSH stdio.
    /// Ensures the daemon is running, then bridges its stdin/stdout
    /// to the daemon via a Unix domain socket.
    #[cfg(not(target_family = "wasm"))]
    #[clap(hide = true)]
    RemoteServerProxy(RemoteServerIdentityArgs),

    /// Run the long-lived remote development server daemon.
    /// Listens on a Unix domain socket and accepts multiple concurrent
    /// connections from proxy processes.
    #[cfg(not(target_family = "wasm"))]
    #[clap(hide = true)]
    RemoteServerDaemon(RemoteServerIdentityArgs),

    /// Connect an Agent Client Protocol agent's stdin/stdout to the loopback socket the agent
    /// driver is listening on. Run from the driver's terminal session so the agent inherits the
    /// shell state established by environment setup commands.
    #[cfg(not(target_family = "wasm"))]
    #[clap(hide = true)]
    AcpBridge {
        /// File the driver wrote with the address to connect to, the token to present, and the
        /// agent command line.
        #[clap(long = "launch-file")]
        launch_file: std::path::PathBuf,
    },

    /// Print the remote-server protocol compatibility key.
    #[cfg(not(target_family = "wasm"))]
    #[clap(hide = true)]
    RemoteServerProtocolVersion,

    /// Run a headless ripgrep search worker.
    #[cfg(not(target_family = "wasm"))]
    #[clap(hide = true)]
    RipgrepSearch {
        #[clap(flatten)]
        parent: ParentOpts,
        #[clap(long = "ignore-case")]
        ignore_case: bool,
        #[clap(long = "multiline")]
        multiline: bool,
        /// Search pattern.
        pattern: String,
        /// Paths to search.
        paths: Vec<std::path::PathBuf>,
    },
}

/// CLI-related subcommands. The command-line interface to Warp isn't a full SDK (e.g. with language bindings),
/// but it allows scripting some Warp functionality.
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Interact with Oz.
    #[command(subcommand)]
    Agent(crate::agent::AgentCommand),

    /// Manage cloud environments.
    #[command(subcommand)]
    Environment(crate::environment::EnvironmentCommand),

    #[command(hide = true)]
    EnvironmentCheckout(crate::environment_checkout::EnvironmentCheckoutArgs),

    /// Manage MCP servers.
    #[command(subcommand)]
    MCP(crate::mcp::MCPCommand),

    /// Manage runs.
    #[command(subcommand, alias = "task")]
    Run(crate::task::TaskCommand),

    /// Manage available models.
    #[command(subcommand)]
    Model(crate::model::ModelCommand),
    /// Manage memory stores.
    #[command(subcommand, alias = "memory-stores")]
    MemoryStore(crate::memory_store::MemoryStoreCommand),
    /// Manage memories.
    #[command(subcommand)]
    Memory(crate::memory_store::MemoryCommand),

    /// Log in to Warp.
    Login,
    /// Log out of Warp.
    Logout,
    /// Print information about the logged-in user.
    Whoami,

    /// Manage providers.
    #[command(subcommand)]
    Provider(crate::provider::ProviderCommand),

    /// Manage integrations.
    #[command(subcommand)]
    Integration(crate::integration::IntegrationCommand),

    /// Create and manage scheduled Oz agents. Scheduled agents run a user-defined task periodically, according to a cron schedule.
    ///
    /// As a shorthand, the `schedule` command behaves identically to `schedule create`.
    Schedule(crate::schedule::ScheduleCommand),

    /// Manage secrets.
    #[command(subcommand)]
    Secret(crate::secret::SecretCommand),

    /// Issue and manage federated identity tokens.
    #[command(subcommand)]
    Federate(crate::federate::FederateCommand),

    /// Support commands for agent harnesses to integrate with Oz.
    #[command(hide = true)]
    HarnessSupport(crate::harness_support::HarnessSupportArgs),

    /// Manage artifacts.
    #[command(subcommand)]
    Artifact(crate::artifact::ArtifactCommand),

    /// Manage API keys.
    #[command(subcommand)]
    ApiKey(crate::api_key::ApiKeyCommand),

    /// Manage cloud agent runners.
    #[command(subcommand)]
    Runner(crate::runner::RunnerCommand),
}

impl CliCommand {
    /// Returns the command path used to identify this invocation in tracing.
    pub fn as_str_for_tracing(&self) -> &'static str {
        match self {
            CliCommand::Agent(command) => command.as_str_for_tracing(),
            CliCommand::Environment(command) => command.as_str_for_tracing(),
            CliCommand::EnvironmentCheckout(_) => "environment_checkout",
            CliCommand::MCP(command) => command.as_str_for_tracing(),
            CliCommand::Run(command) => command.as_str_for_tracing(),
            CliCommand::Model(command) => command.as_str_for_tracing(),
            CliCommand::Login => "login",
            CliCommand::Logout => "logout",
            CliCommand::Whoami => "whoami",
            CliCommand::Provider(command) => command.as_str_for_tracing(),
            CliCommand::Integration(command) => command.as_str_for_tracing(),
            CliCommand::Schedule(command) => command.as_str_for_tracing(),
            CliCommand::Secret(command) => command.as_str_for_tracing(),
            CliCommand::Federate(command) => command.as_str_for_tracing(),
            CliCommand::HarnessSupport(args) => args.command.as_str_for_tracing(),
            CliCommand::Artifact(command) => command.as_str_for_tracing(),
            CliCommand::ApiKey(command) => command.as_str_for_tracing(),
            CliCommand::MemoryStore(command) => command.as_str_for_tracing(),
            CliCommand::Memory(command) => command.as_str_for_tracing(),
            CliCommand::Runner(command) => command.as_str_for_tracing(),
        }
    }
}

/// A subcommand of the main Warp application. This includes all [`WorkerCommand`]s as well as app-specific debugging tools.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    #[clap(flatten)]
    Worker(WorkerCommand),

    /// Commands that make up the Warp CLI.
    #[clap(flatten)]
    CommandLine(Box<CliCommand>),

    /// Generate shell completions for your shell to stdout.
    ///
    ///
    /// For bash, add the following to ~/.bashrc:
    ///     source <(path/to/warp completions bash)
    ///
    /// For zsh, add the following to ~/.zshrc:
    ///     source <(path/to/warp completions zsh)
    ///
    /// For fish, add the following to ~/.config/fish/config.fish:
    ///     path/to/warp completions fish | source
    ///
    /// For Powershell, add the following to $PROFILE:
    ///     path\to\warp | Out-String | Invoke-Expression
    ///
    /// If no shell is provided, this defaults to the shell that Warp was run from.
    #[command(verbatim_doc_comment)]
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Option<clap_complete::aot::Shell>,
    },

    /// Print debugging information and exit.
    #[clap(long_flag = "dump-debug-info")]
    DumpDebugInfo,
    /// Print the JSON schema for the current Warp channel's settings and exit.
    #[cfg(not(target_family = "wasm"))]
    DumpSettingsSchema {
        /// Write the schema to this path instead of standard output.
        output_path: Option<std::path::PathBuf>,
    },

    /// Print telemetry events in production and exit.
    #[clap(long_flag = "print-telemetry-events", hide = true)]
    #[cfg(not(target_family = "wasm"))]
    PrintTelemetryEvents,
}

impl Command {
    /// Whether or not the Command should print to stdout.
    pub fn prints_to_stdout(&self) -> bool {
        match self {
            Command::Worker(_) => false,
            Command::CommandLine(_) | Command::DumpDebugInfo => true,
            Command::Completions { .. } => true,
            #[cfg(not(target_family = "wasm"))]
            Command::DumpSettingsSchema { output_path } => output_path.is_none(),
            #[cfg(not(target_family = "wasm"))]
            Command::PrintTelemetryEvents => true,
        }
    }
}

/// Arguments for the terminal server.
#[cfg(not(windows))]
#[derive(Debug, Clone, Default, clap::Args)]
pub struct TerminalServerArgs {
    #[clap(flatten)]
    pub parent: ParentOpts,
}

/// oh-my-warp: arguments for the `warpkeep` attach worker.
#[cfg(unix)]
#[derive(Debug, Clone, clap::Args)]
pub struct WarpkeepArgs {
    /// Directory holding warpkeep session sockets.
    #[clap(long)]
    pub dir: std::path::PathBuf,

    /// The shell program and its arguments to run inside the session (after `--`).
    #[clap(last = true, required = true)]
    pub command: Vec<String>,
}

/// oh-my-warp: arguments for the detached `warpkeep-master` worker.
#[cfg(unix)]
#[derive(Debug, Clone, clap::Args)]
pub struct WarpkeepMasterArgs {
    /// Path to the Unix socket this master should bind.
    #[clap(long)]
    pub socket: std::path::PathBuf,

    /// The shell program and its arguments to run (after `--`).
    #[clap(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Copy, Clone, clap::ValueEnum)]
pub enum RecoveryMechanism {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[value(name = "force-x11")]
    X11,
    #[value(name = "force-dedicated-gpu")]
    DedicatedGpu,
    #[value(name = "disable-opengl")]
    DisableOpenGL,
    #[value(name = "force-vulkan")]
    ForceVulkan,
}

impl fmt::Display for RecoveryMechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.to_possible_value().expect("no values are skipped");
        f.write_str(value.get_name())
    }
}

/// Returns the subcommand name to use for starting the terminal server.
pub fn terminal_server_subcommand() -> String {
    <Args as CommandFactory>::command()
        .find_subcommand("terminal-server")
        .expect("terminal-server subcommand not found")
        .get_name()
        .to_string()
}

/// Returns the subcommand name to use for starting the installation detection server.
pub fn installation_detection_server_subcommand() -> String {
    <Args as CommandFactory>::command()
        .find_subcommand("installation-detection-server")
        .expect("installation-detection-server subcommand not found")
        .get_name()
        .to_string()
}

/// Returns the subcommand name to use for starting the ripgrep search worker.
#[cfg(not(target_family = "wasm"))]
pub fn ripgrep_search_subcommand() -> String {
    <Args as CommandFactory>::command()
        .find_subcommand("ripgrep-search")
        .expect("ripgrep-search subcommand not found")
        .get_name()
        .to_string()
}

/// Returns the flag to use when finishing the auto-update process.
pub fn finish_update_flag() -> String {
    let command = <Args as CommandFactory>::command();
    let flag = command
        .get_arguments()
        .find(|arg| arg.get_long() == Some("finish-update"))
        .expect("finish-update flag not found")
        .get_long()
        .unwrap();
    format!("--{flag}")
}

/// Returns the flag to use for the dump-debug-info subcommand.
pub fn dump_debug_info_flag() -> String {
    let command = <Args as CommandFactory>::command();
    let flag = command
        .find_subcommand("dump-debug-info")
        .expect("dump-debug-info subcommand not found")
        .get_long_flag()
        .expect("dump-debug-info flag not found");
    format!("--{flag}")
}

/// Returns a flag that sets the current process as the parent of a Warp subcommand to spawn.
pub fn parent_flag() -> String {
    let command = <Args as CommandFactory>::command();
    let flag = command
        .get_arguments()
        .find(|arg| arg.get_long() == Some("parent-pid"))
        .expect("parent-pid flag not found")
        .get_long()
        .unwrap();
    format!("--{flag}={}", std::process::id())
}

/// The name that this binary was invoked as.
pub fn binary_name() -> Option<String> {
    // Adapted from https://github.com/clap-rs/clap/blob/2c04acd3607e5c4676477ca14948419bb31c73a1/clap_builder/src/builder/command.rs#L888-L902
    // Unfortunately, we can't use Command::get_bin_name because it's not populated until args are parsed.
    let arg0 = env::args().next()?;
    Path::new(&arg0).file_name()?.to_str().map(|s| s.to_owned())
}

/// The version string shown for `--version` / `-V`.
///
/// Sourced from [`ChannelState::app_version`], which is populated from the
/// `GIT_RELEASE_TAG` env var at compile time. Falls back to a placeholder for
/// untagged builds (e.g. local `cargo run`).
pub fn version_string() -> &'static str {
    ChannelState::app_version().unwrap_or("<unknown>")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
