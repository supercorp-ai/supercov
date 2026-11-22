package fixture

const val LIMIT = 10

class Ops(val name: String, private var count: Int = 0) {
    var total: Long = 0
    private val log = mutableListOf<String>()

    fun lt(a: Int, b: Int): Boolean = a < b
    fun le(a: Int, b: Int) = a <= b
    fun gtL(a: Long, b: Long): Boolean { return a > b }
    fun geD(a: Double, b: Double): Boolean = a >= b
    fun eq(a: Int, b: Int) = a == b
    fun eqObj(a: String, b: String) = a == b
    fun ident(a: Any, b: Any) = a === b
    fun ne(a: String?, b: String?) = a != b
    fun isNull(a: Any?) = a == null
    fun flag(f: Boolean) = f
    fun notFlag(f: Boolean) = !f
    fun and(a: Boolean, b: Boolean) = a && b
    fun or(a: Boolean, b: Boolean) = a || b
    fun inst(o: Any) = o is String
    fun cmp(a: String, b: String) = a < b

    fun branch(a0: Int, b0: Int): Int {
        var a = a0
        var b = b0
        if (a < b) {
            return a
        }
        if (a == 3 && b > 2 || a != b) {
            return b
        }
        while (a > 0) {
            a--
        }
        for (i in 0 until b) {
            a += i
        }
        for (i in 0..b) {
            a -= i
        }
        do {
            b--
        } while (b > 10)
        return if (a > b) a else b
    }

    fun math(a: Int, b: Int): Int {
        var c = a + b
        c = c - a
        c = c * b
        c = c / (if (b == 0) 1 else b)
        c = c % 7
        c = c and a
        c = c or b
        c = c xor a
        c = c shl 1
        c = c shr 1
        c = c ushr 1
        c += a
        c -= 2
        c *= 3
        return c
    }

    fun lmath(a: Long, b: Long): Long = a * b + a - b / 2
    fun mixed(a: Int, b: Long): Long = a + b
    fun dmath(a: Double, b: Float): Double = a * b + a - b / 2 % 3
    fun concat(a: String, b: Int): String = a + b + "x"
    fun template(a: String, b: Int) = "$a:${b + 1}"
    fun neg(a: Int) = -a
    fun dneg(a: Double): Double = -a

    fun increments(n: Int): Int {
        var i = 0
        i++
        i--
        ++i
        i += 5
        i -= n
        count++
        total++
        var l = n.toLong()
        l++
        return i + count + l.toInt()
    }

    fun voids(s: String) {
        log.add(s)
        log.clear()
        println(s)
        helper()
        this.helper()
        Ops("x").helper()
    }

    private fun helper() {
        count = 1
    }

    fun str(s: String): String = s.trim()
    fun strN(s: String?): String? = s?.trim()
    fun boxedInt(a: Int): Int? = a
    fun obj(o: Any): Any = o
    fun objN(o: Any?): Any? = o
    fun list(): List<String> = log
    fun mlist(): MutableList<String> = log
    fun set(): Set<String> = emptySet()
    fun map(): Map<String, String> = mapOf()
    fun ch(): Char = 'a'
    fun fl(): Float = 1f
    fun zeroRet(): Int = 0
    fun t(): Boolean = true
    fun inferred(a: Int) = a * 2
    fun unit() { count = 2 }

    fun whenSubject(x: Int): Int = when (x) {
        1 -> 10
        2, 3 -> 20
        in 4..9 -> 30
        else -> x * 3
    }

    fun whenBare(x: Int): String = when {
        x < 0 -> "neg"
        x == 0 -> "zero"
        else -> "pos"
    }

    fun safe(s: String?): Int = s?.length ?: -1
    fun bang(s: String?): Int = s!!.length
    fun smart(o: Any): Int = if (o is String) o.length else 0
    fun nullCheck(s: String?): Int {
        if (s == null) return 0
        return s.length
    }

    fun lambda(xs: List<Int>): Int = xs.filter { it > 2 }.map { it * 2 }.sum()
    fun forEach(xs: List<Int>): Int {
        var sum = 0
        for (x in xs) {
            sum += x
        }
        xs.forEach { sum += it }
        return sum
    }

    fun withDefault(a: Int, b: Int = 3): Int = a + b
}

object Single {
    val start = compute()
    fun compute(): Int = LIMIT * 2 + 1
    fun go(x: Int) = x > LIMIT
}

data class Point(val x: Int, val y: Int) {
    fun dist() = x * x + y * y
}

fun topLevel(a: Int): Int = a - 1
