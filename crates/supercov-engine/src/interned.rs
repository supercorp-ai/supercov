//! Ids a coverage report names over and over, stored once.
//!
//! A report says, for every obligation and line, which tests and phases
//! reached it, and for every test and phase, what it reached. On a
//! 3,900-test run that is some twenty million names drawn from a few tens of
//! thousands of distinct ones -- test ids, phase ids, obligation ids and file
//! paths. Held as owned strings each was its own allocation of about a
//! hundred bytes, and analysing that run peaked above 4 GB. An `Id` is a
//! shared pointer to one copy: sixteen bytes in a list, and a clone is a
//! count. It serializes as the string it holds, so nothing a report writes
//! changes.

use std::{
    borrow::Borrow,
    collections::HashSet,
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};

use serde::{Serialize, Serializer};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id(Arc<str>);

impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for Id {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Id {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Id {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Hashes as the text it holds, so a set of ids can be searched with a `&str`.
impl Hash for Id {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (*self.0).hash(state);
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, formatter)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, formatter)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl From<&str> for Id {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for Id {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

impl From<&String> for Id {
    fn from(value: &String) -> Self {
        Self(Arc::from(value.as_str()))
    }
}

impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0.to_string()
    }
}

impl PartialEq<str> for Id {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for Id {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl PartialEq<String> for Id {
    fn eq(&self, other: &String) -> bool {
        &*self.0 == other.as_str()
    }
}

impl PartialEq<Id> for String {
    fn eq(&self, other: &Id) -> bool {
        self.as_str() == &*other.0
    }
}

impl PartialEq<Id> for str {
    fn eq(&self, other: &Id) -> bool {
        self == &*other.0
    }
}

impl PartialEq<Id> for &str {
    fn eq(&self, other: &Id) -> bool {
        *self == &*other.0
    }
}

/// One `Id` per distinct text, for the life of one analysis.
#[derive(Default)]
pub struct Interner {
    ids: HashSet<Id>,
}

impl Interner {
    pub fn id(&mut self, text: &str) -> Id {
        if let Some(id) = self.ids.get(text) {
            return id.clone();
        }
        let id = Id::from(text);
        self.ids.insert(id.clone());
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_the_text_it_holds_everywhere_it_is_seen() {
        let mut interner = Interner::default();
        let first = interner.id("h11/tests/test_x.py::test_a");
        let again = interner.id("h11/tests/test_x.py::test_a");
        assert!(
            Arc::ptr_eq(&first.0, &again.0),
            "one copy per distinct text"
        );
        assert_eq!(first, "h11/tests/test_x.py::test_a");
        assert_eq!(
            serde_json::to_string(&vec![first.clone()]).unwrap(),
            r#"["h11/tests/test_x.py::test_a"]"#
        );
        let mut set = std::collections::BTreeSet::new();
        set.insert(interner.id("b"));
        set.insert(interner.id("a"));
        assert!(set.contains("a"), "found by text");
        assert_eq!(
            set.into_iter().map(String::from).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
