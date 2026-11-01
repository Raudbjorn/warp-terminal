use warpui::App;

use super::*;
use crate::terminal::model::ansi::{Handler as _, InitShellValue};
use crate::terminal::model_events::ModelEvent;
use crate::test_util::terminal::{add_window_with_terminal, initialize_app_for_terminal_view};

const SSH_PROFILE_TEST_COMMAND: &str = "command ssh -p 2222 -- root@10.0.0.5";

fn start_block_with_command(
    view: &mut TerminalView,
    command: &str,
    ctx: &mut ViewContext<TerminalView>,
) {
    let block_id = view.model.lock().block_list().active_block_id().clone();
    view.handle_terminal_event(
        &ModelEvent::AfterBlockStarted {
            block_id,
            command: command.to_string(),
            is_for_in_band_command: false,
        },
        ctx,
    );
}

/// Drives the remote `InitShell` hook and reports whether the model acted on it. An honored hook
/// re-initializes the shell and appends a block; a suppressed one returns before that. This
/// observes suppression through public block-list state instead of the private model flag.
fn model_honors_remote_init_shell(view: &TerminalView) -> bool {
    let mut model = view.model.lock();
    let blocks_before = model.block_list().blocks().len();
    model.init_shell(InitShellValue {
        session_id: Default::default(),
        shell: "bash".to_string(),
        is_subshell: true,
        user: "root".to_string(),
        hostname: "remote".to_string(),
        wsl_name: None,
    });
    model.block_list().blocks().len() > blocks_before
}

#[test]
fn arming_an_ssh_profile_with_warpify_disabled_records_the_rendered_command() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.to_string(), false, ctx);

            assert!(!view.ssh_profile.warpification_enabled_at_arm);
            assert_eq!(
                view.ssh_profile.warpify_disabled_command.as_deref(),
                Some(SSH_PROFILE_TEST_COMMAND)
            );
        });
    })
}

#[test]
fn arming_an_ssh_profile_with_warpify_enabled_records_no_suppression_command() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.to_string(), true, ctx);

            assert!(view.ssh_profile.warpification_enabled_at_arm);
            assert_eq!(view.ssh_profile.warpify_disabled_command, None);
        });
    })
}

#[test]
fn observing_the_armed_command_suppresses_bootstrap_when_warpify_is_disabled() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            assert!(
                view.model.lock().block_list().is_bootstrapped(),
                "the InitShell probe only re-initializes the shell on a bootstrapped block list"
            );

            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.to_string(), false, ctx);
            start_block_with_command(view, SSH_PROFILE_TEST_COMMAND, ctx);

            assert!(
                !model_honors_remote_init_shell(view),
                "the armed profile command must suppress remote bootstrap hooks"
            );
        });
    })
}

#[test]
fn observing_the_armed_command_does_not_suppress_bootstrap_when_warpify_is_enabled() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.to_string(), true, ctx);
            start_block_with_command(view, SSH_PROFILE_TEST_COMMAND, ctx);

            assert!(
                model_honors_remote_init_shell(view),
                "Warpify-enabled profile connections must bootstrap normally"
            );
        });
    })
}

#[test]
fn observing_an_unrelated_command_does_not_suppress_bootstrap() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.to_string(), false, ctx);
            start_block_with_command(view, "ls -la", ctx);

            assert!(
                model_honors_remote_init_shell(view),
                "suppression must not leak onto commands the profile did not launch"
            );
        });
    })
}

const SSH_PROFILE_BARE_COMMAND: &str = "ssh -p 2222 -- root@10.0.0.5";

fn arm_warpify_disabled_profile(
    view: &mut TerminalView,
    command: &str,
    ctx: &mut ViewContext<TerminalView>,
) {
    view.arm_ssh_profile_connection(command.to_string(), false, ctx);
}

#[test]
fn suppression_matches_the_armed_command_through_the_command_builtin_prefix() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            arm_warpify_disabled_profile(view, SSH_PROFILE_BARE_COMMAND, ctx);
            start_block_with_command(view, "command ssh -p 2222 -- root@10.0.0.5", ctx);

            assert!(
                !model_honors_remote_init_shell(view),
                "the `command` builtin prefix is normalized away before matching"
            );
        });
    })
}

#[test]
fn suppression_matches_equivalent_ssh_argv() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            arm_warpify_disabled_profile(view, SSH_PROFILE_BARE_COMMAND, ctx);
            start_block_with_command(view, "ssh  -p 2222  --  root@10.0.0.5", ctx);

            assert!(
                !model_honors_remote_init_shell(view),
                "re-spaced commands still match while retaining every SSH option"
            );
        });
    })
}

#[test]
fn suppression_matches_commands_with_the_password_helper_prefix() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let mut profile = SshHostProfile::new("box".to_string(), "10.0.0.5".to_string());
        profile.user = Some("root".to_string());
        let command = format!(
            "{}{SSH_PROFILE_BARE_COMMAND}",
            askpass::env_prefix(&profile).unwrap()
        );

        terminal.update(&mut app, |view, ctx| {
            arm_warpify_disabled_profile(view, &command, ctx);
            start_block_with_command(view, &command, ctx);

            assert!(!model_honors_remote_init_shell(view));
        });
    })
}

/// Neither matcher recognizes an absolute `ssh` path: the string differs and the parser requires
/// a bare `ssh` argv[0].
#[test]
fn suppression_misses_absolute_ssh_paths() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            arm_warpify_disabled_profile(view, SSH_PROFILE_BARE_COMMAND, ctx);
            start_block_with_command(view, "/usr/bin/ssh -p 2222 -- root@10.0.0.5", ctx);

            assert!(
                model_honors_remote_init_shell(view),
                "an absolute ssh path matches neither the string nor the parsed target"
            );
        });
    })
}

fn watch_quick_connect(view: &mut TerminalView) {
    watch_quick_connect_in(view, None);
}

fn watch_quick_connect_in(view: &mut TerminalView, directory: Option<&str>) {
    view.ssh_profile.connection = Some(SshConnectionWatch::new(
        "ssh root@remote".to_string(),
        None,
        "root@remote".to_string(),
        None,
        directory.map(str::to_string),
    ));
}

#[test]
fn startup_directory_is_entered_once_per_connection_on_the_first_remote_prompt() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let local = SessionId::from(1);
        let remote = SessionId::from(2);
        let sent = |view: &TerminalView| {
            view.ssh_profile
                .connection
                .as_ref()
                .is_some_and(|connection| connection.startup_directory_sent)
        };

        terminal.update(&mut app, |view, ctx| {
            watch_quick_connect_in(view, Some("/srv/app"));
            view.active_block_metadata = Some(BlockMetadata::new(Some(local), None));
            view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(remote), None), ctx);
            assert!(!sent(view), "nothing is sent before ssh starts");

            start_block_with_command(view, "ssh root@remote", ctx);
            view.handle_ssh_manager_precmd(&BlockMetadata::new(None, None), ctx);
            assert!(
                !sent(view),
                "a precmd without a session is not the remote shell"
            );

            view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(remote), None), ctx);
            // Scheduling is not completion: mark the startup directory only when its own block starts.
            assert!(!sent(view));
            view.active_block_metadata = Some(BlockMetadata::new(Some(remote), None));
            start_block_with_command(view, "cd -- /srv/app", ctx);
            assert!(sent(view));

            view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(local), None), ctx);
            view.active_block_metadata = Some(BlockMetadata::new(Some(local), None));
            start_block_with_command(view, "ssh root@remote", ctx);
            assert!(!sent(view), "a reconnect enters the directory again");
        });
    })
}

#[test]
fn reconnect_banner_shows_when_the_local_shell_prompts_after_a_watched_ssh_command() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let local = SessionId::from(1);
        let remote = SessionId::from(2);

        terminal.update(&mut app, |view, ctx| {
            watch_quick_connect(view);
            view.active_block_metadata = Some(BlockMetadata::new(Some(local), None));
            start_block_with_command(view, "ssh root@remote", ctx);

            view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(remote), None), ctx);
            assert!(view.ssh_profile.reconnect_banner.is_none());

            view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(local), None), ctx);
            assert!(view.ssh_profile.reconnect_banner.is_some());

            start_block_with_command(view, "ssh root@remote", ctx);
            assert!(view.ssh_profile.reconnect_banner.is_none());
        });
    })
}

#[test]
fn reconnect_banner_ignores_local_prompts_before_the_watched_command_starts() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let local = SessionId::from(1);

        terminal.update(&mut app, |view, ctx| {
            watch_quick_connect(view);
            view.active_block_metadata = Some(BlockMetadata::new(Some(local), None));
            start_block_with_command(view, "ls", ctx);

            view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(local), None), ctx);
            assert!(view.ssh_profile.reconnect_banner.is_none());
        });
    })
}

#[test]
fn warpified_connections_go_through_the_ssh_wrapper() {
    let (command, control_path) = connection_command(
        "command ssh -p 2222 -- root@host".to_string(),
        true,
        ShellFamily::Posix,
    )
    .unwrap();
    assert_eq!(command, "ssh -p 2222 -- root@host");
    assert_eq!(control_path, None);
}

#[test]
fn an_old_suppression_timer_cannot_expire_a_rearmed_identical_command() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.into(), false, ctx);
            let old_generation = view.ssh_profile.suppression_generation;
            view.arm_ssh_profile_connection(SSH_PROFILE_TEST_COMMAND.into(), false, ctx);
            view.expire_ssh_profile_arm(old_generation);
            assert!(view.ssh_profile_warpify_disabled_command_matches(SSH_PROFILE_TEST_COMMAND));
            view.expire_ssh_profile_arm(view.ssh_profile.suppression_generation);
            assert!(view.ssh_profile.warpify_disabled_command.is_none());
        });
    });
}

#[test]
fn pending_connections_are_registered_only_after_their_command_starts() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            let id = Uuid::new_v4();
            watch_quick_connect(view);
            view.ssh_profile.connection.as_mut().unwrap().profile_id = Some(id);
            view.active_block_metadata = Some(BlockMetadata::new(Some(SessionId::from(1)), None));
            view.set_ssh_manager_attached(true, ctx);
            assert_eq!(SshOpenConnections::open_count(id, ctx), 0);
            start_block_with_command(view, "ssh root@remote", ctx);
            assert_eq!(SshOpenConnections::open_count(id, ctx), 1);
            view.set_ssh_manager_attached(false, ctx);
            assert_eq!(SshOpenConnections::open_count(id, ctx), 0);
        });
    });
}

#[test]
fn startup_directory_does_not_append_to_another_pending_command() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            watch_quick_connect_in(view, Some("/srv/app"));
            view.active_block_metadata = Some(BlockMetadata::new(Some(SessionId::from(1)), None));
            start_block_with_command(view, "ssh root@remote", ctx);
            view.set_pending_command("echo pending", ctx);
            let before = view.input.as_ref(ctx).buffer_text(ctx);
            view.handle_ssh_manager_precmd(
                &BlockMetadata::new(Some(SessionId::from(2)), None),
                ctx,
            );
            assert_eq!(view.input.as_ref(ctx).buffer_text(ctx), before);
            assert!(
                !view
                    .ssh_profile
                    .connection
                    .as_ref()
                    .unwrap()
                    .startup_directory_sent
            );
        });
    });
}

#[test]
fn a_missing_profile_keeps_its_reconnect_banner() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            watch_quick_connect(view);
            let connection = view.ssh_profile.connection.as_mut().unwrap();
            connection.profile_id = Some(Uuid::new_v4());
            connection.disconnected = true;
            view.show_ssh_reconnect_banner("removed".into(), None, false, ctx);
            let banner_id = view.ssh_profile.reconnect_banner.as_ref().unwrap().id();
            view.reconnect_ssh(true, ctx);
            assert_eq!(
                view.ssh_profile.reconnect_banner.as_ref().unwrap().id(),
                banner_id
            );
        });
    });
}

#[test]
fn non_warpified_remote_session_requests_poll_without_a_remote_prompt() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            watch_quick_connect(view);
            view.ssh_profile.connection.as_mut().unwrap().profile_id = Some(Uuid::new_v4());
            view.read_remote_sessions_when_ready(ctx);
            assert!(view.ssh_profile.control_path_poll.is_some());
            assert!(
                !view
                    .ssh_profile
                    .connection
                    .as_ref()
                    .unwrap()
                    .read_remote_sessions_when_ready
            );
        });
    });
}

#[test]
fn powershell_connections_use_its_invocation_and_escaping_rules() {
    let (command, _) = connection_command(
        "command ssh -i 'key with spaces' -- 'user@host'".into(),
        true,
        ShellFamily::PowerShell,
    )
    .unwrap();
    assert!(command.starts_with("ssh "));
    assert!(command.contains("key` with` spaces"));
    assert!(!command.starts_with("command "));
}

#[test]
fn reconnect_uses_only_the_watched_block_and_ssh_failure_status() {
    for (ssh_output, exit_code, expected_attempts) in [
        ("", 255, 0),
        ("client_loop: send disconnect: Broken pipe\n", 0, 0),
        ("client_loop: send disconnect: Broken pipe\n", 255, 1),
    ] {
        App::test((), move |mut app| async move {
            initialize_app_for_terminal_view(&mut app);
            let terminal = add_window_with_terminal(&mut app, None);
            terminal.update(&mut app, |view, ctx| {
                view.model
                    .lock()
                    .simulate_block("old", "client_loop: send disconnect: Broken pipe\n");
                watch_quick_connect(view);
                let local = SessionId::from(1);
                view.active_block_metadata = Some(BlockMetadata::new(Some(local), None));
                view.model
                    .lock()
                    .simulate_long_running_block("ssh root@remote", ssh_output);
                start_block_with_command(view, "ssh root@remote", ctx);
                view.model.lock().command_finished(
                    crate::terminal::model::ansi::CommandFinishedValue {
                        completion_metadata: crate::terminal::model::ansi::CompletionMetadata {
                            exit_code: exit_code.into(),
                            next_block_id: BlockId::new(),
                        },
                        session_id: None,
                    },
                );
                view.handle_ssh_manager_precmd(&BlockMetadata::new(Some(local), None), ctx);
                assert_eq!(
                    view.ssh_profile.connection.as_ref().unwrap().attempt,
                    expected_attempts
                );
            });
        });
    }
}

#[test]
fn cli_agent_native_image_paste_is_preserved_in_a_managed_ssh_session() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use warpui::TypedActionView as _;

    use crate::terminal::cli_agent::CLIAgent;
    use crate::terminal::cli_agent_sessions::{
        CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
        CLIAgentSessionsModel,
    };
    use crate::terminal::view::{Event, TerminalAction};

    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let socket = tempfile::NamedTempFile::new().unwrap();
        let writes = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let captured = writes.clone();
        app.update(|ctx| {
            ctx.subscribe_to_view(&terminal, move |_, event, _| {
                if let Event::WriteBytesToPty { bytes } = event {
                    captured.borrow_mut().push(bytes.to_vec());
                }
            });
        });
        terminal.update(&mut app, |view, ctx| {
            CLIAgentSessionsModel::handle(ctx).update(ctx, |sessions, ctx| {
                sessions.set_session(
                    view.view_id,
                    CLIAgentSession {
                        agent: CLIAgent::Claude,
                        status: CLIAgentSessionStatus::InProgress,
                        session_context: CLIAgentSessionContext::default(),
                        input_state: CLIAgentInputState::Closed,
                        should_auto_toggle_input: false,
                        listener: None,
                        remote_host: None,
                        plugin_version: None,
                        draft_text: None,
                        custom_command_prefix: None,
                        received_rich_notification: false,
                    },
                    ctx,
                );
            });
            view.model.lock().simulate_long_running_block("claude", "");
            watch_quick_connect(view);
            let connection = view.ssh_profile.connection.as_mut().unwrap();
            connection.local_session_id = Some(SessionId::from(1));
            connection.control_path = Some(socket.path().to_path_buf());
            assert!(view.ssh_upload_control_path(ctx).is_some());
            ctx.clipboard().write(ClipboardContent {
                images: Some(vec![warpui::clipboard::ImageData {
                    data: vec![0x89, 0x50, 0x4e, 0x47],
                    mime_type: "image/png".into(),
                    filename: None,
                }]),
                ..Default::default()
            });
            view.handle_action(&TerminalAction::Paste, ctx);
        });
        let expected = if cfg!(windows) {
            vec![0x1b, b'v']
        } else {
            vec![0x16]
        };
        assert_eq!(*writes.borrow(), vec![expected]);
    });
}
