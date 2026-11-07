use std::{
    ffi::OsString,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use super::{
    ConfigStore, InstallSelection, LauncherApi, LauncherConfig, OwnedProcessState, ProcessScan,
    SessionStatus,
};

pub(super) fn run(arguments: Vec<OsString>) -> io::Result<()> {
    match Invocation::parse(arguments)? {
        Invocation::Help => {
            print_help();
            Ok(())
        }
        Invocation::Status(options) => run_status(options),
        Invocation::Session(options) => run_session(options),
    }
}

enum Invocation {
    Help,
    Status(Options),
    Session(Options),
}

#[derive(Default)]
struct Options {
    install_root: Option<PathBuf>,
    candidate: Option<PathBuf>,
    config: Option<PathBuf>,
}

impl Invocation {
    fn parse(arguments: Vec<OsString>) -> io::Result<Self> {
        let mut arguments = arguments.into_iter();
        let Some(command) = arguments.next() else {
            return Ok(Self::Help);
        };
        let command = command.to_string_lossy();
        if command == "--help" || command == "help" {
            return Ok(Self::Help);
        }
        if command == "start" || command == "stop" || command == "open-location" {
            return Err(invalid(
                "standalone start/stop is not supported; use `launcher session` so the child stays owned",
            ));
        }
        if command != "status" && command != "session" {
            return Err(invalid(
                "expected `launcher status` or `launcher session`; use --help",
            ));
        }
        let mut options = Options::default();
        while let Some(argument) = arguments.next() {
            let argument = argument.to_string_lossy();
            match argument.as_ref() {
                "--help" => return Ok(Self::Help),
                "--install-root" | "--candidate" | "--config" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| invalid("option requires a value"))?;
                    let slot = match argument.as_ref() {
                        "--install-root" => &mut options.install_root,
                        "--candidate" => &mut options.candidate,
                        "--config" => &mut options.config,
                        _ => unreachable!(),
                    };
                    if slot.replace(PathBuf::from(value)).is_some() {
                        return Err(invalid("duplicate option"));
                    }
                }
                _ => return Err(invalid("unknown option; use --help")),
            }
        }
        Ok(if command == "status" {
            Self::Status(options)
        } else {
            Self::Session(options)
        })
    }
}

fn resolve_config(options: &Options, persist: bool) -> io::Result<LauncherConfig> {
    let Some(config_path) = &options.config else {
        let root = options
            .install_root
            .clone()
            .ok_or_else(|| invalid("--install-root is required without --config"))?;
        let mut config = LauncherConfig::new(root);
        if let Some(candidate) = &options.candidate {
            config = config.with_candidate(candidate.clone());
        }
        return Ok(config);
    };
    let store = ConfigStore::new(config_path);
    let mut changed = false;
    let mut config = match store.load() {
        Ok(config) => config,
        Err(error) if error.is_missing_config() => {
            let root = options.install_root.clone().ok_or_else(|| {
                invalid("--install-root is required when the config file does not exist")
            })?;
            changed = true;
            LauncherConfig::new(root)
        }
        Err(error) => return Err(io::Error::other(error)),
    };
    if let Some(root) = &options.install_root
        && config.install_root != *root
    {
        config.install_root = root.clone();
        changed = true;
    }
    if let Some(candidate) = &options.candidate
        && config.candidate.as_ref() != Some(candidate)
    {
        config.candidate = Some(candidate.clone());
        changed = true;
    }
    if persist && changed {
        store.save(&config).map_err(io::Error::other)?;
    }
    Ok(config)
}

fn run_status(options: Options) -> io::Result<()> {
    let config = resolve_config(&options, false)?;
    let snapshot = LauncherApi::status(&config).map_err(io::Error::other)?;
    print_install(&snapshot.install);
    print_scan(&snapshot.processes);
    Ok(())
}

fn run_session(options: Options) -> io::Result<()> {
    let config = resolve_config(&options, true)?;
    let mut session = LauncherApi::session(config);
    match session.service().install() {
        Ok(install) => print_install(&install),
        Err(error) => eprintln!("Install unavailable: {error}"),
    }
    println!("Owned process: not running");
    println!("Commands: status, start, stop, open-location, help, quit");

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let command = line.trim();
        if command.is_empty() {
            continue;
        }
        match command {
            "quit" | "exit" => break,
            "help" => println!("Commands: status, start, stop, open-location, help, quit"),
            "status" => match session.status() {
                Ok(status) => print_session_status(&status),
                Err(error) => eprintln!("Status failed: {error}"),
            },
            "start" => match session.start() {
                Ok(state) => println!("Start: {}", describe_owned(&state)),
                Err(error) => eprintln!("Start failed: {error}"),
            },
            "stop" => match session.stop() {
                Ok(state) => println!("Stop: {}", describe_owned(&state)),
                Err(error) => eprintln!("Stop failed: {error}"),
            },
            "open-location" => match session.open_location() {
                Ok(()) => println!("Opened install location"),
                Err(error) => eprintln!("Open location failed: {error}"),
            },
            _ => eprintln!("Unknown command; use help"),
        }
        print!("launcher> ");
        io::stdout().flush()?;
    }
    session.shutdown().map_err(io::Error::other)?;
    println!("Launcher session ended");
    Ok(())
}

fn print_install(install: &InstallSelection) {
    println!("Install root: {}", install.root.display());
    println!("Executable: {}", install.executable.display());
}

fn print_scan(scan: &ProcessScan) {
    if scan.matching.is_empty() {
        println!("Observed install processes: none");
    } else {
        for identity in &scan.matching {
            println!(
                "Observed install process: pid {} {}",
                identity.pid,
                identity.executable.display()
            );
        }
    }
    for identity in &scan.same_name_foreign {
        println!(
            "Same-name foreign process (not owned): pid {} {}",
            identity.pid,
            identity.executable.display()
        );
    }
    if !scan.inaccessible.is_empty() {
        println!("Inaccessible processes: {:?}", scan.inaccessible);
    }
}

fn print_session_status(status: &SessionStatus) {
    print_install(&status.install);
    println!("Owned process: {}", describe_owned(&status.owned));
    print_scan(&status.processes);
}

fn describe_owned(state: &OwnedProcessState) -> String {
    match state {
        OwnedProcessState::Idle => "not running".into(),
        OwnedProcessState::Running { pid, executable } => {
            format!("running pid {pid} {}", executable.display())
        }
        OwnedProcessState::Finished {
            exit_code,
            requested_stop,
        } => format!(
            "finished exit_code={} requested_stop={requested_stop}",
            exit_code.map_or_else(|| "unknown".into(), |code| code.to_string())
        ),
        OwnedProcessState::StatusError { pid, message } => {
            format!("status error pid {pid}: {message}")
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn print_help() {
    println!(
        "neuromita launcher status --install-root DIR [--candidate FILE] [--config FILE]\n\
         neuromita launcher session --install-root DIR [--candidate FILE] [--config FILE]\n\n\
         `status` is read-only. `session` owns one child for the lifetime of the interactive\n\
         process and accepts: status, start, stop, open-location, help, quit.\n\
         No Gemini credentials are required."
    );
}
