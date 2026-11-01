//! Prints the story Conn reads from a Claude Code transcript.
//!
//! Useful for checking the parser against a real session, and for seeing what
//! changed when Claude Code alters the transcript format:
//!
//! ```text
//! cargo run -p conn --example story -- ~/.claude/projects/<project>/<session>.jsonl
//! ```

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: story <transcript.jsonl>");
        std::process::exit(2);
    };
    let jsonl = std::fs::read_to_string(&path).expect("transcript is readable");

    let story = conn::story::parse_transcript(&jsonl);

    println!(
        "{}  ({} bytes, {} turns)\n",
        story.title.as_deref().unwrap_or("(untitled session)"),
        jsonl.len(),
        story.turns.len()
    );

    for turn in story
        .turns
        .iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        println!("── {} ──────────────", turn.started_at.format("%H:%M"));
        println!(" you   {}", first_line(&turn.prompt, 96));
        for step in turn.steps.iter().take(6) {
            let runs = if step.runs > 1 {
                format!(" ×{}", step.runs)
            } else {
                String::new()
            };
            println!("  did  {}{runs}", first_line(&step.description, 96));
        }
        if turn.steps.len() > 6 {
            println!("  did  … and {} more", turn.steps.len() - 6);
        }
        match &turn.outcome {
            Some(outcome) => println!(" said  {}\n", first_line(outcome, 96)),
            None => println!(" said  (still working)\n"),
        }
    }
}

fn first_line(text: &str, width: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= width {
        return line.to_owned();
    }
    line.chars().take(width).collect::<String>() + "…"
}
