use super::*;

fn profile(user: Option<&str>, host: &str) -> SshHostProfile {
    let mut profile = SshHostProfile::new("box".to_string(), host.to_string());
    profile.user = user.map(str::to_string);
    profile
}

#[test]
fn password_prompts_must_name_the_profile_principal() {
    let expected = expected_principal(&profile(Some("Root"), "Box.Example.com"));
    assert_eq!(expected, "root@box.example.com");

    assert!(is_password_prompt_for(
        "root@box.example.com's password: ",
        &expected
    ));
    assert!(is_password_prompt_for(
        "(root@box.example.com) Password: ",
        &expected
    ));

    assert!(!is_password_prompt_for(
        "admin@box.example.com's password: ",
        &expected
    ));
    assert!(!is_password_prompt_for(
        "xroot@box.example.com's password: ",
        &expected
    ));
    assert!(!is_password_prompt_for(
        "root@jump.example.com's password: ",
        &expected
    ));
    assert!(!is_password_prompt_for(
        "(root@box.example.com) Verification code: ",
        &expected
    ));
    assert!(!is_password_prompt_for(
        "Enter passphrase for key '/home/me/.ssh/id_ed25519': ",
        &expected
    ));
    assert!(!is_password_prompt_for(
        "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
        &expected
    ));
}

#[test]
fn profiles_without_a_user_accept_any_user_on_the_host() {
    let expected = expected_principal(&profile(None, "10.0.0.5"));
    assert_eq!(expected, "@10.0.0.5");

    assert!(is_password_prompt_for(
        "me@10.0.0.5's password: ",
        &expected
    ));
    assert!(!is_password_prompt_for(
        "me@10.0.0.55's password: ",
        &expected
    ));
    assert!(!is_password_prompt_for("@10.0.0.5's password: ", &expected));
}

#[test]
fn the_env_prefix_is_stripped_back_to_the_ssh_command() {
    let prefix = env_prefix(&profile(Some("root"), "10.0.0.5")).unwrap();
    let command = format!("{prefix}ssh -p 2222 -- root@10.0.0.5");

    assert!(command.starts_with("SSH_ASKPASS="));
    assert_eq!(strip_env_prefix(&command), "ssh -p 2222 -- root@10.0.0.5");
    assert_eq!(strip_env_prefix("ssh root@10.0.0.5"), "ssh root@10.0.0.5");
    assert_eq!(
        strip_env_prefix("SSH_ASKPASS=/bin/x ssh host"),
        "SSH_ASKPASS=/bin/x ssh host"
    );
}

#[test]
fn saved_passwords_require_an_explicit_account() {
    assert!(env_prefix(&profile(None, "host")).is_none());
    assert!(env_prefix(&profile(Some(""), "host")).is_none());
}

fn command(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn caller_endpoint_requires_explicit_port_and_the_saved_alias() {
    let mut profile = profile(Some("user"), "host");
    profile.port = 2222;
    assert!(is_destination_command(
        &command(&[
            "/usr/bin/ssh",
            "-o",
            "ControlMaster=auto",
            "-p",
            "2222",
            "--",
            "user@box",
            "remote bootstrap"
        ]),
        &profile
    ));
    for args in [
        vec!["ssh", "-p", "22", "--", "user@box"],
        vec!["ssh", "--", "user@box"],
        vec!["ssh", "-p", "2222", "--", "user@other"],
        vec!["ssh", "-p", "2222", "-p", "22", "--", "user@box"],
        vec!["ssh", "-o", " Port=22", "-p", "2222", "--", "user@box"],
        vec!["ssh", "-o", "User=other", "-p", "2222", "--", "user@box"],
        vec!["ssh", "-l", "other", "-p", "2222", "--", "user@box"],
        vec!["sh", "-p", "2222", "--", "user@box"],
    ] {
        assert!(
            !is_destination_command(&command(&args), &profile),
            "{args:?}"
        );
    }
    profile.port_is_explicit = false;
    assert!(env_prefix(&profile).is_none());
    assert!(!is_destination_command(
        &command(&["ssh", "-p", "2222", "--", "user@box"]),
        &profile
    ));
}

#[test]
fn nested_ssh_callers_and_unavailable_or_cyclic_ancestry_cannot_release_credentials() {
    let profile = profile(Some("user"), "host");
    let ssh = command(&["ssh", "-p", "22", "--", "user@box"]);
    assert!(caller_is_destination(&profile, 10, |pid| match pid {
        10 => Some((11, ssh.clone())),
        11 => Some((1, command(&["bash"]))),
        _ => None,
    }));
    assert!(!caller_is_destination(&profile, 10, |pid| match pid {
        10 => Some((11, ssh.clone())),
        11 => Some((12, command(&["sh", "-c", "proxy"]))),
        12 => Some((1, ssh.clone())),
        _ => None,
    }));
    assert!(!caller_is_destination(&profile, 10, |pid| (pid == 10).then(|| (11, ssh.clone()))));
    let mut lookups = 0;
    assert!(!caller_is_destination(&profile, 10, |pid| {
        lookups += 1;
        Some((
            pid + 1,
            if pid == 10 {
                ssh.clone()
            } else {
                command(&["bash"])
            },
        ))
    }));
    assert_eq!(lookups, MAX_PROCESS_ANCESTORS + 1);
}

#[test]
fn reused_process_ids_do_not_reuse_another_connections_prompt_marker() {
    let dir = tempfile::tempdir().unwrap();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    assert!(claim_first_prompt(dir.path(), first, 123).unwrap());
    assert!(!claim_first_prompt(dir.path(), first, 123).unwrap());
    assert!(claim_first_prompt(dir.path(), second, 123).unwrap());
    assert!(!claim_first_prompt(dir.path(), second, 123).unwrap());
}

#[cfg(unix)]
#[test]
fn every_connection_gets_a_new_nonce_without_breaking_command_detection() {
    let profile = profile(Some("user"), "host");
    let first = env_prefix(&profile).unwrap();
    let second = env_prefix(&profile).unwrap();
    assert_ne!(first, second);
    assert!(first.contains(&format!("{CONNECTION_ENV}=")));
    assert_eq!(
        strip_env_prefix(&format!("{first}ssh -p 22 -- user@box")),
        "ssh -p 22 -- user@box"
    );
}

#[cfg(unix)]
#[test]
fn the_real_test_process_is_not_an_ssh_credential_caller() {
    assert!(!destination_process(
        &profile(Some("user"), "host"),
        std::process::id()
    ));
}
