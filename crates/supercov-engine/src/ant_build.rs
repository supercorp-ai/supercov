//! What an Ant build says about where its sources are.
//!
//! Ant has no layout convention: a build names its source directories in
//! `<javac>` and the tests it runs in `<batchtest>`, through properties and
//! files it imports, often from a shared harness outside the project. This
//! reads as much of that as can be known without running Ant: the import
//! chain, the properties it defines (first definition wins, as in Ant), and
//! the directories its test batches scan. A path built at run time by a
//! custom task stays unknown, and the caller falls back to a convention.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// One start, end or empty-element tag, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    /// Where the tag starts and where it ends, `<` to `>` inclusive.
    pub start: usize,
    pub end: usize,
    pub closing: bool,
    pub empty: bool,
}

impl Tag {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Every tag of an XML text, skipping comments, processing instructions,
/// declarations and CDATA. Enough for build files, which are data, not a
/// document: no attempt is made to validate.
pub fn tags(text: &str) -> Vec<Tag> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find('<') {
        let start = at + offset;
        let rest = &text[start..];
        let skip_to = |terminator: &str| {
            rest.find(terminator)
                .map_or(text.len(), |end| start + end + terminator.len())
        };
        if rest.starts_with("<!--") {
            at = skip_to("-->");
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            at = skip_to("]]>");
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            at = skip_to(">");
            continue;
        }
        // The end of the tag: the first `>` outside a quoted value.
        let mut quote = None;
        let mut end = None;
        for (index, &byte) in bytes[start + 1..].iter().enumerate() {
            match (quote, byte) {
                (Some(open), _) if byte == open => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(byte),
                (None, b'>') => {
                    end = Some(start + 1 + index);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let inner = &text[start + 1..end];
        let closing = inner.starts_with('/');
        let empty = inner.ends_with('/');
        let inner = inner.trim_start_matches('/').trim_end_matches('/');
        let name_end = inner
            .find(|character: char| character.is_whitespace())
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_owned();
        if !name.is_empty() {
            found.push(Tag {
                attributes: attributes(&inner[name_end..]),
                name,
                start,
                end,
                closing,
                empty,
            });
        }
        at = end + 1;
    }
    found
}

fn attributes(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        let Some(equals) = rest.find('=') else { break };
        let key = rest[..equals].trim().to_owned();
        let after = rest[equals + 1..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(close) = after[1..].find(quote) else { break };
        found.push((key, unescape(&after[1..1 + close])));
        rest = &after[close + 2..];
    }
    found
}

fn unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// What could be read of an Ant build without running it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AntBuild {
    /// The project's `build.xml` and every file it imports, in the order Ant
    /// reads them.
    pub files: Vec<PathBuf>,
    /// The directories `<batchtest>` scans for test classes.
    pub test_roots: Vec<PathBuf>,
    /// Directories the build writes into: `<javac destdir>` and the usual
    /// output properties. Never sources.
    pub outputs: Vec<PathBuf>,
    /// The files that hold a `<junit>` task.
    pub junit_files: Vec<PathBuf>,
}

/// Properties Ant builds conventionally write their outputs under.
const OUTPUT_PROPERTIES: &[&str] = &[
    "build.dir",
    "build",
    "classes",
    "classes.dir",
    "dest.dir",
    "dist.dir",
    "dist",
    "out.dir",
    "target.dir",
    "tmp.dir",
];

/// Reads the build that `build.xml` at `root` starts. Nothing when there is
/// no such file. `defines` are properties given on the command line, which
/// Ant sets before any file defines its own.
pub fn read_ant_build(root: &Path, defines: &BTreeMap<String, String>) -> AntBuild {
    let mut build = AntBuild::default();
    let start = root.join("build.xml");
    if !start.is_file() {
        return build;
    }
    let mut properties = defines.clone();
    properties
        .entry("basedir".into())
        .or_insert_with(|| root.to_string_lossy().into_owned());
    read_file(root, &start, &mut properties, &mut build);
    for name in OUTPUT_PROPERTIES {
        if let Some(value) = properties.get(*name).filter(|value| !value.contains("${")) {
            push_unique(&mut build.outputs, resolve(root, value));
        }
    }
    build
}

fn read_file(
    root: &Path,
    file: &Path,
    properties: &mut BTreeMap<String, String>,
    build: &mut AntBuild,
) {
    let file = normalized(file);
    if build.files.contains(&file) || build.files.len() > 64 {
        return;
    }
    let Ok(text) = crate::fault_fs::read_to_string(&file) else {
        return;
    };
    build.files.push(file.clone());
    let directory = file.parent().unwrap_or(root).to_path_buf();
    let all = tags(&text);
    let mut in_batchtest = 0usize;
    for tag in &all {
        if tag.closing {
            if tag.name == "batchtest" {
                in_batchtest = in_batchtest.saturating_sub(1);
            }
            continue;
        }
        match tag.name.as_str() {
            "property" => define(root, tag, properties),
            "import" | "include" => {
                if let Some(imported) = tag.attribute("file") {
                    let imported = expand(imported, properties);
                    if !imported.contains("${") {
                        read_file(root, &directory.join(imported), properties, build);
                    }
                }
            }
            "batchtest" if !tag.empty => in_batchtest += 1,
            "fileset" | "dirset" if in_batchtest > 0 => {
                if let Some(dir) = tag.attribute("dir") {
                    let dir = expand(dir, properties);
                    if !dir.contains("${") {
                        push_unique(&mut build.test_roots, resolve(root, &dir));
                    }
                }
            }
            "javac" => {
                if let Some(dir) = tag.attribute("destdir") {
                    let dir = expand(dir, properties);
                    if !dir.contains("${") {
                        push_unique(&mut build.outputs, resolve(root, &dir));
                    }
                }
            }
            "junit" => push_unique(&mut build.junit_files, file.clone()),
            _ => {}
        }
    }
}

/// `<property>` sets a property once: a later definition, in this file or
/// another, leaves the first alone.
fn define(root: &Path, tag: &Tag, properties: &mut BTreeMap<String, String>) {
    if let Some(name) = tag.attribute("name") {
        if properties.contains_key(name) {
            return;
        }
        if let Some(location) = tag.attribute("location") {
            let location = expand(location, properties);
            let value = if location.contains("${") {
                location
            } else {
                resolve(root, &location).to_string_lossy().into_owned()
            };
            properties.insert(name.to_owned(), value);
        } else if let Some(value) = tag.attribute("value") {
            let value = expand(value, properties);
            properties.insert(name.to_owned(), value);
        }
        return;
    }
    if let Some(file) = tag.attribute("file") {
        let file = expand(file, properties);
        if let Ok(text) = crate::fault_fs::read_to_string(resolve(root, &file)) {
            for (key, value) in properties_file(&text) {
                let value = expand(&value, properties);
                properties.entry(key).or_insert(value);
            }
        }
    }
}

/// `key=value` and `key: value` lines, as java.util.Properties reads the
/// plain ones. Continuation lines and escapes beyond the common ones are not
/// needed for the paths a build names.
fn properties_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim_start)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .filter_map(|line| {
            let split = line.find(['=', ':'])?;
            Some((
                line[..split].trim().to_owned(),
                line[split + 1..].trim().to_owned(),
            ))
        })
        .collect()
}

/// `${name}` replaced by each known property, unknown ones left as written.
pub fn expand(value: &str, properties: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(open) = rest.find("${") {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &rest[open + 2..open + close];
        match properties.get(name) {
            Some(known) => out.push_str(known),
            None => out.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// A path as Ant resolves `location` and `dir`: against the project's base
/// directory, whichever file names it.
fn resolve(root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    normalized(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    })
}

/// `..` and `.` folded away without touching the filesystem.
pub fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// The closest directory that holds the project and every file its build
/// imports. None when the build stays inside the project.
pub fn outside_anchor(root: &Path, build: &AntBuild) -> Option<PathBuf> {
    let root = normalized(root);
    let mut anchor = root.clone();
    for file in &build.files {
        while !file.starts_with(&anchor) {
            anchor = anchor.parent()?.to_path_buf();
        }
    }
    (anchor != root).then_some(anchor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temporary(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "supercov-ant-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn tags_skip_comments_and_keep_quoted_angle_brackets() {
        let found = tags(r#"<?xml version="1.0"?><!-- <junit> --><project a='x>y'><junit fork="true"/></project>"#);
        let names: Vec<_> = found.iter().map(|tag| (tag.name.as_str(), tag.closing, tag.empty)).collect();
        assert_eq!(names, [("project", false, false), ("junit", false, true), ("project", true, false)]);
        assert_eq!(found[0].attribute("a"), Some("x>y"));
    }

    /// The Ametys layout: a thin build.xml importing a shared harness three
    /// levels up, which defines the test directory in a target and scans it
    /// in a `<batchtest>` nested in `<jacoco:coverage>`.
    #[test]
    fn a_harness_imported_from_outside_names_the_test_roots() {
        let top = temporary("harness");
        let project = top.join("plugins/forms/master");
        let common = top.join("tools/master/build/common");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&common).unwrap();
        fs::write(
            project.join("build.xml"),
            r#"<project name="Forms">
    <property name="common-build" location="../../../tools/master/build/common/common-build.xml"/>
    <import file="${common-build}"/>
</project>"#,
        )
        .unwrap();
        fs::write(project.join("build.properties"), "# comment\nerrorOnUnusedKeys=false\n").unwrap();
        fs::write(
            common.join("common-build.xml"),
            r#"<project name="common">
    <property file="build.properties"/>
    <property name="tmp.dir" location="tmp"/>
    <target name="prepare">
        <property name="tests-dir" location="test"/>
        <property name="tmp.dir" location="elsewhere"/>
    </target>
    <target name="test">
        <javac destdir="${tmp.dir}/classes-test"><src path="${tests-dir}/src"/></javac>
        <jacoco:coverage>
            <junit fork="true">
                <batchtest todir="x"><fileset dir="${tests-dir}/src"><include name="**/*TestCase.java"/></fileset></batchtest>
            </junit>
        </jacoco:coverage>
    </target>
</project>"#,
        )
        .unwrap();
        let build = read_ant_build(&project, &BTreeMap::new());
        assert_eq!(build.files, [project.join("build.xml"), common.join("common-build.xml")]);
        assert_eq!(build.test_roots, [project.join("test/src")]);
        assert_eq!(build.junit_files, [common.join("common-build.xml")]);
        assert!(build.outputs.contains(&project.join("tmp")), "{:?}", build.outputs);
        assert!(build.outputs.contains(&project.join("tmp/classes-test")), "the first tmp.dir wins: {:?}", build.outputs);
        assert_eq!(outside_anchor(&project, &build), Some(top.clone()));
        fs::remove_dir_all(top).unwrap();
    }

    #[test]
    fn a_build_that_stays_inside_has_no_anchor_and_command_defines_win() {
        let project = temporary("inside");
        fs::write(
            project.join("build.xml"),
            r#"<project><property name="test.src" value="unit"/><target name="t"><junit><batchtest><fileset dir="${test.src}"/></batchtest></junit></target></project>"#,
        )
        .unwrap();
        let defines = BTreeMap::from([("test.src".to_owned(), "it".to_owned())]);
        let build = read_ant_build(&project, &defines);
        assert_eq!(build.test_roots, [project.join("it")]);
        assert_eq!(outside_anchor(&project, &build), None);
        assert_eq!(read_ant_build(&project.join("missing"), &BTreeMap::new()), AntBuild::default());
        fs::remove_dir_all(project).unwrap();
    }
}
