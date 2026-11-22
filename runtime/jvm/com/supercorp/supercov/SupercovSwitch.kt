@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
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
 */
fun supercovNegate(file: Int, index: Int, value: Boolean): Boolean {
    contract {
        returns(true) implies (value)
        returns(false) implies (!value)
    }
    return if (Supercov.on(file, index)) !value else value
}
