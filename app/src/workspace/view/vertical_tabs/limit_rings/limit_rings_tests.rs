use super::*;

const NOW: f64 = 1_800_000_000.;

fn cache(claude: &str) -> String {
    format!(
        r#"{{"claude": {claude},
           "codex": {{"ok": true, "used5h": 90, "reset5h": {soon}, "usedWeek": 57.0, "resetWeek": {soon}, "_ts": {fresh}}},
           "other": {{"ok": true, "used5h": 10, "reset5h": {soon}, "usedWeek": 20, "resetWeek": {soon}, "_ts": {old}}}}}"#,
        soon = NOW + 3600.,
        fresh = NOW - 60.,
        old = NOW - 16. * 60.,
    )
}

#[test]
fn card_counts_between_known_numbers_and_snaps_to_a_dash() {
    let json = cache(&format!(
        r#"{{"ok": true, "used5h": 30, "reset5h": {soon}, "usedWeek": 6, "resetWeek": {past}, "_ts": {fresh}}}"#,
        past = NOW - 1.,
        soon = NOW + 3600.,
        fresh = NOW - 60.,
    ));
    let rings = rings(&json, NOW).unwrap();
    let (claude, codex) = (rings[0].card(), rings[1].card());
    let half = claude.mix(&codex, 0.5);
    // 5 hours: 30 % to Codex's hot 90 %.
    assert_eq!(half.windows[0].percent, Some(60.));
    assert!((half.windows[0].fill - 0.6).abs() < 1e-6);
    assert_eq!(half.windows[0].hot, 0.5);
    // Week: unknown to 57 %; the bar grows from empty, the dash gives way to the number.
    assert_eq!(claude.windows[1].percent, None);
    assert!((half.windows[1].fill - 0.285).abs() < 1e-6);
    assert_eq!(claude.mix(&codex, 0.4).windows[1].percent, None);
    assert_eq!(half.windows[1].percent, Some(57.));
    assert_eq!(claude.mix(&codex, 1.), codex);
}

#[test]
fn passed_reset_makes_only_that_window_unknown() {
    let json = cache(&format!(
        r#"{{"ok": true, "tier": "max", "used5h": 23.0, "reset5h": {past}, "usedWeek": 6, "resetWeek": {soon}, "_ts": {fresh}}}"#,
        past = NOW - 1.,
        soon = NOW + 3600.,
        fresh = NOW - 60.,
    ));
    let rings = rings(&json, NOW).unwrap();
    assert_eq!(rings[0].five_hours, None);
    assert_eq!(rings[0].week.map(|w| w.used), Some(6));
    assert!(!rings[0].stale);
    // Codex: 90 % in 5 hours is hot, not full.
    assert!(rings[1].hot() && !rings[1].full());
    // The third provider: read 16 minutes ago, so nothing is shown.
    assert!(rings[2].stale && rings[2].five_hours.is_none() && rings[2].week.is_none());
}

#[test]
fn providers_come_from_the_file_in_key_order() {
    let json = format!(
        r#"{{"zeta": {{"ok": true, "name": "Zeta CLI", "used5h": 5, "reset5h": {soon}, "_ts": {now}}},
            "alpha": {{"ok": true, "used5h": 7, "reset5h": {soon}, "_ts": {now}}},
            "_version": 2}}"#,
        soon = NOW + 3600.,
        now = NOW,
    );
    let parsed = rings(&json, NOW).unwrap();
    let names: Vec<&str> = parsed.iter().map(|ring| ring.name.as_str()).collect();
    // A non-object value is not a provider; without a "name" the key is shown.
    assert_eq!(names, ["alpha", "Zeta CLI"]);
    assert_eq!(parsed[1].five_hours.map(|w| w.used), Some(5));
    assert!(rings(r#"{}"#, NOW).unwrap().is_empty());
    assert!(rings("[]", NOW).is_none());
}

/// The shape one real writer (a menu bar app) produces, values made up: metadata next
/// to the providers must not become rings, and the labels come from "name".
#[test]
fn a_real_writer_shape_keeps_labels_and_order() {
    let json = format!(
        r#"{{"_written": {now}, "_meta": {{"version": 3}}, "settings": {{"theme": "dark"}},
            "muse": {{"id": "muse", "name": "Muse", "ok": true, "used5h": 3, "reset5h": {soon}, "usedWeek": 9, "resetWeek": {soon}, "tier": null, "_ts": {now}}},
            "codex": {{"id": "codex", "name": "Codex", "ok": true, "used5h": 1.5, "reset5h": {soon}, "usedWeek": 2.0, "resetWeek": {soon}, "tier": "plus", "ageSec": 30, "stale": false, "_ts": {now}}},
            "claude": {{"id": "claude", "name": "Claude", "ok": true, "tier": "max", "used5h": 12.0, "reset5h": {soon}, "usedWeek": 40.0, "resetWeek": {soon}, "scoped": [], "_ts": {now}}}}}"#,
        now = NOW,
        soon = NOW + 3600.,
    );
    let parsed = rings(&json, NOW).unwrap();
    let names: Vec<&str> = parsed.iter().map(|ring| ring.name.as_str()).collect();
    assert_eq!(names, ["Claude", "Codex", "Muse"]);
    assert!(parsed.iter().all(|ring| !ring.stale && ring.week.is_some()));
    assert_eq!(parsed[0].tier.as_deref(), Some("max"));
    assert_eq!(parsed[2].tier, None);
}

#[test]
fn not_ok_or_flagged_stale_shows_nothing() {
    for claude in [
        format!(
            r#"{{"ok": false, "used5h": 5, "reset5h": {}, "_ts": {}}}"#,
            NOW + 60.,
            NOW
        ),
        format!(
            r#"{{"ok": true, "stale": true, "used5h": 5, "reset5h": {}, "_ts": {}}}"#,
            NOW + 60.,
            NOW
        ),
    ] {
        let rings = rings(&cache(&claude), NOW).unwrap();
        assert!(rings[0].stale && rings[0].five_hours.is_none());
    }
}

#[test]
fn the_status_line_is_installed_only_in_harp_mode_with_rings_on() {
    assert!(should_install_status_line(true, true));
    assert!(!should_install_status_line(true, false));
    assert!(!should_install_status_line(false, true));
    assert!(!should_install_status_line(false, false));
}

#[test]
fn with_harp_mode_off_claude_settings_stay_untouched() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let settings = home.join(".claude").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original = "{\"model\": \"opus\"}\n";
    std::fs::write(&settings, original).unwrap();
    let state_dir = root.path().join("state");

    // Limit rings on is not enough: the mode is what allows writing outside the app's own dirs.
    for (harp_mode, rings_enabled) in [(false, true), (true, false), (false, false)] {
        assert!(check_status_line(harp_mode, rings_enabled, &home, &state_dir).is_none());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), original);
        assert!(!state_dir.exists());
    }

    let installed = check_status_line(true, true, &home, &state_dir).map(Result::unwrap);
    assert_eq!(installed, Some(limit_sources::StatusLineInstall::Installed));
    assert!(
        std::fs::read_to_string(&settings)
            .unwrap()
            .contains("statusLine")
    );
}
