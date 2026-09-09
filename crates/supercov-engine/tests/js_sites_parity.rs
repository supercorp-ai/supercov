//! Parity between the engine's effect-site discovery and the prototype that defined the rules.
//!
//! The prototype writes `inventory.json` from a TypeScript-compiler pass over a whole project. This test
//! runs the engine's oxc pass over the same files and compares the site sets: category, classification,
//! note and position, per file. It is the same discipline as the join's parity test, applied to the other
//! half of the denominator.
//!
//! Two rules of the prototype need a type checker, which oxc does not provide, so the sites they produce
//! are excluded from the comparison and counted instead: a mutator call whose receiver must be shown to
//! be a container, and a method call whose receiver must be shown to have a project-declared type. The
//! engine declares those as limitations.
//!
//! Opt-in, since the fixtures live outside this repository:
//!
//! ```sh
//! SUPERCOV_SITES_INVENTORY=/path/to/out/inventory.json \
//! SUPERCOV_SITES_ROOT=/path/to/project \
//!   cargo test -p supercov-engine --test js_sites_parity -- --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use supercov_engine::js_sites::discover_effect_sites;

#[derive(Debug, Deserialize)]
struct Inventory {
    sites: Vec<InventorySite>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InventorySite {
    file: String,
    kind: String,
    category: String,
    classification: String,
    start: InventoryPosition,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct InventoryPosition {
    line: usize,
    column: usize,
}

/// What a site is, for comparison: where it is, what it is, and how it is classified. Ids differ by
/// construction and text is informational, so neither takes part.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    line: usize,
    column: usize,
    category: String,
    classification: String,
    note: Option<String>,
}

/// Sites the prototype can only classify with type information.
fn needs_types(site: &InventorySite) -> bool {
    site.category == "state-call"
        || site
            .note
            .as_deref()
            .is_some_and(|note| note.starts_with("mutator:"))
}

#[test]
fn the_engine_finds_the_same_effect_sites_as_the_prototype() {
    let (Ok(inventory_path), Ok(root)) = (
        std::env::var("SUPERCOV_SITES_INVENTORY"),
        std::env::var("SUPERCOV_SITES_ROOT"),
    ) else {
        eprintln!(
            "skipped: set SUPERCOV_SITES_INVENTORY and SUPERCOV_SITES_ROOT to compare against the \
             prototype's inventory"
        );
        return;
    };
    let inventory: Inventory = serde_json::from_str(
        &std::fs::read_to_string(&inventory_path).expect("inventory.json is readable"),
    )
    .expect("inventory.json parses");

    let mut expected: BTreeMap<String, Vec<Key>> = BTreeMap::new();
    let mut want_gated: BTreeMap<String, BTreeSet<(usize, usize)>> = BTreeMap::new();
    let mut skipped_for_types = 0usize;
    for site in &inventory.sites {
        if site.kind != "effect" {
            continue; // decisions are supercov's own, and already in the manifest
        }
        if needs_types(site) {
            skipped_for_types += 1;
            want_gated
                .entry(site.file.clone())
                .or_default()
                .insert((site.start.line, site.start.column));
            continue;
        }
        expected.entry(site.file.clone()).or_default().push(Key {
            line: site.start.line,
            column: site.start.column,
            category: site.category.clone(),
            classification: site.classification.clone(),
            note: site.note.clone(),
        });
    }

    let mut missing: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();
    let mut matched = 0usize;
    let mut limitations: BTreeSet<String> = BTreeSet::new();
    let mut unreadable: Vec<String> = Vec::new();
    for (file, want) in &expected {
        let path = std::path::Path::new(&root).join(file);
        let Ok(source) = std::fs::read_to_string(&path) else {
            unreadable.push(file.clone());
            continue;
        };
        let found = match discover_effect_sites(file, &source) {
            Ok(found) => found,
            Err(error) => {
                unreadable.push(format!("{file}: {error}"));
                continue;
            }
        };
        limitations.extend(found.limitations.iter().cloned());
        // Both type-gated rules land on a position where the two passes cannot agree by construction:
        // the prototype's answer depends on the receiver's type, and the engine's on syntax alone. Such
        // positions are dropped from both sides rather than counted as a difference in either
        // direction, and reported as excluded.
        let mut gated: BTreeSet<(usize, usize)> = want_gated.get(file).cloned().unwrap_or_default();
        for site in &found.sites {
            if site
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with("mutator:"))
            {
                gated.insert((site.start.line, site.start.column));
            }
        }
        let ours: BTreeSet<Key> = found
            .sites
            .iter()
            .filter(|site| !gated.contains(&(site.start.line, site.start.column)))
            .map(|site| Key {
                line: site.start.line,
                column: site.start.column,
                category: site.category.as_str().to_owned(),
                classification: site.classification.as_str().to_owned(),
                note: site.note.clone(),
            })
            .collect();
        let theirs: BTreeSet<Key> = want
            .iter()
            .filter(|key| !gated.contains(&(key.line, key.column)))
            .cloned()
            .collect();
        for key in theirs.difference(&ours) {
            let text = inventory
                .sites
                .iter()
                .find(|site| {
                    site.file == *file
                        && site.start.line == key.line
                        && site.category == key.category
                })
                .map(|site| site.text.chars().take(60).collect::<String>())
                .unwrap_or_default();
            missing.push(format!(
                "{file}:{}:{} {} {} {:?}  {text}",
                key.line, key.column, key.category, key.classification, key.note
            ));
        }
        for key in ours.difference(&theirs) {
            extra.push(format!(
                "{file}:{}:{} {} {} {:?}",
                key.line, key.column, key.category, key.classification, key.note
            ));
        }
        matched += theirs.intersection(&ours).count();
    }

    let total: usize = expected.values().map(Vec::len).sum();
    eprintln!(
        "{} files, {total} comparable effect sites: {matched} matched, {} missing, {} extra \
         ({skipped_for_types} excluded as needing types)",
        expected.len(),
        missing.len(),
        extra.len(),
    );
    if !limitations.is_empty() {
        eprintln!(
            "limitations declared: {}",
            limitations.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    let report = |label: &str, items: &[String]| {
        if !items.is_empty() {
            eprintln!("\n{} {label}:", items.len());
            for line in items.iter().take(30) {
                eprintln!("  {line}");
            }
        }
    };
    report("missing from the engine", &missing);
    report("found only by the engine", &extra);
    report("files that could not be analyzed", &unreadable);
    // A `this.field.method()` call is a review site unless the enclosing class declares that method.
    // Whether the method belongs to a class or an interface needs types, so the engine reports the site
    // rather than staying silent: never fewer than a type checker would find, sometimes more. Extra
    // sites of exactly that shape are the declared approximation; anything else is a difference.
    let approximate = |line: &String| line.contains("this-callback");
    let unexpected_extra: Vec<String> = extra
        .iter()
        .filter(|line| !approximate(line))
        .cloned()
        .collect();
    let approximated = extra.len() - unexpected_extra.len();
    if approximated > 0 {
        eprintln!("\n{approximated} extra review sites are the declared `this` call approximation");
    }
    assert!(
        missing.is_empty() && unexpected_extra.is_empty() && unreadable.is_empty(),
        "the engine's site set differs from the prototype's: {} missing, {} unexpected extra, {} unreadable",
        missing.len(),
        unexpected_extra.len(),
        unreadable.len(),
    );
}

/// A per-file breakdown, printed rather than asserted: useful when a rule regresses on one shape.
#[test]
fn the_engine_reports_where_it_differs_by_category() {
    let (Ok(inventory_path), Ok(root)) = (
        std::env::var("SUPERCOV_SITES_INVENTORY"),
        std::env::var("SUPERCOV_SITES_ROOT"),
    ) else {
        return;
    };
    let inventory: Inventory = serde_json::from_str(
        &std::fs::read_to_string(&inventory_path).expect("inventory.json is readable"),
    )
    .expect("inventory.json parses");
    let mut theirs: BTreeMap<String, usize> = BTreeMap::new();
    let mut ours: BTreeMap<String, usize> = BTreeMap::new();
    let mut files: BTreeSet<String> = BTreeSet::new();
    for site in &inventory.sites {
        if site.kind != "effect" || needs_types(site) {
            continue;
        }
        files.insert(site.file.clone());
        *theirs.entry(site.category.clone()).or_default() += 1;
    }
    for file in &files {
        let path = std::path::Path::new(&root).join(file);
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(found) = discover_effect_sites(file, &source) {
            for site in &found.sites {
                *ours.entry(site.category.as_str().to_owned()).or_default() += 1;
            }
        }
    }
    eprintln!("\ncategory              prototype   engine");
    for category in theirs.keys().chain(ours.keys()).collect::<BTreeSet<_>>() {
        eprintln!(
            "{category:<20} {:>9} {:>8}",
            theirs.get(category).copied().unwrap_or_default(),
            ours.get(category).copied().unwrap_or_default(),
        );
    }
    let _ = &inventory.sites.first().map(|site| &site.owner);
}
