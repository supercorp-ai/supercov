//! A server a browser suite starts with `docker compose`, built from the copy
//! the suite runs in. The image holds the instrumented code, but nothing else
//! crossed into the container: not the environment that tells the server's
//! runtime where to write, and not the directory it writes to. A `docker`
//! launcher on the suite's PATH hands each service built from the copy that
//! environment and Supercov's directory, mounted at the path it has outside,
//! and records the project so the run can stop it, and read what it wrote,
//! once the command ends.
//!
//! Anything it does not recognise, or cannot read, runs as written.

use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::Value;

/// The hidden command a launcher runs: `supercov __docker <docker> <args>`.
pub const DOCKER_COMMAND: &str = "__docker";
/// Where projects the launcher started are recorded, one JSON line each.
pub const STATE_VARIABLE: &str = "SUPERCOV_DOCKER_STATE";
/// The directory mounted into each service, at the same path.
pub const MOUNT_VARIABLE: &str = "SUPERCOV_DOCKER_MOUNT";
/// The copy: a service whose build context is in it was built from it.
pub const COPY_VARIABLE: &str = "SUPERCOV_DOCKER_COPY";
/// Names, comma-separated, of the variables each such service is given.
pub const ENVIRONMENT_VARIABLE: &str = "SUPERCOV_DOCKER_ENVIRONMENT";
/// Set in each such service: its runtime runs in an image of the copy.
pub const CONTAINER_VARIABLE: &str = "SUPERCOV_CONTAINER";

/// Compose options that take a value, before its subcommand.
const VALUED_OPTIONS: &[&str] = &[
    "-f",
    "--file",
    "-p",
    "--project-name",
    "--profile",
    "--project-directory",
    "--env-file",
    "--ansi",
    "--progress",
    "--parallel",
];

/// Subcommands that create containers, the only moment a service's
/// environment and mounts can be given.
const CREATING: &[&str] = &["up", "run", "create"];

/// What a `docker compose` command line holds: the options before
/// `compose`, the compose options, the subcommand and what follows it.
#[derive(Debug, PartialEq)]
struct ComposeCommand {
    docker_options: Vec<OsString>,
    compose_options: Vec<OsString>,
    subcommand: OsString,
    rest: Vec<OsString>,
}

/// Split a command line given to `docker` (or, when `compose_binary`, to
/// `docker-compose`). None for anything but compose.
fn parse(arguments: &[OsString], compose_binary: bool) -> Option<ComposeCommand> {
    let mut index = 0;
    let mut docker_options = Vec::new();
    if !compose_binary {
        // Docker's own options before the command: `--context x`, `-H x`.
        while index < arguments.len() {
            let argument = arguments[index].to_str()?;
            if argument == "compose" {
                index += 1;
                break;
            }
            if !argument.starts_with('-') {
                return None;
            }
            docker_options.push(arguments[index].clone());
            if matches!(
                argument,
                "--context" | "-c" | "-H" | "--host" | "--config" | "-l" | "--log-level"
            ) {
                index += 1;
                docker_options.push(arguments.get(index)?.clone());
            }
            index += 1;
        }
        if index == 0 || arguments.get(index - 1).and_then(|word| word.to_str()) != Some("compose")
        {
            return None;
        }
    }
    let mut compose_options = Vec::new();
    while index < arguments.len() {
        let argument = arguments[index].to_str()?;
        if !argument.starts_with('-') {
            return Some(ComposeCommand {
                docker_options,
                compose_options,
                subcommand: arguments[index].clone(),
                rest: arguments[index + 1..].to_vec(),
            });
        }
        compose_options.push(arguments[index].clone());
        if VALUED_OPTIONS.contains(&argument) {
            index += 1;
            compose_options.push(arguments.get(index)?.clone());
        }
        index += 1;
    }
    None
}

/// The project's configuration as compose resolves it, with each service
/// built from `copy` given `environment` and `mount`, and the names of those
/// services. None when no service is, or the configuration is not what
/// compose prints.
fn instrument(
    mut config: Value,
    copy: &Path,
    mount: &Path,
    environment: &[(String, String)],
) -> Option<(String, Value, Vec<String>)> {
    let name = config.get("name")?.as_str()?.to_owned();
    let services = config.get_mut("services")?.as_object_mut()?;
    let mut changed = Vec::new();
    for (service_name, service) in services.iter_mut() {
        let Some(context) = service
            .get("build")
            .and_then(|build| build.get("context"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !Path::new(context).starts_with(copy) {
            continue;
        }
        let service = service.as_object_mut()?;
        let variables = service
            .entry("environment")
            .or_insert_with(|| Value::Object(Default::default()));
        let variables = variables.as_object_mut()?;
        for (key, value) in environment {
            variables.insert(key.clone(), Value::String(value.clone()));
        }
        // Tells a runtime it runs in an image of the copy, where the copy's
        // files are at a path of the image's own (`/app`).
        variables.insert(CONTAINER_VARIABLE.into(), Value::String("1".into()));
        // Python writes the bytecode of Supercov's runtime beside it, in the
        // mounted directory, as the container's root: the run could not
        // remove it. A service's own setting stays.
        variables
            .entry("PYTHONPYCACHEPREFIX")
            .or_insert_with(|| Value::String("/tmp/supercov-pycache".into()));
        let volumes = service
            .entry("volumes")
            .or_insert_with(|| Value::Array(Vec::new()));
        volumes.as_array_mut()?.push(serde_json::json!({
            "type": "bind",
            "source": mount.display().to_string(),
            "target": mount.display().to_string(),
        }));
        changed.push(service_name.clone());
    }
    (!changed.is_empty()).then_some((name, config, changed))
}

/// What a variable a runtime needs is in a container whose image sets it
/// too: the runtime's part of ours first, then the image's. Ours was made
/// for the run's own processes and carries the host's value after it.
fn joined_with_image(name: &str, ours: &str, image: Option<&str>) -> String {
    let (runtime, separator) = match name {
        // `<runtime>:<host's PYTHONPATH>`.
        "PYTHONPATH" => (ours.split(':').next().unwrap_or(ours).to_owned(), ":"),
        // `-r<runtime> <host's RUBYOPT>`.
        _ => (
            ours.split_whitespace()
                .filter(|option| option.contains("supercov"))
                .collect::<Vec<_>>()
                .join(" "),
            " ",
        ),
    };
    match image.filter(|value| !value.is_empty()) {
        Some(image) => format!("{runtime}{separator}{image}"),
        None => runtime,
    }
}

/// A variable the image of `service` sets, once built.
fn image_variable(
    docker: &Path,
    docker_options: &[OsString],
    image: &str,
    name: &str,
) -> Option<String> {
    let output = Command::new(docker)
        .args(docker_options)
        .args([
            "image",
            "inspect",
            "--format",
            "{{json .Config.Env}}",
            image,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    serde_json::from_slice::<Vec<String>>(&output.stdout)
        .ok()?
        .into_iter()
        .find_map(|entry| entry.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

/// Run `docker` as the launcher was asked to, giving services built from the
/// copy what their runtime needs. Returns the exit code.
pub fn run(docker: &Path, arguments: &[OsString]) -> i32 {
    let compose_binary = docker
        .file_stem()
        .is_some_and(|stem| stem == OsStr::new("docker-compose"));
    let rewritten = parse(arguments, compose_binary)
        .filter(|command| {
            command
                .subcommand
                .to_str()
                .is_some_and(|subcommand| CREATING.contains(&subcommand))
        })
        .and_then(|command| rewrite(docker, compose_binary, &command));
    run_docker(docker, rewritten.as_deref().unwrap_or(arguments))
}

/// The command line that runs `command` on the instrumented configuration,
/// recorded for the run to stop. None to run it as written.
fn rewrite(docker: &Path, compose_binary: bool, command: &ComposeCommand) -> Option<Vec<OsString>> {
    let state = PathBuf::from(std::env::var_os(STATE_VARIABLE)?);
    let mount = PathBuf::from(std::env::var_os(MOUNT_VARIABLE)?);
    let copy = PathBuf::from(std::env::var_os(COPY_VARIABLE)?);
    let environment = std::env::var(ENVIRONMENT_VARIABLE)
        .ok()?
        .split(',')
        .filter(|name| !name.is_empty())
        .filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?)))
        .collect::<Vec<_>>();
    let prefix = |options: &[OsString]| {
        let mut line = Vec::new();
        if !compose_binary {
            line.extend(command.docker_options.iter().cloned());
            line.push(OsString::from("compose"));
        }
        line.extend(options.iter().cloned());
        line
    };
    let mut config_line = prefix(&command.compose_options);
    config_line.extend(["config", "--format", "json"].map(OsString::from));
    let output = Command::new(docker)
        .args(&config_line)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let config = serde_json::from_slice::<Value>(&output.stdout).ok()?;
    let (name, mut config, services) = instrument(config, &copy, &mount, &environment)?;
    fs::create_dir_all(&state).ok()?;
    let file = state.join(format!("{}.compose.json", sanitized(&name)));
    fs::write(&file, serde_json::to_vec_pretty(&config).ok()?).ok()?;
    // The resolved file holds every service of every file the command named,
    // so it replaces them; the project keeps its name and its directory.
    let mut options = vec![
        OsString::from("-f"),
        file.clone().into_os_string(),
        OsString::from("-p"),
        OsString::from(&name),
    ];
    let mut kept = command.compose_options.iter();
    while let Some(option) = kept.next() {
        match option.to_str() {
            Some("-f" | "--file" | "-p" | "--project-name") => {
                kept.next();
            }
            Some("--project-directory" | "--env-file" | "--profile") => {
                options.push(option.clone());
                options.extend(kept.next().cloned());
            }
            _ => options.push(option.clone()),
        }
    }
    // Built from this copy before anything starts: `up` alone reuses an
    // image that exists, which may hold the project as it was at another
    // run, or no probes at all.
    let mut build = prefix(&options);
    build.push(OsString::from("build"));
    build.extend(services.iter().map(OsString::from));
    let _ = Command::new(docker)
        .args(&build)
        .stdin(Stdio::null())
        .status();
    // A Python or Ruby runtime goes before what the image itself sets.
    let mut rewritten = false;
    for service in &services {
        let image = config["services"][service]["image"]
            .as_str()
            .map_or_else(|| format!("{name}-{service}"), str::to_owned);
        for variable in ["PYTHONPATH", "RUBYOPT"] {
            let Some(ours) = config["services"][service]["environment"][variable].as_str() else {
                continue;
            };
            let value = joined_with_image(
                variable,
                ours,
                image_variable(docker, &command.docker_options, &image, variable).as_deref(),
            );
            config["services"][service]["environment"][variable] = Value::String(value);
            rewritten = true;
        }
    }
    if rewritten {
        fs::write(&file, serde_json::to_vec_pretty(&config).ok()?).ok()?;
    }
    let mut record = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("projects.jsonl"))
        .ok()?;
    let line = serde_json::json!({
        "docker": docker.display().to_string(),
        "composeBinary": compose_binary,
        "dockerOptions": command.docker_options.iter().map(|option| option.to_string_lossy()).collect::<Vec<_>>(),
        "file": file.display().to_string(),
        "project": name,
    });
    writeln!(record, "{line}").ok()?;
    let mut line = prefix(&options);
    line.push(command.subcommand.clone());
    line.extend(command.rest.iter().cloned());
    Some(line)
}

fn sanitized(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(unix)]
fn run_docker(docker: &Path, arguments: &[OsString]) -> i32 {
    use std::os::unix::process::CommandExt;
    // The launcher becomes docker: a signal the suite sends its server
    // reaches compose as it would without Supercov.
    let error = Command::new(docker).args(arguments).exec();
    eprintln!("[supercov] could not run {}: {error}", docker.display());
    127
}

#[cfg(not(unix))]
fn run_docker(docker: &Path, arguments: &[OsString]) -> i32 {
    Command::new(docker)
        .args(arguments)
        .status()
        .map_or(127, |status| status.code().unwrap_or(1))
}

/// Stop what the launcher started, so each server writes its last window
/// before the run reads them. Stopped, never removed: the containers are
/// the suite's. Quietly does nothing when nothing was started.
pub fn stop_started(state: &Path) {
    let Ok(text) = fs::read_to_string(state.join("projects.jsonl")) else {
        return;
    };
    let mut stopped = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Ok(project) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(docker), Some(file), Some(name)) = (
            project["docker"].as_str(),
            project["file"].as_str(),
            project["project"].as_str(),
        ) else {
            continue;
        };
        if !stopped.insert(name.to_owned()) {
            continue;
        }
        let mut line = Vec::<String>::new();
        if project["composeBinary"].as_bool() != Some(true) {
            line.extend(
                project["dockerOptions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option.as_str().map(str::to_owned)),
            );
            line.push("compose".into());
        }
        line.extend(["-f", file, "-p", name, "stop", "-t", "10"].map(str::to_owned));
        let _ = Command::new(docker)
            .args(&line)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The launchers, `docker` and `docker-compose` for those on PATH, in
/// `directory`, or None when neither is.
pub fn write_launchers(directory: &Path, path: &OsStr) -> Option<()> {
    let binary = crate::source_tools::launching_binary()?;
    let mut found = false;
    for name in ["docker", "docker-compose"] {
        let Some(real) = std::env::split_paths(path)
            .map(|entry| entry.join(name))
            .find(|candidate| candidate.is_file())
        else {
            continue;
        };
        // A launcher of ours found on PATH is not the real one.
        if real.starts_with(directory) {
            continue;
        }
        fs::create_dir_all(directory).ok()?;
        let quoted = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        crate::source_tools::write_launcher(
            &directory.join(name),
            &format!(
                "#!/bin/sh\n# Gives a server compose builds from Supercov's copy what its coverage needs; see `supercov --help`.\nexec {} {DOCKER_COMMAND} {} \"$@\"\n",
                quoted(&binary),
                quoted(&real),
            ),
        )
        .ok()?;
        found = true;
    }
    found.then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<OsString> {
        line.split_whitespace().map(OsString::from).collect()
    }

    #[test]
    fn a_compose_command_is_split_where_compose_reads_it() {
        let command = parse(
            &words("--context ci compose -f a.yml -p demo --profile e2e up -d web"),
            false,
        )
        .unwrap();
        assert_eq!(command.docker_options, words("--context ci"));
        assert_eq!(
            command.compose_options,
            words("-f a.yml -p demo --profile e2e")
        );
        assert_eq!(command.subcommand, "up");
        assert_eq!(command.rest, words("-d web"));
        let binary = parse(&words("up --build"), true).unwrap();
        assert!(binary.docker_options.is_empty() && binary.compose_options.is_empty());
        assert_eq!(binary.subcommand, "up");
        assert!(parse(&words("run --rm alpine true"), false).is_none());
        assert!(parse(&words("build ."), false).is_none());
    }

    #[test]
    fn only_services_built_from_the_copy_are_given_the_runtime() {
        let config = serde_json::json!({
            "name": "shop",
            "services": {
                "api": { "build": { "context": "/work/.supercov/workspaces/workspace/shop/api" }, "environment": { "PORT": "8080" } },
                "db": { "image": "postgres:17" },
                "other": { "build": { "context": "/elsewhere" } },
            }
        });
        let (name, config, services) = instrument(
            config,
            Path::new("/work/.supercov/workspaces/workspace/shop"),
            Path::new("/work/.supercov"),
            &[(
                "SUPERCOV_GO_SERVER_EVIDENCE_DIR".into(),
                "/work/.supercov/work/run/go/evidence".into(),
            )],
        )
        .unwrap();
        assert_eq!(name, "shop");
        assert_eq!(services, ["api"]);
        assert_eq!(
            config["services"]["api"]["environment"]["SUPERCOV_CONTAINER"],
            "1"
        );
        let api = &config["services"]["api"];
        assert_eq!(api["environment"]["PORT"], "8080");
        assert_eq!(
            api["environment"]["SUPERCOV_GO_SERVER_EVIDENCE_DIR"],
            "/work/.supercov/work/run/go/evidence"
        );
        assert_eq!(api["volumes"][0]["source"], "/work/.supercov");
        assert_eq!(api["volumes"][0]["target"], "/work/.supercov");
        assert!(config["services"]["db"].get("volumes").is_none());
        assert!(config["services"]["other"].get("environment").is_none());
        // A runtime goes before what the image sets.
        assert_eq!(
            joined_with_image("PYTHONPATH", "/r/runtime:/host/lib", Some("/app")),
            "/r/runtime:/app"
        );
        assert_eq!(
            joined_with_image("PYTHONPATH", "/r/runtime", None),
            "/r/runtime"
        );
        assert_eq!(
            joined_with_image(
                "RUBYOPT",
                "-r/r/supercov_runtime.rb -W0",
                Some("-rbundler/setup")
            ),
            "-r/r/supercov_runtime.rb -rbundler/setup"
        );
        // No service built from the copy: the command runs as written.
        assert!(
            instrument(
                serde_json::json!({ "name": "x", "services": { "db": { "image": "postgres" } } }),
                Path::new("/copy"),
                Path::new("/m"),
                &[]
            )
            .is_none()
        );
    }
}
