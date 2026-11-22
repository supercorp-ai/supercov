package fixture;

import java.util.ArrayList;
import java.util.Collection;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.function.IntUnaryOperator;
import java.util.stream.Stream;

public class Ops {
    static final int CONST = 4;
    static final boolean DEBUG = false;
    private int count;
    private long total;
    private Integer boxed = 0;
    private final List<String> log = new ArrayList<>();

    public static boolean lt(int a, int b) { return a < b; }
    public static boolean le(int a, int b) { return a <= b; }
    public static boolean gt(long a, long b) { return a > b; }
    public static boolean ge(double a, double b) { return a >= b; }
    public static boolean eq(int a, int b) { return a == b; }
    public static boolean ne(Object a, Object b) { return a != b; }
    public static boolean isNull(Object a) { return a == null; }
    public static boolean zero(int a) { return a > 0; }
    public static boolean flag(boolean f) { return f; }
    public static boolean notFlag(boolean f) { return !f; }
    public static boolean and(boolean a, boolean b) { return a && b; }
    public static boolean or(boolean a, boolean b) { return a || b; }
    public static boolean band(boolean a, boolean b) { return a & b; }
    public static boolean inst(Object o) { return o instanceof String; }

    public static int branch(int a, int b) {
        if (a < b) {
            return a;
        }
        if (a == 3 && b > 2 || a != b) {
            return b;
        }
        while (a > 0) {
            a--;
        }
        for (int i = 0; i < b; i++) {
            a += i;
        }
        do {
            b--;
        } while (b > 10);
        return a > b ? a : b;
    }

    public static int math(int a, int b) {
        int c = a + b;
        c = c - a;
        c = c * b;
        c = c / (b == 0 ? 1 : b);
        c = c % 7;
        c = c & a;
        c = c | b;
        c = c ^ a;
        c = c << 1;
        c = c >> 1;
        c = c >>> 1;
        c += a;
        c -= 2;
        c *= 3;
        return c;
    }

    public static long lmath(long a, long b) { return a * b + a - b / (b == 0 ? 1 : b); }
    public static double dmath(double a, float b) { return a * b + a - b / 2 % 3; }
    public static String concat(String a, int b) { return a + b + "x"; }
    public static int folded() { return CONST * 2 + 60 * 60; }
    public static int neg(int a) { return -a; }
    public static double dneg(double a) { return -a; }

    public int increments(int n) {
        int i = 0;
        i++;
        i--;
        ++i;
        i += 5;
        i -= n;
        count++;
        total++;
        boxed++;
        long l = n;
        l++;
        byte by = 1;
        by++;
        int[] arr = new int[3];
        arr[0]++;
        arr[i++ % 3] = 1;
        return i + count + by + arr[0] + (int) l;
    }

    public void voids(String s) {
        log.add(s);
        log.clear();
        System.out.println(s);
        helper();
        this.helper();
        new Ops().helper();
        Collections.sort(log);
        if (DEBUG) {
            helper();
        }
        assert s != null;
    }

    private void helper() {
        count = 1;
    }

    public String str(String s) { return s.trim(); }
    public Integer boxedInt(int a) { return a; }
    public Long boxedLong(long a) { return a; }
    public Boolean boxedBool(boolean a) { return a; }
    public Character boxedChar(char a) { return a; }
    public Object obj(Object o) { return o; }
    public List<String> list() { return log; }
    public Set<String> set() { return null; }
    public Map<String, String> map() { return null; }
    public Collection<String> coll() { return log; }
    public Optional<String> opt() { return Optional.of("a"); }
    public Stream<String> stream() { return log.stream(); }
    public Iterable<String> iterable() { return log; }
    public int[] array() { return new int[0]; }
    public char ch() { return 'a'; }
    public byte by() { return 1; }
    public short sh() { return 1; }
    public float fl() { return 1f; }
    public double db() { return 1.0; }
    public long lg() { return 0L; }
    public int zeroRet() { return 0; }
    public boolean t() { return true; }
    public boolean f() { return false; }

    public int sw(String s) {
        switch (s) {
            case "a": return 1;
            case "b": return 2;
            default: return 3;
        }
    }

    public int foreach(List<Integer> xs) {
        int sum = 0;
        for (int x : xs) {
            sum += x;
        }
        for (int i = 0; i < xs.size(); i++) {
            sum = sum + xs.get(i);
        }
        return sum;
    }

    public int lambda(List<Integer> xs) {
        IntUnaryOperator op = x -> x + 1;
        return xs.stream().filter(x -> x > 2).mapToInt(x -> x * 2).sum() + op.applyAsInt(1);
    }

    public int tryFinally(int a) {
        try {
            if (a > 1) {
                return a + 1;
            }
            return a - 1;
        } finally {
            count = count + 1;
        }
    }

    public int multiLine(int a,
                         int b) {
        return a
            + b
            * a;
    }

    public int ternaryChain(int a) {
        return a > 10 ? 1 : a > 5 ? 2 : a < -3 ? 3 : 4;
    }

    public static int sameLine(int a, int b) { return (a + b) + (a + b) + (a - b); }

    public boolean cond(int a) {
        boolean b = a > 3;
        boolean c = !b;
        boolean d = b == c;
        return d;
    }

    public long ternaryMixed(int a, long b) { return a < b ? a : b; }
    public boolean chars(char c) { return c >= 'a' && c <= 'z'; }
    public boolean nan(double d) { return d != d; }
    public int compareObj(String a, String b) { return a.compareTo(b) < 0 ? -1 : 1; }

    public enum Color { RED, GREEN }
    public int esw(Color c) {
        switch (c) {
            case RED: return 1;
            default: return 2;
        }
    }

    public int newSwitch(int x) {
        return switch (x) {
            case 1 -> 10;
            case 2 -> 20;
            default -> x * 3;
        };
    }

    public void throwing(int a) {
        if (a < 0) {
            throw new IllegalArgumentException("neg " + a);
        }
    }

    public Ops() {
        super();
        helper();
    }

    static {
        System.setProperty("x", "y");
    }

    public static int staticMath = CONST + 1;
}
