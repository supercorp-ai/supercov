@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
// Kotlin before 1.7 honours @OptIn only under -opt-in flags a build may not
// pass; the use is suppressed by name there, and opted in everywhere else.
@file:Suppress(
    "EXPERIMENTAL_API_USAGE_ERROR",
    "EXPERIMENTAL_IS_NOT_ENABLED",
    "OPT_IN_IS_NOT_ENABLED",
    "EXPERIMENTAL_ANNOTATION_ON_WRONG_TARGET",
)
@file:JvmName("SupercovSwitch")

package com.supercorp.supercov

import kotlin.contracts.contract

/**
 * A Kotlin condition, negated while its mutant is on (`jvm_schemata.rs`).
 *
 * The contract tells the compiler what the condition as written tells it,
 * so `if (x != null) x.length` still compiles with the switch around
 * `x != null`. With the mutant on it is broken, as PIT's negated jump breaks
 * it: the branch runs with `x` null, and fails where the code relies on it.
 *
 * Public in so many words: a module in explicit API mode compiles it too.
 */
public fun supercovNegate(file: Int, index: Int, value: Boolean): Boolean {
    contract {
        returns(true) implies (value)
        returns(false) implies (!value)
    }
    return if (Supercov.on(file, index)) !value else value
}
