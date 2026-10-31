use std::{env, fs, path::PathBuf, time::Instant};

use local_multi_agent_service::autocomplete::{
    AutocompleteBlockContext, LocalCommandAutocompleteRequest, LocalCommandAutocompleteResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_SERVICE_URL: &str = "http://127.0.0.1:8787";
const DEFAULT_CASES: &str = include_str!("../eval_cases/autocomplete.jsonl");

#[derive(Debug, Default)]
struct Args {
    service_url: String,
    prompt_file: Option<PathBuf>,
    case_filter: Option<String>,
    repeat: usize,
    json: bool,
    dump_request: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct EvalCase {
    name: String,
    prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recent_blocks: Vec<AutocompleteBlockContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    file_candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_file: Option<String>,
}

#[derive(Debug, Serialize)]
struct EvalResult {
    case: String,
    iteration: usize,
    command: String,
    source: String,
    parse_status: String,
    raw_output: String,
    duration_ms: u128,
    passed: bool,
    failures: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    local_multi_agent_service::install_tls_provider();
    let args = parse_args()?;
    let prompt_override = match &args.prompt_file {
        Some(path) => Some(fs::read_to_string(path)?),
        None => None,
    };

    let cases = load_cases(DEFAULT_CASES, args.case_filter.as_deref())?;
    if cases.is_empty() {
        anyhow::bail!("No autocomplete eval cases matched.");
    }

    let client = reqwest::Client::new();
    let endpoint = format!(
        "{}/ai/local-command-autocomplete",
        args.service_url.trim_end_matches('/')
    );

    let mut failed = 0usize;
    for iteration in 1..=args.repeat {
        for case in &cases {
            let request = case.to_request(prompt_override.clone());
            if args.dump_request {
                print_request(case, &request, args.json)?;
            }

            let started = Instant::now();
            let response = client
                .post(&endpoint)
                .json(&request)
                .send()
                .await?
                .error_for_status()?
                .json::<LocalCommandAutocompleteResponse>()
                .await?;
            let duration_ms = started.elapsed().as_millis();
            let failures = validate(case, &response);
            let result = EvalResult {
                case: case.name.clone(),
                iteration,
                command: response.most_likely_action.clone(),
                source: response.source.clone(),
                parse_status: response.parse_status.clone(),
                raw_output: response.raw_output.clone(),
                duration_ms,
                passed: failures.is_empty(),
                failures,
            };
            if !result.passed {
                failed += 1;
            }
            print_result(&result, args.json)?;
        }
    }

    if failed > 0 {
        anyhow::bail!("{failed} autocomplete eval case(s) failed.");
    }
    Ok(())
}

impl EvalCase {
    fn to_request(
        &self,
        system_prompt_override: Option<String>,
    ) -> LocalCommandAutocompleteRequest {
        LocalCommandAutocompleteRequest {
            prefix: self.prefix.clone(),
            cwd: self.cwd.clone(),
            shell: self.shell.clone(),
            platform: self.platform.clone(),
            recent_blocks: self.recent_blocks.clone(),
            history: self.history.clone(),
            file_candidates: self.file_candidates.clone(),
            system_prompt_override,
        }
    }
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        service_url: DEFAULT_SERVICE_URL.to_string(),
        repeat: 1,
        ..Default::default()
    };
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--service-url" => {
                args.service_url = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--service-url requires a value"))?;
            }
            "--prompt-file" => {
                args.prompt_file =
                    Some(PathBuf::from(iter.next().ok_or_else(|| {
                        anyhow::anyhow!("--prompt-file requires a value")
                    })?));
            }
            "--case" => {
                args.case_filter = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("--case requires a value"))?,
                );
            }
            "--repeat" => {
                args.repeat = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--repeat requires a value"))?
                    .parse::<usize>()?
                    .max(1);
            }
            "--json" => args.json = true,
            "--dump-request" => args.dump_request = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => anyhow::bail!("Unknown argument `{other}`. Use --help."),
        }
    }
    Ok(args)
}

fn print_help() {
    println!(
        "Usage: cargo run -p local_multi_agent_service --bin warp-local-autocomplete-eval -- [options]\n\
\n\
Options:\n\
  --service-url <url>   Running local service URL [default: http://127.0.0.1:8787]\n\
  --prompt-file <path>  Override the autocomplete system prompt\n\
  --case <name>         Run cases whose name contains this text\n\
  --repeat <n>          Repeat each selected case\n\
  --json                Print JSONL results\n\
  --dump-request        Print each request before sending\n"
    );
}

fn load_cases(raw: &str, case_filter: Option<&str>) -> anyhow::Result<Vec<EvalCase>> {
    raw.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            Some((index + 1, line))
        })
        .map(|(line_number, line)| {
            serde_json::from_str::<EvalCase>(line).map_err(|error| {
                anyhow::anyhow!("Invalid eval case on line {line_number}: {error}")
            })
        })
        .filter(|result| match (result, case_filter) {
            (Ok(case), Some(filter)) => case.name.contains(filter),
            _ => true,
        })
        .collect()
}

fn validate(case: &EvalCase, response: &LocalCommandAutocompleteResponse) -> Vec<String> {
    let command = response.most_likely_action.as_str();
    let mut failures = Vec::new();
    if command.is_empty() {
        failures.push("empty command".to_string());
    }
    if !command.starts_with(&case.prefix) {
        failures.push(format!(
            "command does not preserve prefix `{}`",
            case.prefix
        ));
    }
    if command.contains('\n') || command.contains('\r') {
        failures.push("command is not single-line".to_string());
    }
    if let Some(expected) = &case.expected_contains {
        if !command.contains(expected) {
            failures.push(format!("command does not contain `{expected}`"));
        }
    }
    if let Some(expected_file) = &case.expected_file {
        if !command.contains(expected_file) {
            failures.push(format!("command does not contain file `{expected_file}`"));
        }
    }
    if command.contains("```") || command.starts_with("You can ") || command.starts_with("I ") {
        failures.push("command looks like an explanation".to_string());
    }
    failures
}

fn print_request(
    case: &EvalCase,
    request: &LocalCommandAutocompleteRequest,
    as_json: bool,
) -> anyhow::Result<()> {
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "type": "request",
                "case": case.name,
                "request": request,
            }))?
        );
    } else {
        println!(
            "\n# {}\n{}",
            case.name,
            serde_json::to_string_pretty(request)?
        );
    }
    Ok(())
}

fn print_result(result: &EvalResult, as_json: bool) -> anyhow::Result<()> {
    if as_json {
        println!("{}", serde_json::to_string(result)?);
    } else {
        let status = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "[{status}] {} #{} ({} ms, {}, {}): {}",
            result.case,
            result.iteration,
            result.duration_ms,
            result.source,
            result.parse_status,
            result.command
        );
        for failure in &result.failures {
            println!("  - {failure}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(command: &str) -> LocalCommandAutocompleteResponse {
        LocalCommandAutocompleteResponse {
            most_likely_action: command.to_string(),
            raw_output: command.to_string(),
            commands: vec![command.to_string()],
            source: "model".to_string(),
            parse_status: "accepted".to_string(),
        }
    }

    #[test]
    fn validate_accepts_expected_token_and_file_candidate() {
        let case = EvalCase {
            prefix: "sed -n '1,120p' ".to_string(),
            expected_contains: Some("provider.rs".to_string()),
            expected_file: Some("crates/local_multi_agent_service/src/provider.rs".to_string()),
            ..Default::default()
        };

        assert!(
            validate(
                &case,
                &response("sed -n '1,120p' crates/local_multi_agent_service/src/provider.rs")
            )
            .is_empty()
        );
    }

    #[test]
    fn validate_rejects_non_prefix_multiline_and_explanation() {
        let case = EvalCase {
            prefix: "git ".to_string(),
            ..Default::default()
        };

        let failures = validate(&case, &response("I would run git status\n```"));
        assert!(failures.iter().any(|failure| failure.contains("prefix")));
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("single-line"))
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("explanation"))
        );
    }

    #[test]
    fn load_cases_filters_by_name() {
        let cases = load_cases(
            r#"
{"name":"first","prefix":"git "}
{"name":"second","prefix":"cargo "}
"#,
            Some("second"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].prefix, "cargo ");
    }
}
