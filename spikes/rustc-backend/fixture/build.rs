use std::{env, fs, path::PathBuf};

fn main() {
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    fs::write(
        output.join("generated.rs"),
        "pub fn generated_by_build_script(value: bool) -> usize { if value { 7 } else { 9 } }\n",
    )
    .expect("write generated source");
}
