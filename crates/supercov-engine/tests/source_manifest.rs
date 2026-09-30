#[test]
fn frozen_inputs_accept_absolute_relative_and_windows_verbatim_paths_once() {
    let temporary =
        std::env::temp_dir().join(format!("supercov-source-paths-{}", std::process::id()));
    let directory = temporary.join("project");
    std::fs::create_dir_all(&directory).unwrap();
    let canonical = std::fs::canonicalize(&directory).unwrap();
    // Discovery supplies ordinary absolute paths, while canonicalize on Windows
    // returns a verbatim path. Both must identify the same captured source file.
    #[cfg(windows)]
    let root = std::path::PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    #[cfg(not(windows))]
    let root = canonical.clone();
    let source = "import assert from 'node:assert/strict'; assert.equal(1, 1);";
    std::fs::write(root.join("test.js"), source).unwrap();
    let inputs = supercov_engine::assertion_inputs::capture(
        &root,
        "javascript",
        [
            "test.js".into(),
            root.join("test.js"),
            canonical.join("test.js"),
        ],
    )
    .unwrap();
    assert_eq!(inputs.files.len(), 1);
    assert_eq!(inputs.files["test.js"], source);
    let outside = root.parent().unwrap().join("outside.js");
    std::fs::write(&outside, source).unwrap();
    assert!(
        supercov_engine::assertion_inputs::capture(&root, "javascript", [outside])
            .unwrap_err()
            .contains("outside project")
    );
    std::fs::remove_dir_all(temporary).unwrap();
}
