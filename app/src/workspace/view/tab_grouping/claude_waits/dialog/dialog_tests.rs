use super::*;

pub(in crate::workspace::view) const PERMISSION: &str = "⏺ Write(/tmp/x)\n\n────────────────────────────────────────\n Create file\n /tmp/x\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n  1 hello\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n Do you want to create x?\n ❯ 1. Yes\n   2. Yes, allow all edits during this session (shift+tab)\n   3. No, and tell Claude what to do differently (esc)\n\n Esc to cancel · Tab to amend\n";

/// `PERMISSION` with the ❯ on option `at` (0-based).
pub(in crate::workspace::view) fn permission_at(at: usize) -> String {
    let labels = [
        "1. Yes",
        "2. Yes, allow all edits during this session (shift+tab)",
        "3. No, and tell Claude what to do differently (esc)",
    ];
    let head = PERMISSION.split(" ❯ 1. Yes").next().unwrap();
    let mut screen = head.to_owned();
    for (i, label) in labels.iter().enumerate() {
        screen.push_str(if i == at { " ❯ " } else { "   " });
        screen.push_str(label);
        screen.push('\n');
    }
    screen.push_str("\n Esc to cancel · Tab to amend\n");
    screen
}

pub(in crate::workspace::view) const OTHER: &str = "────────────────────────────────────────\n Bash command\n   rm -rf build\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n";

const ASK: &str = "☐ Database\n\nDo you want to use Postgres?\n\n ❯ 1. Yes\n   2. No\n   3. Type something.\n   4. Chat about this\n";

/// The input box below an answered dialog: no ❯ list.
const PLAIN: &str = "⏺ Done.\n\n────────────────────────────────────────\n❯ \n────────────────────────────────────────\n  ? for shortcuts\n";

/// A real Claude Code 2.1.304 Bash permission (warp-mobile bridge fixture): the
/// command between dashed rules, a "Tip:" and a description above it.
fn bash_prompt() -> String {
    let (rule, dashed) = ("─".repeat(120), "╌".repeat(120));
    [
        "❯ Run this exact bash command and nothing else: date > /tmp/wm-x/bash.txt",
        "",
        "  Writing current date to file",
        "  ⎿  $ date > /tmp/wm-x/bash.txt",
        "",
        &rule,
        " Bash command",
        " Tip: auto mode handles these prompts for you — choose \"switch to auto mode\" below",
        " Write current date to file",
        &dashed,
        " date > /tmp/wm-x/bash.txt",
        &dashed,
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. Yes, and always allow access to /tmp/wm-x from this project",
        "   3. Yes, and switch to auto mode · auto mode handles these prompts for you",
        "   4. No",
        "",
        " Esc to cancel · Tab to amend",
    ]
    .join("\n")
}

#[test]
fn a_permission_dialog_reads_as_the_phone_reads_it() {
    let dialog = dialog_on_screen(PERMISSION).unwrap();
    assert_eq!(dialog.title, "Create file");
    assert_eq!(
        dialog.body.last().map(String::as_str),
        Some("Do you want to create x?")
    );
    assert_eq!(
        dialog.options,
        [
            "Yes",
            "Yes, allow all edits during this session (shift+tab)",
            "No, and tell Claude what to do differently (esc)"
        ]
    );
    assert_eq!(dialog.selected, 0);
    assert_eq!(dialog_on_screen(&permission_at(2)).unwrap().selected, 2);
}

#[test]
fn a_moved_cursor_is_the_same_dialog_another_command_is_not() {
    let shown = dialog_on_screen(PERMISSION).unwrap();
    assert!(dialog_on_screen(&permission_at(1)).unwrap().same_as(&shown));
    let other = dialog_on_screen(&PERMISSION.replace("1 hello", "1 bye")).unwrap();
    assert!(!other.same_as(&shown));
}

#[test]
fn only_a_tool_permission_gets_allow_and_deny() {
    assert!(matches!(
        card_on_screen(PERMISSION),
        Some(Card::Approval(_))
    ));
    assert!(matches!(card_on_screen(OTHER), Some(Card::Approval(_))));
    assert!(matches!(
        card_on_screen(&bash_prompt()),
        Some(Card::Approval(_))
    ));
    // A question that happens to ask "Do you want to …?" is no permission.
    assert!(matches!(card_on_screen(ASK), Some(Card::Dialog(_))));
    assert_eq!(card_on_screen(PLAIN), None);
}

#[test]
fn a_permission_names_what_claude_wants_never_the_tip() {
    assert_eq!(
        approval_subject(&dialog_on_screen(PERMISSION).unwrap()),
        "/tmp/x"
    );
    assert_eq!(
        approval_subject(&dialog_on_screen(OTHER).unwrap()),
        "rm -rf build"
    );
    let bash = dialog_on_screen(&bash_prompt()).unwrap();
    assert_eq!(approval_subject(&bash), "Write current date to file");
}
