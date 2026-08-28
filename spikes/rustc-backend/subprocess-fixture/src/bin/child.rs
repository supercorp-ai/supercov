use std::{env, fs, process, thread, time::Duration};

fn main() {
    match env::args().nth(1).as_deref() {
        Some("inherited") => println!(
            "{}",
            supercov_subprocess_fixture::inherited_child_probe(true)
        ),
        Some("background") => println!(
            "{}",
            supercov_subprocess_fixture::background_child_probe(true)
        ),
        Some("late") => {
            let marker = env::var_os("SUPERCOV_LATE_PID_FILE")
                .expect("late child requires SUPERCOV_LATE_PID_FILE");
            fs::write(marker, format!("{}\n", process::id())).expect("write late-child marker");
            thread::sleep(Duration::from_secs(30));
            println!("{}", supercov_subprocess_fixture::late_child_probe(true));
        }
        other => panic!("unexpected child mode: {other:?}"),
    }
}

