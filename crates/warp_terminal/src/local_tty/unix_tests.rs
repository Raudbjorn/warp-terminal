use super::*;

fn shell_starter(shell_type: ShellType, shell_path: &str) -> DirectShellStarter {
    DirectShellStarter::new_for_test(shell_type, PathBuf::from(shell_path), Vec::new())
}

fn env_value(command: &Command, key: &str) -> Option<Option<String>> {
    command
        .get_envs()
        .find(|(env_key, _)| *env_key == std::ffi::OsStr::new(key))
        .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
}

#[test]
fn host_bash_command_sets_history_size_sentinels() {
    let command = build_host_shell_command(
        shell_starter(ShellType::Bash, "/bin/bash"),
        None,
        HashMap::new(),
        None,
        false,
        false,
        false,
        false,
        true,
        None,
    );

    assert_eq!(
        env_value(&command, "HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
}

#[test]
fn host_non_bash_command_does_not_set_history_size_sentinels() {
    let command = build_host_shell_command(
        shell_starter(ShellType::Zsh, "/bin/zsh"),
        None,
        HashMap::new(),
        None,
        false,
        false,
        false,
        false,
        true,
        None,
    );

    assert_eq!(env_value(&command, "HISTFILESIZE"), None);
    assert_eq!(env_value(&command, "HISTSIZE"), None);
    assert_eq!(env_value(&command, "WARP_INITIAL_HISTFILESIZE"), None);
    assert_eq!(env_value(&command, "WARP_INITIAL_HISTSIZE"), None);
}

#[test]
fn docker_sandbox_command_sets_history_size_sentinels() {
    let docker_starter =
        DockerSandboxShellStarter::new(shell_starter(ShellType::Bash, "sbx"), None);
    let command = build_docker_sandbox_command(
        &docker_starter,
        None,
        HashMap::new(),
        false,
        false,
        false,
        false,
        true,
    );

    assert_eq!(
        env_value(&command, "HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTFILESIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
    assert_eq!(
        env_value(&command, "WARP_INITIAL_HISTSIZE"),
        Some(Some(BASH_HISTORY_SIZE_SENTINEL.to_owned()))
    );
}

const HARP_PLUGIN_DIR: &str = "/opt/warp/resources/bundled/claude-plugin/harp";

fn bash_command_with_plugin_dir(plugin_dir: Option<PathBuf>) -> Command {
    bash_command_with_env_and_plugin_dir(HashMap::new(), plugin_dir)
}

fn bash_command_with_env_and_plugin_dir(
    env_vars: HashMap<OsString, OsString>,
    plugin_dir: Option<PathBuf>,
) -> Command {
    build_host_shell_command(
        shell_starter(ShellType::Bash, "/bin/bash"),
        None,
        env_vars,
        None,
        false,
        false,
        false,
        false,
        true,
        plugin_dir,
    )
}

// Harp mode off (the default) must leave the user's Claude Code setup exactly as master does: no
// plugin dir is added, and an inherited HARP_AGENT_STATUS (e.g. Warp launched from a Harp-mode
// shell) is dropped so a plugin copy inherited the same way stays quiet.
#[test]
fn host_command_without_plugin_dir_injects_nothing() {
    let command = bash_command_with_plugin_dir(None);

    assert_eq!(env_value(&command, "CLAUDE_CODE_PLUGIN_DIRS"), None);
    assert_eq!(env_value(&command, "HARP_AGENT_STATUS"), Some(None));
}

#[test]
fn host_command_with_plugin_dir_injects_it_and_the_status_gate() {
    let command = bash_command_with_plugin_dir(Some(PathBuf::from(HARP_PLUGIN_DIR)));

    // The runner's own CLAUDE_CODE_PLUGIN_DIRS (if any) comes first; Harp's dir is appended last.
    let plugin_dirs = env_value(&command, "CLAUDE_CODE_PLUGIN_DIRS")
        .flatten()
        .expect("CLAUDE_CODE_PLUGIN_DIRS is set when a plugin dir is given");
    assert_eq!(plugin_dirs.split(':').next_back(), Some(HARP_PLUGIN_DIR));
    assert_eq!(
        env_value(&command, "HARP_AGENT_STATUS"),
        Some(Some("1".to_owned()))
    );
}

// A caller-provided list (launch config / tab env) is the base the plugin is appended to, once,
// instead of replacing the plugin wholesale.
#[test]
fn host_command_appends_plugin_dir_to_a_caller_provided_list_once() {
    let env_vars = HashMap::from([(
        OsString::from("CLAUDE_CODE_PLUGIN_DIRS"),
        OsString::from(format!("/mine:{HARP_PLUGIN_DIR}")),
    )]);
    let command =
        bash_command_with_env_and_plugin_dir(env_vars, Some(PathBuf::from(HARP_PLUGIN_DIR)));

    assert_eq!(
        env_value(&command, "CLAUDE_CODE_PLUGIN_DIRS"),
        Some(Some(format!("/mine:{HARP_PLUGIN_DIR}")))
    );
    assert_eq!(
        env_value(&command, "HARP_AGENT_STATUS"),
        Some(Some("1".to_owned()))
    );
}

#[test]
fn harp_claude_plugin_dirs_rejects_paths_that_break_the_list() {
    assert_eq!(
        harp_claude_plugin_dirs(Path::new(HARP_PLUGIN_DIR), Some("/mine".to_owned())),
        Some(format!("/mine:{HARP_PLUGIN_DIR}"))
    );
    // `:` is the list separator; such a path would be split into two bogus entries.
    assert_eq!(
        harp_claude_plugin_dirs(
            Path::new("/home/u/a:b/resources/bundled/claude-plugin/harp"),
            Some("/mine".to_owned())
        ),
        None
    );
}

// A Harp plugin inherited from another install (e.g. a dev build launched from a packaged
// Harp-mode Warp) is replaced, not kept next to ours, so its hooks do not fire twice.
#[test]
fn with_plugin_dir_replaces_another_copy_of_the_plugin() {
    let inherited = "/mine:/opt/other/resources/bundled/claude-plugin/harp:/theirs";
    assert_eq!(
        with_plugin_dir(Some(inherited.to_owned()), HARP_PLUGIN_DIR),
        format!("/mine:/theirs:{HARP_PLUGIN_DIR}")
    );
    // Only the trailing `claude-plugin/harp` identifies the plugin; another `harp` dir stays.
    assert_eq!(
        with_plugin_dir(Some("/x/harp".to_owned()), HARP_PLUGIN_DIR),
        format!("/x/harp:{HARP_PLUGIN_DIR}")
    );
}

#[test]
fn harp_plugin_dir_keeps_the_users_plugin_dirs() {
    let harp = "/Applications/Harp.app/Contents/Resources/bundled/claude-plugin/harp";
    assert_eq!(with_plugin_dir(None, harp), harp);
    assert_eq!(with_plugin_dir(Some(String::new()), harp), harp);
    assert_eq!(
        with_plugin_dir(Some("/mine".to_owned()), harp),
        format!("/mine:{harp}")
    );
    // A nested Harp shell inherits the list and must not add Harp's plugin twice.
    let inherited = format!("/mine:{harp}");
    assert_eq!(with_plugin_dir(Some(inherited.clone()), harp), inherited);
}
