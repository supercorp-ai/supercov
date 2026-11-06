//! Which imports that only types use TypeScript erases, read from the
//! project's tsconfig. A config that keeps them, or that cannot be read,
//! keeps them as runtime obligations.
use crate::fault_fs as fs;
use crate::source_discovery::{strip_jsonc_comments, strip_trailing_commas};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[track_caller]
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
            let mut parent = path
                .parent()
                .expect("a configuration file has a directory")
                .join(parent);
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
            let config = parent.join("tsconfig.json");
            if config.exists() {
                add(&root, config, &mut paths);
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

/// Whether `file`'s imports that only types use are erased: by its nearest
/// tsconfig alone. tsc, esbuild (tsx, Vite, Vitest), swc, Babel's TypeScript
/// preset and ts-node all erase them unless that config keeps them, so the
/// command that runs the file does not decide: on 2026-11-06 a unit run and
/// an end-to-end run of one commit counted `import { type Funnel }`
/// differently, and their runs could not be merged. Node's own type
/// stripping keeps an inline-type import; a project that runs on it sets
/// `verbatimModuleSyntax`, and one that does not counts one import less.
/// A config that cannot be read, or that reaches outside the project, keeps
/// them.
pub(crate) fn elides_type_imports(root: &Path, file: &str) -> bool {
    let root_path = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let root = root_path.as_path();
    let target = root.join(file);
    let Some(config) = target
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(root))
        .map(|dir| dir.join("tsconfig.json"))
        .find(|config| config.exists())
    else {
        return false;
    };
    let Some(opts) = options(root, &config, &mut BTreeSet::new()) else {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_chain_preservation_overrides_and_unknowns_fail_closed() {
        let root = std::env::temp_dir().join(format!("supercov-imports-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
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
            (
                r#"{"compilerOptions":{"importsNotUsedAsValues":"preserve"}}"#,
                false,
            ),
            (
                r#"{"compilerOptions":{"importsNotUsedAsValues":"remove"}}"#,
                true,
            ),
            (r#"{"extends":"./tsconfig.json"}"#, false),
            // An array of parents, one of them no name; options that are no
            // object.
            (r#"{"extends":["./base.json"]}"#, false),
            (r#"{"extends":["./base.json", 5]}"#, false),
            (r#"{"compilerOptions":1}"#, false),
        ] {
            fs::write(root.join("tsconfig.json"), config).unwrap();
            assert_eq!(
                elides_type_imports(&root, "src/main.ts"),
                expected,
                "{config}"
            );
        }
        fs::write(root.join("tsconfig.json"), "{}").unwrap();
        // A configuration that cannot be read keeps imports.
        assert!(!crate::fault::during("fs.read_to_string", || {
            elides_type_imports(&root, "src/main.ts")
        }));
        // The fingerprint follows a base named in either form.
        fs::write(root.join("tsconfig.json"), r#"{"extends":"./base.json"}"#).unwrap();
        assert!(config_paths(&root, &["src/main.ts".into()]).contains(&root.join("base.json")));
        fs::write(root.join("tsconfig.json"), r#"{"extends":["./base.json"]}"#).unwrap();
        assert!(config_paths(&root, &["src/main.ts".into()]).contains(&root.join("base.json")));
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
        let elides = || elides_type_imports(&root, "src/main.ts");
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

    /// The nearest tsconfig decides, and only it: a chain without an
    /// extension, as a list, or leaving the project; a nested config for
    /// the files below it; a file outside the project, which has none.
    #[test]
    fn the_nearest_tsconfig_decides() {
        let root = std::env::temp_dir().join(format!(
            "supercov-imports-nearest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src/nested")).unwrap();
        let elides = |file: &str| elides_type_imports(&root, file);
        let write = |name: &str, text: &str| fs::write(root.join(name), text).unwrap();
        write("base.json", r#"{"compilerOptions":{"strict":true}}"#);
        write("tsconfig.json", r#"{"extends":"./base"}"#);
        assert!(elides("src/main.ts"));
        write("tsconfig.json", r#"{"extends":["./base.json","./base"]}"#);
        assert!(elides("src/main.ts"));
        write("tsconfig.json", r#"{"extends":"../outside.json"}"#);
        assert!(!elides("src/main.ts"));
        write("tsconfig.json", r#"{"extends":7}"#);
        assert!(!elides("src/main.ts"));
        write("tsconfig.json", "{}");
        // A config another command might name is not read.
        write(
            "tsconfig.build.json",
            r#"{"compilerOptions":{"verbatimModuleSyntax":true}}"#,
        );
        assert!(elides("src/main.ts"));
        assert!(
            !config_paths(&root, &["src/main.ts".to_owned()])
                .contains(&root.join("tsconfig.build.json"))
        );
        write(
            "src/nested/tsconfig.json",
            r#"{"compilerOptions":{"verbatimModuleSyntax":true}}"#,
        );
        assert!(!elides("src/nested/deep.ts"));
        assert!(elides("src/main.ts"));
        fs::remove_file(root.join("src/nested/tsconfig.json")).unwrap();
        // An empty extends list adds nothing; a missing base is no path.
        write("tsconfig.json", r#"{"extends":[]}"#);
        assert!(elides("src/main.ts"));
        write("tsconfig.json", r#"{"extends":"./missing"}"#);
        assert!(!elides("src/main.ts"));
        assert!(
            !config_paths(&root, &["src/main.ts".to_owned()])
                .iter()
                .any(|path| path.ends_with("missing.json"))
        );
        write("tsconfig.json", "{}");
        assert!(!elides("/elsewhere/main.ts"));
        assert!(config_paths(&root, &["/elsewhere/main.ts".to_owned()]).is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
