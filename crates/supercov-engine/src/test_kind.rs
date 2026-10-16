//! A Python or Ruby test's kind from where it lives, read as the JavaScript
//! runtime reads one (`runtime/javascript/provenance.mjs`): the nearest
//! directory that names a kind, then the file's own name, then `unit`. A
//! Rails app's Capybara tests live in `test/system` or `spec/features`, and
//! its request specs in `spec/requests`; every one of them was reported as a
//! unit test.

const KINDS: &[(&str, &[&str])] = &[
    ("unit", &["unit"]),
    ("component", &["component", "components", "ct"]),
    ("integration", &["integration", "int", "requests"]),
    ("e2e", &["e2e", "end-to-end", "offline", "online"]),
];

/// Directories that say what a suite is only as a whole name.
const DIRECTORIES: &[(&str, &str)] = &[("system", "e2e"), ("features", "e2e")];

/// The kind one path segment names: the word furthest into it wins, as in
/// `checkout-integration-e2e`.
fn segment_kind(segment: &str) -> Option<&'static str> {
    // Words split on `_`, `.`, `-` and camel case, keeping `end-to-end`.
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for character in segment.chars() {
        if character.is_ascii_uppercase()
            && previous.is_some_and(|before| before.is_ascii_lowercase() || before.is_ascii_digit())
        {
            spaced.push(' ');
        }
        spaced.push(if matches!(character, '_' | '.') {
            ' '
        } else {
            character.to_ascii_lowercase()
        });
        previous = Some(character);
    }
    let spaced = spaced.replace("end-to-end", "end~to~end").replace('-', " ");
    let mut best = None;
    for (position, word) in spaced.split(' ').enumerate() {
        let word = word.replace('~', "-");
        if let Some((kind, _)) = KINDS
            .iter()
            .find(|(_, words)| words.contains(&word.as_str()))
        {
            best = Some((position, *kind));
        }
    }
    best.map(|(_, kind)| kind)
}

/// The kind a tag names, as `@e2e` does: a test's own statement of what it
/// is, which outranks where it lives.
pub fn from_tags(tags: &[String]) -> Option<&'static str> {
    tags.iter().find_map(
        |tag| match tag.trim_start_matches('@').to_ascii_lowercase().as_str() {
            "unit" => Some("unit"),
            "component" => Some("component"),
            "integration" => Some("integration"),
            "e2e" => Some("e2e"),
            _ => None,
        },
    )
}

/// A test's kind: the one a tag names, else the one its path names.
pub fn with_tags(file: Option<&str>, tags: &[String]) -> &'static str {
    from_tags(tags).unwrap_or_else(|| from_path(file))
}

pub fn from_path(file: Option<&str>) -> &'static str {
    let Some(file) = file else {
        return "unit";
    };
    let segments = file.split(['/', '\\']).collect::<Vec<_>>();
    let (name, directories) = segments.split_last().unwrap_or((&"", &[]));
    directories
        .iter()
        .rev()
        .find_map(|directory| {
            DIRECTORIES
                .iter()
                .find(|(name, _)| directory.eq_ignore_ascii_case(name))
                .map(|(_, kind)| *kind)
                .or_else(|| segment_kind(directory))
        })
        .or_else(|| segment_kind(name))
        .unwrap_or("unit")
}

#[cfg(test)]
mod tests {
    use super::{from_path, from_tags, with_tags};

    #[test]
    fn a_tag_naming_a_level_is_the_kind_and_outranks_the_path() {
        let tags = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(from_tags(&tags(&["api", "@E2E"])), Some("e2e"));
        assert_eq!(from_tags(&tags(&["api", "smoke"])), None);
        assert_eq!(
            with_tags(Some("spec/models/a_spec.rb"), &tags(&["integration"])),
            "integration"
        );
        assert_eq!(
            with_tags(Some("test/e2e/a_test.py"), &tags(&["api"])),
            "e2e"
        );
    }

    #[test]
    fn a_test_is_the_kind_its_directory_names() {
        assert_eq!(from_path(Some("test/system/composer_test.rb")), "e2e");
        assert_eq!(from_path(Some("spec/features/sign_in_spec.rb")), "e2e");
        assert_eq!(
            from_path(Some("spec/requests/rooms_spec.rb")),
            "integration"
        );
        assert_eq!(
            from_path(Some("tests/integration/test_api.py")),
            "integration"
        );
        assert_eq!(from_path(Some("tests/e2e/test_flow.py")), "e2e");
        assert_eq!(from_path(Some("test/models/user_test.rb")), "unit");
        // The nearest directory wins, then the file's name.
        assert_eq!(from_path(Some("tests/e2e/unit/test_x.py")), "unit");
        assert_eq!(from_path(Some("tests/test_checkout_e2e.py")), "e2e");
        assert_eq!(from_path(Some("tests/end-to-end/test_x.py")), "e2e");
        assert_eq!(
            from_path(Some("spec/apiIntegration_spec.rb")),
            "integration"
        );
        // A word inside another word is not a kind.
        assert_eq!(from_path(Some("tests/interior/test_x.py")), "unit");
        assert_eq!(from_path(None), "unit");
    }
}
