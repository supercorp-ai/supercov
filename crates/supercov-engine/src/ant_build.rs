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
    pub attributes: Vec<Attribute>,
    /// Where the tag starts and where it ends, `<` to `>` inclusive.
    pub start: usize,
    pub end: usize,
    pub closing: bool,
    pub empty: bool,
}

/// An attribute of a tag, and where it is in the text the tag was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub value: String,
    /// The offset of its name.
    pub name_at: usize,
    /// The offset of the quote that closes its value.
    pub value_end: usize,
}

impl Tag {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attribute_at(name)
            .map(|attribute| attribute.value.as_str())
    }

    pub fn attribute_at(&self, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
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
        let unslashed = inner.trim_start_matches('/');
        let base = start + 1 + inner.len() - unslashed.len();
        let inner = unslashed.trim_end_matches('/');
        let name_end = inner
            .find(|character: char| character.is_whitespace())
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_owned();
        if !name.is_empty() {
            found.push(Tag {
                attributes: attributes(&inner[name_end..], base + name_end),
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

/// The attributes of a tag's text after its name, which starts at `base`.
fn attributes(text: &str, base: usize) -> Vec<Attribute> {
    let mut found = Vec::new();
    let mut at = 0;
    loop {
        at = text.len() - text[at..].trim_start().len();
        let rest = &text[at..];
        let Some(equals) = rest.find('=') else { break };
        let name = rest[..equals].trim().to_owned();
        let after = rest[equals + 1..].trim_start();
        let open = text.len() - after.len();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(close) = after[1..].find(quote) else {
            break;
        };
        let value_end = open + 1 + close;
        found.push(Attribute {
            name,
            value: unescape(&after[1..1 + close]),
            name_at: base + at,
            value_end: base + value_end,
        });
        at = value_end + 1;
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
    /// The directories `<batchtest>` (and `<junitlauncher>`'s
    /// `<testclasses>`) scans for test classes.
    pub test_roots: Vec<PathBuf>,
    /// Directories the build writes into: `<javac destdir>` and the usual
    /// output properties. Never sources.
    pub outputs: Vec<PathBuf>,
    /// The files that hold a `<junit>`, `<junitlauncher>` or `<testng>` task.
    pub junit_files: Vec<PathBuf>,
    /// Every directory a `<javac>` compiles that can be read without running
    /// the build: `srcdir`, nested `<src path>`, and `<src refid>` naming a
    /// `<path>` or `<dirset>` the build defines. Tests among them included.
    pub sources: Vec<PathBuf>,
    /// Whether some `<javac>` compiles from a directory that cannot be read
    /// without running the build: one under a property only the build sets,
    /// or a path id it never defines.
    pub sources_unread: bool,
    /// `<path id>` and `<dirset id>` definitions, for `<src refid>`.
    defined_paths: BTreeMap<String, Vec<PathBuf>>,
    /// `<src refid>` uses, by the output directory they compile into, resolved
    /// once every definition is read.
    referenced_sources: Vec<(PathBuf, String)>,
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
    // Ant resolves every relative path against the project's base directory,
    // which `<project basedir>` may move from the build file's own.
    let based = crate::fault_fs::read_to_string(&start)
        .ok()
        .and_then(|text| {
            tags(&text)
                .into_iter()
                .find(|tag| tag.name == "project")
                .and_then(|tag| tag.attribute("basedir").map(str::to_owned))
        })
        .filter(|basedir| !basedir.contains("${"))
        .map(|basedir| resolve(root, &basedir));
    let base = based.unwrap_or_else(|| normalized(root));
    let root = base.as_path();
    let mut properties = defines.clone();
    properties
        .entry("basedir".into())
        .or_insert_with(|| root.to_string_lossy().into_owned());
    // Properties a build sets at its top level hold before any target runs:
    // read first, they win over one a target that may never run would set.
    let mut ignored = AntBuild::default();
    read_file(
        root,
        &start,
        &mut properties,
        &mut ignored,
        &mut BTreeMap::new(),
        Pass::TopLevel,
    );
    let mut compiled = BTreeMap::new();
    read_file(
        root,
        &start,
        &mut properties,
        &mut build,
        &mut compiled,
        Pass::All,
    );
    for (destination, reference) in std::mem::take(&mut build.referenced_sources) {
        let Some(defined) = build.defined_paths.get(&reference).cloned() else {
            build.sources_unread = true;
            continue;
        };
        for source in defined {
            push_unique(compiled.entry(destination.clone()).or_default(), source);
        }
    }
    for sources in compiled.values() {
        for source in sources {
            push_unique(&mut build.sources, source.clone());
        }
    }
    // A batch that scans compiled classes -- `<junitlauncher>`'s
    // `<testclasses>` always does -- names the directory a `<javac>` wrote
    // them into: its tests are that compilation's sources.
    let scanned = std::mem::take(&mut build.test_roots);
    for root in scanned {
        match compiled.get(&root) {
            Some(sources) => sources
                .iter()
                .for_each(|source: &PathBuf| push_unique(&mut build.test_roots, source.clone())),
            None => push_unique(&mut build.test_roots, root),
        }
    }
    for name in OUTPUT_PROPERTIES {
        if let Some(value) = properties.get(*name).filter(|value| !value.contains("${")) {
            push_unique(&mut build.outputs, resolve(root, value));
        }
    }
    build
}

/// Which properties a reading defines: the top-level ones alone, or every
/// one, together with what the build says about its tests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    TopLevel,
    All,
}

/// `compiled` maps each `<javac>` output directory to its sources.
fn read_file(
    root: &Path,
    file: &Path,
    properties: &mut BTreeMap<String, String>,
    build: &mut AntBuild,
    compiled: &mut BTreeMap<PathBuf, Vec<PathBuf>>,
    pass: Pass,
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
    // An external entity (`<!ENTITY common SYSTEM "common.xml">`, then
    // `&common;`) puts another file's elements where it is referred to: the
    // older way of sharing build fragments, before `<import>`.
    for (name, system) in external_entities(&text) {
        if text.contains(&format!("&{name};")) {
            read_file(
                root,
                &directory.join(system),
                properties,
                build,
                compiled,
                pass,
            );
        }
    }
    let all = tags(&text);
    let mut in_batchtest = 0usize;
    let mut in_target = 0usize;
    // The `<subant>` being read: its build file name and where it runs them.
    let mut subant: Option<String> = None;
    // The `<path id>` being read, for the directories it lists.
    let mut defining: Option<String> = None;
    // The output directory of the `<javac>` being read, for its nested `<src>`.
    let mut javac: Option<PathBuf> = None;
    let known = |value: &str, properties: &BTreeMap<String, String>| {
        let value = expand(value, properties);
        (!value.contains("${")).then(|| resolve(root, &value))
    };
    for tag in &all {
        if tag.closing {
            if matches!(tag.name.as_str(), "batchtest" | "testclasses" | "testng") {
                in_batchtest = in_batchtest.saturating_sub(1);
            }
            if tag.name == "javac" {
                javac = None;
            }
            if tag.name == "target" {
                in_target = in_target.saturating_sub(1);
            }
            if tag.name == "subant" {
                subant = None;
            }
            if tag.name == "path" {
                defining = None;
            }
            continue;
        }
        if tag.name == "target" && !tag.empty {
            in_target += 1;
        }
        if pass == Pass::TopLevel
            && tag.name != "property"
            && tag.name != "import"
            && tag.name != "include"
        {
            continue;
        }
        match tag.name.as_str() {
            "property" if pass == Pass::All || in_target == 0 => define(root, tag, properties),
            "property" => {}
            "import" | "include" => {
                if let Some(imported) = tag.attribute("file") {
                    let imported = expand(imported, properties);
                    if !imported.contains("${") {
                        read_file(
                            root,
                            &directory.join(imported),
                            properties,
                            build,
                            compiled,
                            pass,
                        );
                    }
                }
            }
            // `<ant>` runs another build file, by default with the caller's
            // properties, its paths against its own directory.
            "ant" => {
                let directory = tag
                    .attribute("dir")
                    .and_then(|dir| known(dir, properties))
                    .unwrap_or_else(|| root.to_path_buf());
                let antfile = expand(tag.attribute("antfile").unwrap_or("build.xml"), properties);
                if !antfile.contains("${") {
                    read_sub_build(
                        &directory,
                        &resolve(&directory, &antfile),
                        properties,
                        build,
                        compiled,
                    );
                }
            }
            "subant" if !tag.empty => {
                subant = Some(expand(
                    tag.attribute("antfile").unwrap_or("build.xml"),
                    properties,
                ));
            }
            "fileset" | "dirset" | "filelist" if subant.is_some() => {
                let antfile = subant.clone().unwrap_or_else(|| "build.xml".into());
                let Some(dir) = tag.attribute("dir").and_then(|dir| known(dir, properties)) else {
                    continue;
                };
                let listed = if tag.name == "filelist" {
                    tag.attribute("files")
                        .map(|files| {
                            files
                                .split([',', ' '])
                                .filter(|f| !f.is_empty())
                                .map(|f| dir.join(expand(f, properties)))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    matching(&dir, tag.attribute("includes").unwrap_or("**"))
                };
                for path in listed {
                    // A dirset names the directories each build file is in.
                    let (directory, file) = if tag.name == "dirset" {
                        (path.clone(), path.join(&antfile))
                    } else {
                        (path.parent().unwrap_or(&dir).to_path_buf(), path)
                    };
                    read_sub_build(&directory, &file, properties, build, compiled);
                }
            }
            "batchtest" | "testclasses" | "testng" if !tag.empty => {
                in_batchtest += 1;
                if tag.name == "testng" {
                    push_unique(&mut build.junit_files, file.clone());
                }
            }
            "testng" => push_unique(&mut build.junit_files, file.clone()),
            "fileset" | "dirset" | "classfileset" if in_batchtest > 0 => {
                if let Some(dir) = tag.attribute("dir") {
                    let dir = expand(dir, properties);
                    if !dir.contains("${") {
                        push_unique(&mut build.test_roots, resolve(root, &dir));
                    }
                }
            }
            "javac" => {
                let destination = tag
                    .attribute("destdir")
                    .and_then(|dir| known(dir, properties));
                build.sources_unread |= destination.is_none();
                if let Some(destination) = &destination {
                    let listed = tag.attribute("srcdir").map(path_list).unwrap_or_default();
                    let sources = listed
                        .iter()
                        .filter_map(|dir| known(dir, properties))
                        .collect::<Vec<_>>();
                    build.sources_unread |= sources.len() < listed.len();
                    // A build that compiles in place writes its classes beside
                    // its sources: that directory is not only output.
                    if !sources.contains(destination) {
                        push_unique(&mut build.outputs, destination.clone());
                    }
                    let compiled_from = compiled.entry(destination.clone()).or_default();
                    for source in sources {
                        push_unique(compiled_from, source);
                    }
                }
                if !tag.empty {
                    javac = destination;
                }
            }
            "src" if javac.is_some() => {
                let Some(destination) = &javac else { continue };
                if let Some(path) = tag.attribute("path") {
                    for source in path_list(&expand(path, properties)) {
                        match known(&source, properties) {
                            Some(source) => push_unique(
                                compiled.entry(destination.clone()).or_default(),
                                source,
                            ),
                            None => build.sources_unread = true,
                        }
                    }
                }
                if let Some(reference) = tag.attribute("refid") {
                    build
                        .referenced_sources
                        .push((destination.clone(), reference.to_owned()));
                }
            }
            "path" if !tag.empty && tag.attribute("id").is_some() => {
                defining = tag.attribute("id").map(str::to_owned);
            }
            "pathelement" if defining.is_some() => {
                let listed = tag.attribute("location").or_else(|| tag.attribute("path"));
                let id = defining.clone().unwrap_or_default();
                for entry in listed
                    .map(|value| path_list(&expand(value, properties)))
                    .unwrap_or_default()
                {
                    if let Some(directory) = known(&entry, properties) {
                        push_unique(
                            build.defined_paths.entry(id.clone()).or_default(),
                            directory,
                        );
                    }
                }
            }
            "dirset" if tag.attribute("id").is_some() || defining.is_some() => {
                let id = tag
                    .attribute("id")
                    .map(str::to_owned)
                    .or_else(|| defining.clone())
                    .unwrap_or_default();
                if let Some(dir) = tag.attribute("dir").and_then(|dir| known(dir, properties)) {
                    let directories = matching(&dir, tag.attribute("includes").unwrap_or("*"))
                        .into_iter()
                        .filter(|path| path.is_dir());
                    for directory in directories {
                        push_unique(
                            build.defined_paths.entry(id.clone()).or_default(),
                            directory,
                        );
                    }
                }
            }
            "junit" | "junitlauncher" => push_unique(&mut build.junit_files, file.clone()),
            _ => {}
        }
    }
}

/// A build `<ant>` or `<subant>` runs: read as its own project, with the
/// caller's properties and its directory as the base for its paths.
fn read_sub_build(
    directory: &Path,
    file: &Path,
    properties: &BTreeMap<String, String>,
    build: &mut AntBuild,
    compiled: &mut BTreeMap<PathBuf, Vec<PathBuf>>,
) {
    let mut inherited = properties.clone();
    inherited.insert("basedir".into(), directory.to_string_lossy().into_owned());
    let mut ignored = AntBuild::default();
    read_file(
        directory,
        file,
        &mut inherited,
        &mut ignored,
        &mut BTreeMap::new(),
        Pass::TopLevel,
    );
    read_file(directory, file, &mut inherited, build, compiled, Pass::All);
}

/// The `<!ENTITY name SYSTEM "file">` declarations of a build file's DOCTYPE.
fn external_entities(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<!ENTITY") {
        rest = &rest[at + "<!ENTITY".len()..];
        let declaration = &rest[..rest.find('>').unwrap_or(rest.len())];
        let mut words = declaration.split_whitespace();
        let (Some(name), Some(kind)) = (words.next(), words.next()) else {
            continue;
        };
        if name == "%" || kind != "SYSTEM" {
            continue;
        }
        let quoted = declaration[declaration.find("SYSTEM").unwrap_or(0) + 6..].trim();
        let Some(quote) = quoted.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        if let Some(close) = quoted[1..].find(quote) {
            found.push((name.to_owned(), quoted[1..1 + close].to_owned()));
        }
    }
    found
}

/// The files under `directory` an Ant pattern list names (`*/build.xml`,
/// `modules/**/build.xml`), comma- or space-separated, in sorted order.
fn matching(directory: &Path, patterns: &str) -> Vec<PathBuf> {
    let patterns = patterns
        .split([',', ' '])
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>();
    let mut found = Vec::new();
    let mut pending = vec![(directory.to_path_buf(), 0usize)];
    while let Some((next, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(directory) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                if depth < 8 {
                    pending.push((path.clone(), depth + 1));
                }
                if patterns
                    .iter()
                    .any(|pattern| ant_pattern(pattern, &relative))
                {
                    found.push(path);
                }
                continue;
            }
            if patterns
                .iter()
                .any(|pattern| ant_pattern(pattern, &relative))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Whether a `/`-separated path matches an Ant pattern: `**` any number of
/// directories, `*` any run within one name, `?` one character.
fn ant_pattern(pattern: &str, path: &str) -> bool {
    fn names(pattern: &[&str], path: &[&str]) -> bool {
        match (pattern.first(), path.first()) {
            (None, None) => true,
            (Some(&"**"), _) => {
                names(&pattern[1..], path) || (!path.is_empty() && names(pattern, &path[1..]))
            }
            (Some(part), Some(name)) => {
                one(part.as_bytes(), name.as_bytes()) && names(&pattern[1..], &path[1..])
            }
            _ => false,
        }
    }
    fn one(pattern: &[u8], name: &[u8]) -> bool {
        match (pattern.first(), name.first()) {
            (None, None) => true,
            (Some(b'*'), _) => {
                one(&pattern[1..], name) || (!name.is_empty() && one(pattern, &name[1..]))
            }
            (Some(b'?'), Some(_)) => one(&pattern[1..], &name[1..]),
            (Some(expected), Some(actual)) => expected == actual && one(&pattern[1..], &name[1..]),
            _ => false,
        }
    }
    // A pattern that ends in `/` names everything below it, as `/**` would.
    let mut pattern = pattern.trim_start_matches('/').to_owned();
    if pattern.is_empty() || pattern.ends_with('/') {
        pattern.push_str("**");
    }
    names(
        &pattern.split('/').collect::<Vec<_>>(),
        &path.split('/').collect::<Vec<_>>(),
    )
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

/// An Ant path-like attribute's entries: `:` or `;` between them, except the
/// colon of a Windows drive (`C:\src`).
fn path_list(value: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let characters = value.chars().collect::<Vec<_>>();
    for (index, &character) in characters.iter().enumerate() {
        let drive = character == ':'
            && current.len() == 1
            && current.chars().all(|c| c.is_ascii_alphabetic())
            && matches!(characters.get(index + 1), Some('\\' | '/'));
        if (character == ':' && !drive) || character == ';' {
            if !current.trim().is_empty() {
                entries.push(current.trim().to_owned());
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if !current.trim().is_empty() {
        entries.push(current.trim().to_owned());
    }
    entries
}

/// `key=value` and `key: value` lines, as java.util.Properties reads the
/// plain ones. Continuation lines and escapes beyond the common ones are not
/// needed for the paths a build names.
fn properties_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim_start)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .filter_map(|line| {
            // The separator is the first `=` or `:` that is not escaped.
            let bytes = line.as_bytes();
            let mut split = None;
            let mut index = 0;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index += 1,
                    b'=' | b':' => {
                        split = Some(index);
                        break;
                    }
                    _ => {}
                }
                index += 1;
            }
            let split = split?;
            Some((
                unescape_property(line[..split].trim()),
                unescape_property(line[split + 1..].trim()),
            ))
        })
        .collect()
}

/// java.util.Properties escapes: `\:` `\=` `\\` and the like stand for
/// the character, `\t` `\n` `\r` for theirs, `\uXXXX` for a code point.
fn unescape_property(value: &str) -> String {
    let mut out = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let code = characters.by_ref().take(4).collect::<String>();
                out.extend(u32::from_str_radix(&code, 16).ok().and_then(char::from_u32));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// `${name}` replaced by each known property, unknown ones left as written.
pub fn expand(value: &str, properties: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(open) = rest.find("${") {
        // `$$` is Ant's escape for a literal `$`: `$${x}` is the text `${x}`.
        if rest[..open].ends_with('$') {
            out.push_str(&rest[..open - 1]);
            out.push_str("${");
            rest = &rest[open + 2..];
            continue;
        }
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
        let found = tags(
            r#"<?xml version="1.0"?><!-- <junit> --><project a='x>y'><![CDATA[<javac>]]>< ><junit fork="true"/><echo level=info message="unread"/></project>"#,
        );
        let names: Vec<_> = found
            .iter()
            .map(|tag| (tag.name.as_str(), tag.closing, tag.empty))
            .collect();
        assert_eq!(
            names,
            [
                ("project", false, false),
                ("junit", false, true),
                ("echo", false, true),
                ("project", true, false)
            ]
        );
        assert_eq!(found[0].attribute("a"), Some("x>y"));
        // An unquoted value is not XML, and ends what can be read of the tag.
        assert_eq!(found[2].attribute("level"), None);
        assert_eq!(found[2].attribute("message"), None);
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
        fs::write(
            project.join("build.properties"),
            "# comment\nerrorOnUnusedKeys=false\n",
        )
        .unwrap();
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
        assert_eq!(
            build.files,
            [project.join("build.xml"), common.join("common-build.xml")]
        );
        assert_eq!(build.test_roots, [project.join("test/src")]);
        assert_eq!(build.junit_files, [common.join("common-build.xml")]);
        assert!(
            build.outputs.contains(&project.join("tmp")),
            "{:?}",
            build.outputs
        );
        assert!(
            build.outputs.contains(&project.join("tmp/classes-test")),
            "the first tmp.dir wins: {:?}",
            build.outputs
        );
        assert_eq!(outside_anchor(&project, &build), Some(top.clone()));
        fs::remove_dir_all(top).unwrap();
    }

    #[test]
    fn a_batch_of_compiled_classes_names_the_sources_compiled_into_them() {
        let project = temporary("compiled");
        fs::write(
            project.join("build.xml"),
            r#"<project><target name="t">
    <javac srcdir="src/main" destdir="out/classes"/>
    <javac destdir="out/test-classes"><src path="src/test"/></javac>
    <junitlauncher><testclasses><fileset dir="out/test-classes" includes="**/*Test.class"/></testclasses></junitlauncher>
</target></project>"#,
        )
        .unwrap();
        let build = read_ant_build(&project, &BTreeMap::new());
        assert_eq!(build.test_roots, [project.join("src/test")]);
        assert_eq!(build.junit_files, [project.join("build.xml")]);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn top_level_properties_win_over_a_target_and_basedir_moves_paths() {
        let top = temporary("basedir");
        let project = top.join("build");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("build.xml"),
            r#"<project basedir="..">
    <target name="it"><property name="test.dir" value="it"/></target>
    <property name="test.dir" value="unit"/>
    <target name="t"><javac srcdir="src" destdir="src"/><junit><batchtest><fileset dir="${test.dir}"/></batchtest></junit></target>
</project>"#,
        )
        .unwrap();
        let build = read_ant_build(&project, &BTreeMap::new());
        assert_eq!(
            build.test_roots,
            [top.join("unit")],
            "the top-level definition, under basedir"
        );
        assert!(
            !build.outputs.contains(&top.join("src")),
            "an in-place compile is not output: {:?}",
            build.outputs
        );
        assert_eq!(path_list("src:lib;C:\\other"), ["src", "lib", "C:\\other"]);
        fs::remove_dir_all(top).unwrap();
    }

    /// Fragments shared through an entity, a module run by `<ant>`, and
    /// modules a `<subant>` finds by pattern: each one's tests are read, each
    /// against its own directory.
    #[test]
    fn entities_ant_and_subant_builds_are_followed() {
        let project = temporary("sub-builds");
        let write = |relative: &str, text: &str| {
            let path = project.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "build.xml",
            r#"<?xml version="1.0"?>
<!DOCTYPE project [ <!ENTITY shared SYSTEM "shared.xml"> ]>
<project name="top">
    &shared;
    <target name="all">
        <ant dir="core" antfile="core.xml"/>
        <subant><fileset dir="modules" includes="*/build.xml"/></subant>
    </target>
</project>"#,
        );
        write(
            "shared.xml",
            r#"<target name="shared"><junit><batchtest><fileset dir="shared-tests"/></batchtest></junit></target>"#,
        );
        write(
            "core/core.xml",
            r#"<project><target name="t"><junit><batchtest><fileset dir="test"/></batchtest></junit></target></project>"#,
        );
        write(
            "modules/a/build.xml",
            r#"<project><target name="t"><junit><batchtest><fileset dir="tests"/></batchtest></junit></target></project>"#,
        );
        write("modules/b/notes.xml", "<project/>");
        let build = read_ant_build(&project, &BTreeMap::new());
        assert_eq!(
            build.test_roots,
            [
                project.join("shared-tests"),
                project.join("core/test"),
                project.join("modules/a/tests")
            ],
            "{build:?}"
        );
        assert_eq!(
            build.junit_files,
            [
                project.join("shared.xml"),
                project.join("core/core.xml"),
                project.join("modules/a/build.xml")
            ]
        );
        assert!(ant_pattern("**/build.xml", "a/b/build.xml"));
        assert!(ant_pattern("mod?/*.xml", "mod1/x.xml"));
        assert!(!ant_pattern("*/build.xml", "a/b/build.xml"));
        fs::remove_dir_all(project).unwrap();
    }

    /// What a build holds that cannot or need not be followed is stepped
    /// past, and what can be is still read around it.
    #[test]
    fn the_build_reads_past_what_it_cannot_follow() {
        let project = temporary("reads-past");
        let write = |relative: &str, text: &str| {
            let path = project.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "build.xml",
            r#"<?xml version="1.0"?>
<!DOCTYPE project [
    <!ENTITY % parameters SYSTEM "parameters.dtd">
    <!ENTITY inline "text">
    <!ENTITY lonely>
    <!ENTITY single SYSTEM 'single.xml'>
    <!ENTITY bare SYSTEM bare.xml>
    <!ENTITY open SYSTEM "open.xml>
    <!ENTITY unused SYSTEM "unused.xml">
]>
<project name="past">
    &single; &bare; &open;
    <property name="unset" location="${nowhere}/x"/>
    <property name="referenced" refid="somewhere"/>
    <property environment="env"/>
    <property file="missing.properties"/>
    <import file="${nowhere}/common.xml"/>
    <import file="cycle.xml"/>
    <target name="empty"/>
    <target name="all">
        <ant antfile="${nowhere}.xml"/>
        <subant antfile="part.xml"/>
        <subant antfile="part.xml">
            <filelist dir="parts" files="one/part.xml,two/part.xml"/>
            <dirset dir="dirs" includes="*"/>
        </subant>
        <testng outputdir="out"/>
        <junit><batchtest><fileset refid="tests"/><fileset dir="${nowhere}"/></batchtest></junit>
        <apt destdir="out/apt"><src path="annotated"/></apt>
        <javac destdir="out/classes"><classpath><path><pathelement location="lib"/></path></classpath></javac>
        <copy todir="out"><dirset dir="java"/></copy>
    </target>
    <path id="empty"/>
    <dirset id="dirs" dir="${nowhere}"/>
</project>"#,
        );
        write(
            "cycle.xml",
            r#"<project><import file="build.xml"/></project>"#,
        );
        let batch = |dir: &str| {
            format!(
                r#"<project><target name="t"><junit><batchtest><fileset dir="{dir}"/></batchtest></junit></target></project>"#
            )
        };
        write("single.xml", &batch("single-tests"));
        write("unused.xml", &batch("unused-tests"));
        write("parts/one/part.xml", &batch("t1"));
        write("parts/two/part.xml", &batch("t2"));
        write("dirs/x/part.xml", &batch("tx"));
        let build = read_ant_build(&project, &BTreeMap::new());
        assert_eq!(
            build.test_roots,
            [
                project.join("single-tests"),
                project.join("parts/one/t1"),
                project.join("parts/two/t2"),
                project.join("dirs/x/tx"),
            ],
            "{build:?}"
        );
        assert!(build.junit_files.contains(&project.join("build.xml")));
        assert!(build.sources.is_empty(), "{build:?}");
        assert!(build.files.contains(&project.join("cycle.xml")));

        // An import chain stops somewhere: past 64 files, nothing more is read.
        for index in 0..70 {
            write(
                &format!("chain/{index}.xml"),
                &format!(r#"<project><import file="{}.xml"/></project>"#, index + 1),
            );
        }
        let build = read_ant_build(&project.join("chain"), &BTreeMap::new());
        assert!(build.files.is_empty(), "no build.xml there: {build:?}");
        fs::copy(project.join("chain/0.xml"), project.join("chain/build.xml")).unwrap();
        let build = read_ant_build(&project.join("chain"), &BTreeMap::new());
        assert_eq!(build.files.len(), 65, "{build:?}");
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn patterns_path_lists_and_properties_read_their_edges() {
        assert!(!ant_pattern("**/b", ""));
        assert!(!ant_pattern("*b", "a"));
        assert!(ant_pattern("modules/", "modules/a/build.xml"));
        assert!(ant_pattern("/", "anything/at/all"));
        assert_eq!(path_list("1:/a;C:rel::;"), ["1", "/a", "C", "rel"]);
        assert_eq!(
            properties_file("\n   \n!bang=1\nno separator\nkey=value"),
            [("key".to_owned(), "value".to_owned())]
        );
        // A directory deeper than eight levels is not searched.
        let top = temporary("deep");
        let deep = (0..10).fold(top.clone(), |path, level| path.join(level.to_string()));
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("build.xml"), "<project/>").unwrap();
        fs::write(top.join("0/build.xml"), "<project/>").unwrap();
        assert_eq!(matching(&top, "**/build.xml"), [top.join("0/build.xml")]);
        fs::remove_dir_all(top).unwrap();
    }

    /// Every `<javac>` source that can be read: an attribute, nested
    /// `<src path>`, and `<src refid>` naming a path or dirset the build
    /// defines, even after the compilation that refers to it.
    #[test]
    fn javac_sources_are_read_through_paths_and_dirsets() {
        let project = temporary("javac-sources");
        fs::create_dir_all(project.join("modules/billing/java")).unwrap();
        fs::create_dir_all(project.join("modules/orders/java")).unwrap();
        fs::write(
            project.join("build.xml"),
            r#"<project>
    <target name="compile">
        <javac srcdir="java" destdir="out/a"/>
        <javac destdir="out/b"><src path="generated:extra"/><src refid="modules"/></javac>
    </target>
    <path id="modules"><pathelement location="shared/java"/><dirset dir="modules" includes="*/java"/></path>
</project>"#,
        )
        .unwrap();
        let build = read_ant_build(&project, &BTreeMap::new());
        assert_eq!(
            build.sources,
            [
                project.join("java"),
                project.join("generated"),
                project.join("extra"),
                project.join("shared/java"),
                project.join("modules/billing/java"),
                project.join("modules/orders/java"),
            ],
            "{build:?}"
        );
        assert!(!build.sources_unread, "{build:?}");
        // Each way a compilation's sources can be known only to the build
        // that runs: an output, a source directory or a nested path under a
        // property it sets, and a path id it never defines.
        for javac in [
            r#"<javac srcdir="java" destdir="${classes}"/>"#,
            r#"<javac srcdir="java:${generated}" destdir="out"/>"#,
            r#"<javac destdir="out"><src path="${generated}"/></javac>"#,
            r#"<javac destdir="out"><src refid="made.at.run.time"/></javac>"#,
        ] {
            fs::write(
                project.join("build.xml"),
                format!(r#"<project><target name="compile">{javac}</target></project>"#),
            )
            .unwrap();
            let build = read_ant_build(&project, &BTreeMap::new());
            assert!(build.sources_unread, "{javac}: {build:?}");
        }
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn properties_files_are_read_with_their_escapes() {
        assert_eq!(
            properties_file("a\\:b=c\\:\\\\d\nsrc.dir : C\\:\\\\src\n# skip\ntab=\\t\\u0041\n"),
            [
                ("a:b".to_owned(), "c:\\d".to_owned()),
                ("src.dir".to_owned(), "C:\\src".to_owned()),
                ("tab".to_owned(), "\tA".to_owned()),
            ]
        );
        let known = BTreeMap::from([("x".to_owned(), "1".to_owned())]);
        assert_eq!(expand("$${x} ${x} $${y}", &known), "${x} 1 ${y}");
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
        assert_eq!(
            read_ant_build(&project.join("missing"), &BTreeMap::new()),
            AntBuild::default()
        );
        fs::remove_dir_all(project).unwrap();
    }
}
