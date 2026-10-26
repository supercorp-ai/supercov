//! The Kotlin grammar, built from `vendor/tree-sitter-kotlin`: upstream's
//! generated parser with scanner fixes it has not released.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_kotlin() -> *const ();
}

/// The grammar, for a tree-sitter parser.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_kotlin) };
