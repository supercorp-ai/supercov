use std::{
    env, fs,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const CONTEXT_ENV: &str = "SUPERCOV_RUST_CONTEXT_ID";

fn child() -> Command {
    Command::new(env!("CARGO_BIN_EXE_supercov-subprocess-child"))
}

#[test]
fn inherited_subprocess_is_attributed() {
    let output = child()
        .arg("inherited")
        .output()
        .expect("run inherited-context child");
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "inherited");
}

#[test]
fn contextless_subprocess_is_background() {
    let output = child()
        .env_remove(CONTEXT_ENV)
        .arg("background")
        .output()
        .expect("run contextless child");
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "background");
}

#[test]
fn late_subprocess_is_contained() {
    let Some(marker) = env::var_os("SUPERCOV_LATE_PID_FILE") else {
        return;
    };
    let marker = std::path::PathBuf::from(marker);
    let mut command = child();
    command.arg("late");
    let spawned = command.spawn().expect("spawn late child");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.is_file() {
        assert!(Instant::now() < deadline, "late child never became ready");
        thread::sleep(Duration::from_millis(10));
    }
    let recorded_pid = fs::read_to_string(&marker).expect("read late-child marker");
    assert_eq!(recorded_pid.trim(), spawned.id().to_string());
    drop(spawned);
}

