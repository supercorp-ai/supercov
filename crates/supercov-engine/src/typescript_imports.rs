//! Conservative import-elision policy for the supported tsc/tsx/ts-node path.
//! Unknown loaders, config resolution and emit overrides retain runtime obligations.
use crate::fault_fs as fs;
use crate::source_discovery::{strip_jsonc_comments, strip_trailing_commas};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&strip_trailing_commas(&strip_jsonc_comments(&text))).ok()
}

fn options(root: &Path, path: &Path, seen: &mut BTreeSet<PathBuf>) -> Option<Map<String, Value>> {
    let path = fs::canonicalize(path).ok()?;
    if !path.starts_with(root) {
        return None;
    }
    if !seen.insert(path.clone()) || seen.len() > 32 {
        return None;
    }
    let config = json(&path)?;
    let mut result = Map::new();
    if let Some(extends) = config.get("extends") {
        let parents = match extends {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().map(Value::as_str).collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        for parent in parents {
            // Package-based and external config chains are deliberately not
            // guessed. They can keep imports alive through inherited settings.
            if !parent.starts_with('.') {
                return None;
            }
            let mut parent = path.parent()?.join(parent);
            if parent.extension().is_none() {
                parent.set_extension("json");
            }
            result.extend(options(root, &parent, seen)?);
        }
    }
    if let Some(local) = config.get("compilerOptions") {
        result.extend(local.as_object()?.clone());
    }
    seen.remove(&path);
    Some(result)
}

fn expanded_command(root: &Path, command: &[String]) -> String {
    let mut result = command.join(" ");
    // Expand only scripts actually named by the command. Other build tools in
    // package.json are not evidence about the compiler used by this run.
    if matches!(
        command.first().map(String::as_str),
        Some("npm" | "pnpm" | "yarn")
    ) && let Some(scripts) =
        json(&root.join("package.json")).and_then(|v| v.get("scripts").cloned())
    {
        for arg in command.iter().skip(1) {
            if let Some(script) = scripts.get(arg).and_then(Value::as_str) {
                result.push('\n');
                result.push_str(script);
            }
        }
    }
    result
}

/// Include relative base configs in the normal run fingerprint, even when a
/// base is named `base.json` rather than `tsconfig.*.json`.
pub(crate) fn config_paths(root: &Path, files: &[String]) -> BTreeSet<PathBuf> {
    fn add(root: &Path, path: PathBuf, paths: &mut BTreeSet<PathBuf>) {
        let Ok(path) = fs::canonicalize(path) else {
            return;
        };
        if !path.starts_with(root) || paths.len() > 256 || !paths.insert(path.clone()) {
            return;
        }
        if let Some(config) = json(&path) {
            let parents = match config.get("extends") {
                Some(Value::String(s)) => vec![s.as_str()],
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            for parent in parents.into_iter().filter(|p| p.starts_with('.')) {
                let mut parent = path.parent().unwrap().join(parent);
                if parent.extension().is_none() {
                    parent.set_extension("json");
                }
                add(root, parent, paths);
            }
        }
    }
    let given = root;
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let mut paths = BTreeSet::new();
    for file in files {
        let path = root.join(file);
        for parent in path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&root))
        {
            // Named root build configs can be selected with `tsc -p`. Include
            // their local bases too, even when those bases have arbitrary names.
            if let Ok(entries) = fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("tsconfig") && name.ends_with(".json") {
                        add(&root, entry.path(), &mut paths);
                    }
                }
            }
        }
    }
    // Followed through links on the canonical root, handed back under the
    // root as given: callers name files relative to it, and the canonical
    // form differs -- `\\?\C:\...` on Windows, `/private/var` for `/var` on
    // macOS -- so a fingerprint called its tsconfig outside the project.
    paths
        .into_iter()
        .filter_map(|path| path.strip_prefix(&root).ok().map(|local| given.join(local)))
        .collect()
}

pub(crate) fn elides_type_imports(root: &Path, file: &str, command: &[String]) -> bool {
    elides_type_imports_in(root, file, command, |name| std::env::var_os(name))
}

/// `elides_type_imports` with the environment passed in, so a test can name
/// a loader override without changing the process's own.
fn elides_type_imports_in(
    root: &Path,
    file: &str,
    command: &[String],
    environment: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> bool {
    let root_path = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let root = root_path.as_path();
    let text = expanded_command(root, command);
    let tokens = text
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .collect::<Vec<_>>();
    // A compiler name in an arbitrary argument is not evidence of execution.
    // Recognize direct invocations and Node's explicit TS loader options.
    let recognized = text.split(['\n', ';', '&', '|']).any(|part| {
        let words = part.split_whitespace().collect::<Vec<_>>();
        let executable = words
            .first()
            .and_then(|s| Path::new(s).file_name())
            .and_then(|s| s.to_str());
        matches!(executable, Some("tsc" | "tsx" | "ts-node"))
            || (executable == Some("node")
                && words.iter().enumerate().any(|(i, word)| {
                    matches!(
                        *word,
                        "--import=tsx"
                            | "--loader=ts-node/esm"
                            | "--experimental-loader=ts-node/esm"
                    ) || (matches!(*word, "--import" | "--loader" | "--experimental-loader")
                        && words
                            .get(i + 1)
                            .is_some_and(|s| matches!(*s, "tsx" | "ts-node/esm")))
                }))
    });
    if !recognized {
        return false;
    }
    let mut explicit_configs = Vec::new();
    // A recognized command has its executable's word at least.
    let words = text.split_whitespace().collect::<Vec<_>>();
    let readable = words.iter().enumerate().all(|(index, word)| {
        let value = if matches!(*word, "-p" | "--project" | "--tsconfig") {
            let Some(value) = words.get(index + 1) else {
                return false;
            };
            Some(*value)
        } else {
            word.strip_prefix("--project=")
                .or_else(|| word.strip_prefix("--tsconfig="))
                .or_else(|| word.strip_prefix("-p="))
        };
        if let Some(value) = value {
            let path = root.join(value);
            let Ok(path) = fs::canonicalize(path) else {
                return false;
            };
            // Root-local named tsconfigs are included in the input fingerprint.
            // Other explicit config layouts remain conservative for now.
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.parent() != Some(root)
                || !name.starts_with("tsconfig")
                || !name.ends_with(".json")
            {
                return false;
            }
            explicit_configs.push(path);
            true
        } else {
            // `-pconfig.json`, a form not read. (No word starting `-p`
            // starts `--`, so `--project` never reaches here.)
            !word.starts_with("-p")
        }
    });
    if !readable {
        return false;
    }
    // CLI/environment overrides may select a different config or import policy.
    if [
        "--verbatimModuleSyntax",
        "--preserveValueImports",
        "--importsNotUsedAsValues",
        "--emitDecoratorMetadata",
        "--compilerOptions",
        "--compiler-options",
    ]
    .iter()
    .any(|s| text.contains(s))
        || [
            "TS_NODE_COMPILER_OPTIONS",
            "TS_NODE_PROJECT",
            "TSX_TSCONFIG_PATH",
        ]
        .iter()
        .any(|s| environment(s).is_some())
    {
        return false;
    }
    // A recognized transpiler is not sufficient if the same command also
    // invokes a custom bundler/loader with independently controlled emit.
    if tokens.iter().any(|s| {
        matches!(
            *s,
            "swc" | "babel" | "webpack" | "vite" | "vitest" | "esbuild"
        )
    }) {
        return false;
    }
    let permits_elision = |path: &Path| {
        let Some(opts) = options(root, path, &mut BTreeSet::new()) else {
            return false;
        };
        [
            "verbatimModuleSyntax",
            "preserveValueImports",
            "emitDecoratorMetadata",
        ]
        .iter()
        .all(|key| opts.get(*key).is_none_or(|v| v == &Value::Bool(false)))
            && opts
                .get("importsNotUsedAsValues")
                .is_none_or(|v| v == "remove")
    };
    if !explicit_configs.iter().all(|path| permits_elision(path)) {
        return false;
    }
    let target = root.join(file);
    for dir in target
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(root))
    {
        let config = dir.join("tsconfig.json");
        if config.exists() {
            return permits_elision(&config);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_chain_preservation_overrides_and_unknowns_fail_closed() {
        let root = std::env::temp_dir().join(format!("supercov-imports-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        let command = vec!["node".into(), "--import".into(), "tsx".into()];
        fs::write(
            root.join("base.json"),
            r#"{"compilerOptions":{"verbatimModuleSyntax":true}}"#,
        )
        .unwrap();
        for (config, expected) in [
            (
                r#"{/* default */ "compilerOptions": {"strict":true,},}"#,
                true,
            ),
            (r#"{"extends":"./base.json"}"#, false),
            (
                r#"{"extends":"./base.json","compilerOptions":{"verbatimModuleSyntax":false}}"#,
                true,
            ),
            (r#"{"extends":"@project/config"}"#, false),
            (
                r#"{"compilerOptions":{"emitDecoratorMetadata":true}}"#,
                false,
            ),
            (r#"{"extends":"./tsconfig.json"}"#, false),
        ] {
            fs::write(root.join("tsconfig.json"), config).unwrap();
            assert_eq!(
                elides_type_imports(&root, "src/main.ts", &command),
                expected,
                "{config}"
            );
        }
        fs::write(root.join("tsconfig.json"), "{}").unwrap();
        assert!(!elides_type_imports(
            &root,
            "src/main.ts",
            &["node".into(), "--experimental-strip-types".into()]
        ));
        assert!(!elides_type_imports(
            &root,
            "src/main.ts",
            &["tsx".into(), "--tsconfig".into(), "other.json".into()]
        ));
        fs::write(
            root.join("tsconfig.build.json"),
            r#"{"extends":"./tsconfig.json"}"#,
        )
        .unwrap();
        let build = vec!["tsc".into(), "-p".into(), "tsconfig.build.json".into()];
        assert!(elides_type_imports(&root, "src/main.ts", &build));
        fs::write(
            root.join("tsconfig.build.json"),
            r#"{"extends":"./base.json"}"#,
        )
        .unwrap();
        assert!(!elides_type_imports(&root, "src/main.ts", &build));
        assert!(config_paths(&root, &["src/main.ts".into()]).contains(&root.join("base.json")));
        assert!(!elides_type_imports(
            &root,
            "src/main.ts",
            &["node".into(), "script.js".into(), "tsx".into()]
        ));
        fs::remove_dir_all(root).unwrap();
    }

    /// A config chain that leaves the project, runs past 32 configs or
    /// names something that is not JSON keeps imports; the fingerprint
    /// follows a chain no further than 257 configs, nor outside the project.
    #[test]
    fn config_chains_that_leave_the_project_or_run_long_are_not_followed() {
        let base = std::env::temp_dir().join(format!(
            "supercov-imports-chains-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        let write = |name: &str, text: &str| fs::write(root.join(name), text).unwrap();
        let tsx = vec!["tsx".to_owned(), "src/main.ts".to_owned()];
        let elides = || elides_type_imports(&root, "src/main.ts", &tsx);
        let configs = || config_paths(&root, &["src/main.ts".to_owned()]);
        // No tsconfig up to the project's root.
        assert!(!elides());
        fs::write(base.join("shared.json"), r#"{"compilerOptions":{}}"#).unwrap();
        write("tsconfig.json", r#"{"extends":"../shared.json"}"#);
        assert!(!elides());
        assert_eq!(configs(), BTreeSet::from([root.join("tsconfig.json")]));
        let chain = |length: usize| {
            for n in 0..length {
                write(
                    &format!("c{n}.json"),
                    &format!(r#"{{"extends":"./c{}"}}"#, n + 1),
                );
            }
            write(&format!("c{length}.json"), "{}");
            write("tsconfig.json", r#"{"extends":"./c0"}"#);
        };
        chain(10);
        assert!(elides());
        assert!(configs().contains(&root.join("c10.json")));
        chain(40);
        assert!(!elides());
        chain(300);
        assert_eq!(configs().len(), 257);
        for n in 0..=300 {
            fs::remove_file(root.join(format!("c{n}.json"))).unwrap();
        }
        write("tsconfig.json", "not json");
        write("tsconfig.txt", "{}");
        assert!(!elides());
        // Named by a file whose directory is gone, too.
        assert_eq!(
            config_paths(&root, &["gone/main.ts".to_owned()]),
            BTreeSet::from([root.join("tsconfig.json")])
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn each_loader_form_config_name_and_override_is_read() {
        let root = std::env::temp_dir().join(format!(
            "supercov-imports-forms-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src/nested")).unwrap();
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let elides = |file: &str, command: &str| elides_type_imports(&root, file, &words(command));
        let write = |name: &str, text: &str| fs::write(root.join(name), text).unwrap();
        write("tsconfig.json", "{}");
        write("base.json", r#"{"compilerOptions":{"strict":true}}"#);
        write(
            "preserve.json",
            r#"{"compilerOptions":{"importsNotUsedAsValues":"preserve"}}"#,
        );
        // Each way Node is told to load a TypeScript loader.
        assert!(elides("src/main.ts", "node --import=tsx src/main.ts"));
        assert!(elides(
            "src/main.ts",
            "node --loader ts-node/esm src/main.ts"
        ));
        assert!(elides(
            "src/main.ts",
            "node --experimental-loader=ts-node/esm src/main.ts"
        ));
        assert!(!elides("src/main.ts", "node --loader other src/main.ts"));
        // An extends chain without an extension, as a list, or leaving the project.
        write("tsconfig.json", r#"{"extends":"./base"}"#);
        assert!(elides("src/main.ts", "tsx src/main.ts"));
        write("tsconfig.json", r#"{"extends":["./base.json","./base"]}"#);
        assert!(elides("src/main.ts", "tsx src/main.ts"));
        write("tsconfig.json", r#"{"extends":"../outside.json"}"#);
        assert!(!elides("src/main.ts", "tsx src/main.ts"));
        write("tsconfig.json", r#"{"extends":7}"#);
        assert!(!elides("src/main.ts", "tsx src/main.ts"));
        write("tsconfig.json", "{}");
        // Explicit configs: named in every form, and refused where not a root tsconfig.
        write(
            "tsconfig.build.json",
            r#"{"compilerOptions":{"importsNotUsedAsValues":"remove"}}"#,
        );
        assert!(elides("src/main.ts", "tsc --project=tsconfig.build.json"));
        assert!(elides(
            "src/main.ts",
            "tsx --tsconfig=tsconfig.build.json src/main.ts"
        ));
        assert!(!elides("src/main.ts", "tsc -p"));
        assert!(!elides("src/main.ts", "tsc -ptsconfig.build.json"));
        assert!(!elides("src/main.ts", "tsc -p preserve.json"));
        write(
            "tsconfig.preserve.json",
            r#"{"compilerOptions":{"importsNotUsedAsValues":"preserve"}}"#,
        );
        assert!(!elides("src/main.ts", "tsc -p tsconfig.preserve.json"));
        write("tsconfig.txt", "{}");
        assert!(!elides("src/main.ts", "tsc -p tsconfig.txt"));
        assert!(!elides("src/main.ts", "tsc -p missing.json"));
        // An override on the command line, or a bundler beside the compiler.
        assert!(!elides("src/main.ts", "tsc --verbatimModuleSyntax"));
        assert!(!elides("src/main.ts", "tsc && vite build"));
        // Or an override in the environment a loader reads.
        let overridden = |name: &str| (name == "TS_NODE_PROJECT").then(|| "other.json".into());
        assert!(!elides_type_imports_in(
            &root,
            "src/main.ts",
            &words("tsx src/main.ts"),
            overridden
        ));
        // The nearest tsconfig decides for a file below it.
        write(
            "src/nested/tsconfig.json",
            r#"{"compilerOptions":{"verbatimModuleSyntax":true}}"#,
        );
        assert!(!elides("src/nested/deep.ts", "tsx src/nested/deep.ts"));
        assert!(elides("src/main.ts", "tsx src/main.ts"));
        fs::remove_file(root.join("src/nested/tsconfig.json")).unwrap();
        // An empty extends list adds nothing; a missing base is no path.
        write("tsconfig.json", r#"{"extends":[]}"#);
        assert!(elides("src/main.ts", "tsx src/main.ts"));
        write("tsconfig.json", r#"{"extends":"./missing"}"#);
        assert!(!elides("src/main.ts", "tsx src/main.ts"));
        assert!(
            !config_paths(&root, &["src/main.ts".to_owned()])
                .iter()
                .any(|path| path.ends_with("missing.json"))
        );
        write("tsconfig.json", "{}");
        // A package script the command names, and a package manager alone.
        write("package.json", r#"{"scripts":{"start":"tsx src/main.ts"}}"#);
        assert!(elides("src/main.ts", "npm start"));
        assert!(!elides("src/main.ts", "npm"));
        // A file outside the project has no tsconfig of it.
        assert!(!elides("/elsewhere/main.ts", "tsx /elsewhere/main.ts"));
        assert!(config_paths(&root, &["/elsewhere/main.ts".to_owned()]).is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
