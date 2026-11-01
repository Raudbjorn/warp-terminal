use std::path::PathBuf;

use anyhow::anyhow;
use project_replace::engine::build_matcher;
use project_replace::scope::{Scope, ScopeGlobs};
use project_replace::worker::search_roots;
use string_offset::ByteOffset;

/// A single submatch span within a matched line.
#[derive(Clone, Debug)]
pub struct Submatch {
    /// Byte offset into `line_text` where this submatch starts.
    pub byte_start: ByteOffset,
    /// Byte offset into `line_text` where this submatch ends (exclusive).
    pub byte_end: ByteOffset,
}

/// A single search match: one line in one file, with submatch highlights.
#[derive(Clone, Debug)]
pub struct Match {
    pub file_path: PathBuf,
    pub line_number: u32,
    pub line_text: String,
    pub submatches: Vec<Submatch>,
}

/// Entry point for the ripgrep subprocess.
///
/// Runs a ripgrep search in-process and writes JSON results to stdout.
/// The main Warp process spawns this via the `ripgrep-search` CLI
/// subcommand and reads the JSON output.
pub fn run_search_subprocess(
    patterns: &[String],
    paths: Vec<PathBuf>,
    ignore_case: bool,
    multiline: bool,
    scope: &ScopeGlobs,
    #[cfg_attr(not(unix), allow(unused_variables))] parent_pid: Option<u32>,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    crate::monitor_parent_and_exit_on_change(parent_pid);

    if patterns.is_empty() {
        return Err(anyhow!("No patterns specified"));
    }
    if paths.is_empty() {
        return Err(anyhow!("No paths specified"));
    }

    let matcher = build_matcher(patterns, ignore_case, multiline)?;
    let scope = Scope::parse(scope)?;
    let stdout = std::sync::Mutex::new(std::io::stdout());
    search_roots(&matcher, multiline, &paths, &scope, &stdout);

    Ok(())
}

#[cfg(not(target_family = "wasm"))]
mod process_impl {
    use std::path::PathBuf;
    use std::process::Stdio;

    use futures::StreamExt as _;
    use futures::io::{AsyncBufReadExt as _, BufReader};
    use futures::stream::Stream;
    use project_replace::scope::ScopeGlobs;

    use super::{Match, Submatch};
    use crate::types::RipgrepMessage;

    /// Searches `paths` for lines matching `patterns` and returns all results.
    ///
    /// Spawns a child process that runs the ripgrep search, collects every
    /// match, and returns them once the search is complete.
    ///
    /// For incremental results, use [`search_streaming`] instead.
    pub async fn search(
        patterns: &[String],
        paths: &[PathBuf],
        ignore_case: bool,
        multiline: bool,
    ) -> anyhow::Result<Vec<Match>> {
        let scope = ScopeGlobs::default();
        let stream = search_streaming(patterns, paths, ignore_case, multiline, &scope)?;
        Ok(stream.collect().await)
    }

    /// Searches `paths` for lines matching `patterns`, returning a stream
    /// of matches as they are found.
    ///
    /// This is the preferred entry point when responsiveness matters (e.g.
    /// the global search UI). The caller controls batching and throttling.
    pub fn search_streaming(
        patterns: &[String],
        paths: &[PathBuf],
        ignore_case: bool,
        multiline: bool,
        scope: &ScopeGlobs,
    ) -> anyhow::Result<impl Stream<Item = Match>> {
        let child = spawn_search_process(patterns, paths, ignore_case, multiline, scope)?;
        Ok(match_stream_from_child(child))
    }

    /// Spawns the warp CLI with the `ripgrep-search` subcommand.
    fn spawn_search_process(
        patterns: &[String],
        paths: &[PathBuf],
        ignore_case: bool,
        multiline: bool,
        scope: &ScopeGlobs,
    ) -> Result<async_process::Child, std::io::Error> {
        let current_exe = std::env::current_exe()?;
        let mut cmd = command::r#async::Command::new(current_exe);

        cmd.arg(warp_cli::ripgrep_search_subcommand())
            .arg(warp_cli::parent_flag());

        if ignore_case {
            cmd.arg("--ignore-case");
        }

        if multiline {
            cmd.arg("--multiline");
        }

        // Joined with `=` so that a pattern starting with a dash is not read as an option.
        if !scope.include.trim().is_empty() {
            cmd.arg(format!("--include={}", scope.include));
        }
        if !scope.exclude.trim().is_empty() {
            cmd.arg(format!("--exclude={}", scope.exclude));
        }

        for pattern in patterns {
            cmd.arg(pattern);
        }

        for path in paths {
            cmd.arg(path);
        }

        cmd.kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());

        cmd.spawn()
    }

    /// Turns a child process (with piped stdout) into a stream of parsed matches.
    fn match_stream_from_child(mut child: async_process::Child) -> impl Stream<Item = Match> {
        let stdout = child
            .stdout
            .take()
            .expect("spawn_search_process must pipe stdout");
        let reader = BufReader::new(stdout);

        futures::stream::unfold((reader, child), |(mut reader, child)| async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => return None,
                    Ok(_) => {}
                }
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<RipgrepMessage>(&line) {
                    Ok(RipgrepMessage::Match { data }) => {
                        let submatches = data
                            .submatches
                            .into_iter()
                            .map(|s| Submatch {
                                byte_start: s.start,
                                byte_end: s.end,
                            })
                            .collect();
                        let m = Match {
                            file_path: PathBuf::from(data.path.text),
                            line_number: data.line_number,
                            line_text: data.lines.text,
                            submatches,
                        };
                        return Some((m, (reader, child)));
                    }
                    Ok(RipgrepMessage::Begin | RipgrepMessage::End) => continue,
                    Err(err) => {
                        log::warn!("ripgrep: failed to parse JSON line: {err}");
                        continue;
                    }
                }
            }
        })
    }
}

#[cfg(not(target_family = "wasm"))]
pub use process_impl::{search, search_streaming};
