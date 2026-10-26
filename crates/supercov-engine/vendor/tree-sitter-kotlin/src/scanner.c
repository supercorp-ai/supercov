#include "tree_sitter/parser.h"

#include <string.h>
#include <wctype.h>

enum TokenType {
    SEMI,
    CLASS_MEMBER_SEMI,
    BLOCK_COMMENT,
    NOT_IS,
    IN,
    Q_DOT,
    MULTILINE_STRING_CONTENT,
    CONSTRUCTOR,
    GET,
    SET,
    DOLLAR,
    ACCESSOR_SEMI,
};

#define MAX_WORD_SIZE 16
#define MAX_WORDS 16

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static inline void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

static bool scan_word(TSLexer *lexer, const char *const word) {
    for (uint8_t i = 0; word[i] != '\0'; i++) {
        if (lexer->lookahead != word[i]) {
            return false;
        }
        skip(lexer);
    }
    return true;
}

static bool scan_words(TSLexer *lexer, const char words[MAX_WORDS][MAX_WORD_SIZE], char scanned_word[16],
                       uint8_t *index) {
    if (!scanned_word[0]) {
        for (uint8_t i = 0; i < MAX_WORD_SIZE - 1; i++) {
            // Read the whole identifier (letters, digits, `_`), so that `in1`, `open_file` or
            // `set_value` are not mistaken for the keywords `in`, `open` or `set`.
            if (!(iswalpha(lexer->lookahead) || (i > 0 && (iswdigit(lexer->lookahead) || lexer->lookahead == '_')))) {
                if (i == 0) {
                    return false;
                }
                break;
            }
            scanned_word[i] = (char)lexer->lookahead;
            skip(lexer);
        }
    }

    for (uint8_t i = 0; i < MAX_WORDS; i++) {
        if (strncmp(scanned_word, words[i], MAX_WORD_SIZE) == 0) {
            if (index != NULL) {
                *index = i;
            }
            return true;
        }
    }

    return false;
}

// With the lookahead on the `(` after a `set` that may be a setter: true when it is a call on a
// new statement. A setter's parameter is a name, possibly annotated (`set(v) {`, `set(@A v) {`);
// `set("k") {` and `set("k", v)` are calls.
static bool set_paren_is_call(TSLexer *lexer) {
    skip(lexer);
    while (iswspace(lexer->lookahead)) {
        skip(lexer);
    }
    if (!(iswalpha(lexer->lookahead) || lexer->lookahead > 127 || lexer->lookahead == '_' ||
          lexer->lookahead == '`' || lexer->lookahead == '@')) {
        return true;
    }
    if (lexer->lookahead != '@') {
        // A setter takes one name, optionally typed: `set(a, b)` and `set(f(x))` are calls.
        while (iswalnum(lexer->lookahead) || lexer->lookahead > 127 || lexer->lookahead == '_' ||
               lexer->lookahead == '`') {
            skip(lexer);
        }
        while (iswspace(lexer->lookahead)) {
            skip(lexer);
        }
        if (lexer->lookahead != ':' && lexer->lookahead != ')') {
            return true;
        }
    }
    // skip until the closing parenthesis
    while (lexer->lookahead != ')' && !lexer->eof(lexer)) {
        skip(lexer);
    }
    skip(lexer);

    while (iswspace(lexer->lookahead)) {
        if (lexer->lookahead == '\n') {
            return true;
        }
        skip(lexer);
    }
    return false;
}

// Before an annotated accessor, the semi that may come between a property's accessors.
static bool accessor_semi(TSLexer *lexer, bool after_annotation, const bool *valid_symbols) {
    if (!after_annotation || !valid_symbols[ACCESSOR_SEMI]) {
        return false;
    }
    lexer->result_symbol = ACCESSOR_SEMI;
    return true;
}

void *tree_sitter_kotlin_external_scanner_create() { return NULL; }

void tree_sitter_kotlin_external_scanner_destroy(void *payload) {}

unsigned tree_sitter_kotlin_external_scanner_serialize(void *payload, char *buffer) { return 0; }

void tree_sitter_kotlin_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {}

bool tree_sitter_kotlin_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    if (valid_symbols[MULTILINE_STRING_CONTENT]) {
        bool did_advance = false;
        lexer->result_symbol = MULTILINE_STRING_CONTENT;
        while (!lexer->eof(lexer)) {
            switch (lexer->lookahead) {
                case '$':
                    lexer->mark_end(lexer);
                    advance(lexer);
                    if (iswalpha(lexer->lookahead) || lexer->lookahead == '{') {
                        return did_advance;
                    }
                    did_advance = true;
                    break;
                case '"':
                    // The last three quotes of a run end the string; any before them are text
                    // (`"""say "hi"""""` ends with the quote after `hi` as text).
                    if (did_advance) {
                        lexer->mark_end(lexer);
                        advance(lexer);
                        if (lexer->lookahead == '"') {
                            advance(lexer);
                            if (lexer->lookahead == '"') {
                                return true;
                            }
                        }
                        break;
                    }
                    // At the start of the token: one quote of text when at least three follow it.
                    advance(lexer);
                    lexer->mark_end(lexer);
                    did_advance = true;
                    if (lexer->lookahead == '"') {
                        advance(lexer);
                        if (lexer->lookahead == '"') {
                            advance(lexer);
                            return lexer->lookahead == '"';
                        }
                    }
                    break;
                default:
                    advance(lexer);
                    did_advance = true;
                    break;
            }
        }
    }

    if (valid_symbols[SEMI] || valid_symbols[CLASS_MEMBER_SEMI]) {
        lexer->result_symbol = valid_symbols[SEMI] ? SEMI : CLASS_MEMBER_SEMI;
        lexer->mark_end(lexer);
        bool saw_newline = false;
        for (;;) {
            if (lexer->eof(lexer)) {
                return true;
            }

            if (lexer->lookahead == ';') {
                advance(lexer);
                lexer->mark_end(lexer);
                return true;
            }

            if (!iswspace(lexer->lookahead)) {
                break;
            }

            if (lexer->lookahead == '\n') {
                skip(lexer);
                saw_newline = true;
                break;
            }

            if (lexer->lookahead == '\r') {
                skip(lexer);

                if (lexer->lookahead == '\n') {
                    skip(lexer);
                }

                saw_newline = true;
                break;
            }

            skip(lexer);
        }

        // Skip whitespace and comments
        while (iswspace(lexer->lookahead)) {
            skip(lexer);
        }
        if (lexer->lookahead == '/') {
            goto comment;
        }

        if (!saw_newline) {
            switch (lexer->lookahead) {
                case '!':
                    skip(lexer);
                    goto continue_not_is_from_semi;
                case '?':
                    if (valid_symbols[Q_DOT]) {
                        goto q_dot_from_semi;
                    }
                    return false;
                // Inside a class body, a declaration-starting hard keyword right
                // after a complete member terminates it even without a newline:
                // the spec grammar makes `semis` optional after declarations, so
                // `class C { fun a() {} fun b() {} }` is valid Kotlin. All of these
                // are hard keywords and can never continue the preceding member
                // (e.g. as an infix function name). Restricted to CLASS_MEMBER_SEMI:
                // at statement level a soft keyword like `enum` in `enum class`
                // parses as a bare identifier first, and a speculative SEMI before
                // `class` would split the modifier from its declaration. Class
                // members cannot be bare expressions, so that hazard does not exist
                // for CLASS_MEMBER_SEMI.
                case 'c':
                case 'f':
                case 'o':
                case 't':
                case 'v':
                case 'i':
                    if (valid_symbols[CLASS_MEMBER_SEMI] && !valid_symbols[SEMI]) {
                        char decl_word[16] = {0};
                        if (scan_words(lexer,
                                       (const char[16][16]){"class", "fun", "interface", "object",
                                                            "typealias", "val", "var"},
                                       decl_word, NULL)) {
                            // Word boundary: reject identifiers that merely start
                            // with a keyword (`val2`, `fun_x`).
                            return !iswalnum(lexer->lookahead) && lexer->lookahead != '_';
                        }
                        return false;
                    }
                    if (lexer->lookahead == 'i') {
                        return scan_word(lexer, "import");
                    }
                    return false;
                case ';':
                    advance(lexer);
                    lexer->mark_end(lexer);
                    return true;
                // A closing brace terminates the current statement/member even without a
                // newline (`class A { fun m() {} }`). mark_end already sat at the start, so
                // this yields a zero-width semicolon before `}`.
                case '}':
                    return true;
                default:
                    return false;
            }
        }

        char scanned_word[16] = {0};
        // An annotation came first: an accessor after it starts with a semi when one may come
        // between a property's accessors (`get() = x` then `@A set(v)`), or the getter's
        // expression would read the annotation as the start of a trailing lambda.
        bool after_annotation = false;
    _switch:
        switch (lexer->lookahead) {
            case ',':
            case '.':
            case ':':
            case '*':
            case '%':
            case '>':
            case '<':
            case '=':
            case '{':
            case '[':
            case '|':
            case '&':
            case '/':
                return false;
            // Insert a semicolon before `--` and `++`, but not before binary `+` or `-`.
            // Insert before +/-{float}
            case '+':
                skip(lexer);
                if (lexer->lookahead == '+') {
                    return true;
                }
                return iswdigit(lexer->lookahead);
            case '-':
                skip(lexer);
                if (lexer->lookahead == '-') {
                    return true;
                }
                return iswdigit(lexer->lookahead);
            // Don't insert a semicolon before `!=`, but do insert one before a unary `!`.
            case '!':
                skip(lexer);
                if (lexer->lookahead == 'i' && valid_symbols[NOT_IS]) {
                    skip(lexer);
                    if (lexer->lookahead == 's') {
                        skip(lexer);
                        if (!iswalnum(lexer->lookahead)) {
                            return true;
                        }
                    }
                }
                return lexer->lookahead != '=';
            case '?':
                if (valid_symbols[Q_DOT]) {
                    goto q_dot_from_semi;
                }
                return true;
            case 'e':
            case 'i':
            case 'g':
            case 's':
            case 'p':
            case 'a':
            case 'f':
            case 'o':
            case 'l':
            case 'v':
            case 'n':
            case 'c':
            case 'b':
            case 'w':
                while (scan_words(lexer,
                                  (const char[16][16]){"public", "private", "protected", "internal", "abstract",
                                                       "final", "open", "override", "lateinit", "vararg", "noinline",
                                                       "crossinline", "external", "suspend", "inline", "actual"},
                                  scanned_word, NULL)) {
                    memset(scanned_word, 0, MAX_WORD_SIZE);
                    while (iswspace(lexer->lookahead)) {
                        skip(lexer);
                    }
                }

                uint8_t index = -1;
                bool res = scan_words(
                    lexer,
                    (const char[16][16]){"else", "in", "instanceof", "get", "set", "constructor", "by", "as", "where"},
                    scanned_word, &index);

                // If `CLASS_MEMBER_SEMI` is valid, we found a secondary constructor and so we want to insert a semi, OR
                // we found a variable named constructor whose field is being accessed
                if (index == 5) {
                    while (iswspace(lexer->lookahead)) {
                        skip(lexer);
                    }
                    if (lexer->lookahead == '.' || lexer->lookahead == '=') {
                        return true;
                    }
                    // Right after a class header, `constructor` on a later line still belongs to it: Kotlin
                    // allows newlines before the primary constructor (`class B\n  @A\n  constructor(x: X)`).
                    if (valid_symbols[CONSTRUCTOR]) {
                        return false;
                    }
                    if (valid_symbols[CLASS_MEMBER_SEMI]) {
                        return true;
                    }
                }
                // Ordinarily, we should not insert a semicolon if there is an `else` on the next line,
                // except for when it's a 'when entry', which has a `->` after the `else`.
                else if (index == 0) {
                    while (iswspace(lexer->lookahead)) {
                        skip(lexer);
                    }
                    if (lexer->lookahead == '-') {
                        skip(lexer);
                        if (lexer->lookahead == '>') {
                            return true;
                        }
                    }
                }
                // If `get` was found and the keyword is not valid, return a semi since it's being used as an identifier
                // `get` is also an infix function name, so GET is valid after any expression. A getter is
                // possible only where SET is too (after a property) or where no expression continues (after
                // a setter); on a new line `get` is never infix.
                else if (index == 3 && (!valid_symbols[GET] || (!valid_symbols[SET] && valid_symbols[NOT_IS]) ||
                                        lexer->lookahead == '[')) {
                    return true;
                }
                // A getter is `get` alone on its line or `get()`: anything else (`get("/a") {`, `get {`,
                // `get<T>()`, `get_x`) is a call on a new statement.
                else if (index == 3) {
                    while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
                        skip(lexer);
                    }
                    if (lexer->lookahead == '(') {
                        skip(lexer);
                        while (iswspace(lexer->lookahead)) {
                            skip(lexer);
                        }
                        return lexer->lookahead != ')' || accessor_semi(lexer, after_annotation, valid_symbols);
                    }
                    return !(lexer->lookahead == '\n' || lexer->lookahead == '\r' || lexer->lookahead == ';' ||
                             lexer->lookahead == '}' || lexer->lookahead == '/' || lexer->eof(lexer)) ||
                           accessor_semi(lexer, after_annotation, valid_symbols);
                }
                // If `set` was found and the keyword is not valid, return a semi since it's being used as an identifier
                else if (index == 4 && (!valid_symbols[SET] || lexer->lookahead == '[' || lexer->lookahead == '(' ||
                                        lexer->lookahead == '.')) {
                    if (lexer->lookahead == '(' && valid_symbols[SET]) {
                        return set_paren_is_call(lexer) || accessor_semi(lexer, after_annotation, valid_symbols);
                    }
                    return true;
                }
                // A setter is `set` alone on its line or `set(name)`: `set {` is a call on a new statement.
                else if (index == 4) {
                    while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
                        skip(lexer);
                    }
                    if (lexer->lookahead == '(') {
                        return set_paren_is_call(lexer) || accessor_semi(lexer, after_annotation, valid_symbols);
                    }
                    return !(lexer->lookahead == '\n' || lexer->lookahead == '\r' ||
                             lexer->lookahead == ';' || lexer->lookahead == '}' || lexer->lookahead == '/' ||
                             lexer->eof(lexer)) ||
                           accessor_semi(lexer, after_annotation, valid_symbols);
                }
                // If `in` was found and this specific external keyword is valid,
                // return a semi since it's being used in a range test
                else if (index == 1 && valid_symbols[IN]) {
                    return true;
                }
                // `where` continues a declaration only as a type-constraints
                // clause; followed by `?` `.` `=` `(` it is an ordinary
                // identifier (`where?.let { ... }`) — close the statement.
                else if (index == 8) {
                    while (iswspace(lexer->lookahead)) {
                        skip(lexer);
                    }
                    if (lexer->lookahead == '?' || lexer->lookahead == '.' ||
                        lexer->lookahead == '=' || lexer->lookahead == '(') {
                        return true;
                    }
                }
                return !res;
            case ';':
                advance(lexer);
                lexer->mark_end(lexer);
                return true;
            case '@':
                // An annotation may prefix a continuation (accessor, secondary or primary
                // constructor): skip it and re-dispatch on the word that follows, whose own case
                // decides. Bailing to a semi here orphans `@OptIn(...) get()` accessors and
                // `@X internal constructor` headers.
                if (valid_symbols[CONSTRUCTOR] || valid_symbols[GET] || valid_symbols[SET]) {
                    bool saw_paren = false;
                    // Same EOF guard as above: at end of input lookahead is neither '\n' nor
                    // whitespace, so neither arm of this condition can end the loop.
                    while (!lexer->eof(lexer) &&
                           (saw_paren ? lexer->lookahead != '\n' : !iswspace(lexer->lookahead))) {
                        skip(lexer);
                        if (lexer->lookahead == '(') {
                            saw_paren = true;
                        }
                        if (lexer->lookahead == ')') {
                            saw_paren = false;
                        }
                    }
                    while (iswspace(lexer->lookahead)) {
                        skip(lexer);
                    }
                    while (lexer->lookahead == '/') {
                        // Line comments after the annotation (`@Suppress("X") // why`) are skipped
                        // with it; any other `/` ends the statement.
                        skip(lexer);
                        if (lexer->lookahead != '/') {
                            return true;
                        }
                        while (!lexer->eof(lexer) && lexer->lookahead != '\n') {
                            skip(lexer);
                        }
                        while (iswspace(lexer->lookahead)) {
                            skip(lexer);
                        }
                    }
                    after_annotation = true;
                    goto _switch;
                }
                return true;

            default:
                return true;
        }
    }

    while (iswspace(lexer->lookahead)) {
        skip(lexer);
    }

    if (valid_symbols[NOT_IS]) {
        if (lexer->lookahead == '!') {
            advance(lexer);
        continue_not_is_from_semi:
            if (lexer->lookahead == 'i') {
                advance(lexer);
                if (lexer->lookahead == 's') {
                    advance(lexer);
                    lexer->result_symbol = NOT_IS;
                    lexer->mark_end(lexer);
                    return !iswalnum(lexer->lookahead);
                }
            }
        }
    }

    if (valid_symbols[IN]) {
        if (lexer->lookahead == 'i') {
            advance(lexer);
            if (lexer->lookahead == 'n') {
                advance(lexer);
                lexer->result_symbol = IN;
                lexer->mark_end(lexer);
                return !iswalnum(lexer->lookahead);
            }
        }
    }

q_dot_from_semi:
    if (valid_symbols[Q_DOT]) {
        while (iswspace(lexer->lookahead)) {
            skip(lexer);
        }
        if (lexer->lookahead == '?') {
            advance(lexer);
            while (iswspace(lexer->lookahead)) {
                skip(lexer);
            }
            if (lexer->lookahead == '.') {
                advance(lexer);
                lexer->result_symbol = Q_DOT;
                lexer->mark_end(lexer);
                return true;
            }
        }
    }

comment:
    if (valid_symbols[DOLLAR]) {
        return false;
    }

    if (lexer->lookahead == '/') {
        advance(lexer);
        if (lexer->lookahead != '*') {
            return false;
        }
        advance(lexer);

        bool after_star = false;
        unsigned nesting_depth = 1;
        for (;;) {
            switch (lexer->lookahead) {
                case '\0':
                    return false;
                case '*':
                    advance(lexer);
                    after_star = true;
                    break;
                case '/':
                    if (after_star) {
                        advance(lexer);
                        after_star = false;
                        nesting_depth--;
                        if (nesting_depth == 0) {
                            lexer->result_symbol = BLOCK_COMMENT;
                            lexer->mark_end(lexer);
                            return true;
                        }
                    } else {
                        advance(lexer);
                        after_star = false;
                        if (lexer->lookahead == '*') {
                            nesting_depth++;
                            advance(lexer);
                        }
                    }
                    break;
                default:
                    advance(lexer);
                    after_star = false;
                    break;
            }
        }
    }

    return false;
}
