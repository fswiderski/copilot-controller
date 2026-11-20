use std::{
    io::{self, Write},
    process::ExitCode,
};

use copilot_controller::{cli, native::windows_worker};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(result) = windows_worker::maybe_run_mx_creative_review_worker(&args) {
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => failure(error.to_string()),
        };
    }
    let allowlist = match if args.first().is_some_and(|argument| {
        argument == windows_worker::MX_CREATIVE_DISPLAY_INFO_HIDDEN_WORKER_ARGUMENT
    }) {
        cli::mx_creative_display_info_allowlist()
    } else if args
        .first()
        .is_some_and(|argument| argument == windows_worker::AKP03_HIDDEN_WORKER_ARGUMENT)
    {
        cli::akp03_probe_allowlist()
    } else if args
        .first()
        .is_some_and(|argument| argument == windows_worker::MX_CREATIVE_HIDDEN_WORKER_ARGUMENT)
    {
        cli::mx_creative_evidence_allowlist()
    } else {
        cli::diagnostic_allowlist()
    } {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    if let Some(result) = windows_worker::maybe_run_hidden_worker(&args, &allowlist) {
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => failure(error.to_string()),
        };
    }
    let args = match args
        .into_iter()
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(_) => return failure("arguments must be valid UTF-8".into()),
    };
    let command = match cli::parse(&args) {
        Ok(command) => command,
        Err(error) => return failure(error),
    };
    if matches!(command, cli::Command::ReviewInput) {
        return match cli::execute_review_input() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => failure(error),
        };
    }
    let output = cli::execute(command, io::stdin().lock()).and_then(cli::Output::render);
    match output {
        Ok(text) => match io::stdout().lock().write_all(text.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => failure("output write failed".into()),
        },
        Err(error) => failure(error),
    }
}

fn failure(error: String) -> ExitCode {
    let _ = writeln!(io::stderr().lock(), "{error}");
    ExitCode::FAILURE
}
