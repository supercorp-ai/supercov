fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let build = supercov_engine::ant_build::read_ant_build(&root, &Default::default());
    println!("{build:#?}");
    println!(
        "anchor {:?}",
        supercov_engine::ant_build::outside_anchor(&root, &build)
    );
}
