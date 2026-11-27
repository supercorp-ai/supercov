package fixture;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.function.Function;
import java.util.function.Predicate;
import java.util.function.Supplier;
import java.util.logging.Logger;

public class More {
    private static final Logger LOG = Logger.getLogger("more");
    private static final int SIZE = compute(3);
    private static final List<String> NAMES = new ArrayList<>();
    private int field = 2 + width();
    private final Supplier<Integer> lazy = () -> field + 1;

    static {
        NAMES.add("a");
        setup();
    }

    {
        field = field * 2;
    }

    private static int compute(int x) {
        return x * 2 + helperStatic(x);
    }

    private static int helperStatic(int x) {
        return x - 1;
    }

    private static void setup() {
        System.out.println("setup" + SIZE);
    }

    private int width() {
        return field + 3;
    }

    public int iincLike(int x) {
        x = x + 1;
        x = x - 2;
        int y = x;
        y = 5 + y;
        char c = 'a';
        c += 1;
        c++;
        short s = 1;
        s += 2;
        String str = "a";
        str += x;
        Integer boxed = 3;
        boxed += 2;
        return y + c + s + str.length() + boxed;
    }

    public void logs(int x) {
        LOG.info("x is " + (x + 1));
        if (x > 2) {
            LOG.fine(() -> "big " + x);
        }
    }

    public <T> T generic(T t) { return t; }
    public Integer zeroBoxed() { return 0; }
    public String emptyStr() { return ""; }
    public List<String> emptyList() { return Collections.emptyList(); }
    public List<String> listOf() { return List.of(); }
    public Map<String, Integer> mapOf() { return Map.of("a", 1); }
    public Boolean boxedTrue() { return true; }
    public Boolean boxedFalse() { return Boolean.FALSE; }
    public boolean ternaryBool(int a) { return a > 2 ? true : false; }
    public int ternaryZero(int a) { return a > 2 ? 1 : 0; }
    public double dzero() { return 0.0; }
    public long lfolded() { return 3 - 3; }
    public int divMinusOne(int a) { return a / -1; }
    public int[] arr() { return null; }

    public boolean conditionTernary(boolean a, boolean b, boolean c) {
        if (a ? b : c) {
            return true;
        }
        return false;
    }

    public int loops(int[] xs) {
        int total = 0;
        for (int i = 0; i < xs.length; i++) {
            total += xs[i];
        }
        for (int i = xs.length - 1; i >= 0; i--) {
            total -= xs[i];
        }
        for (int i = 0, j = 10; i < j; i++, j--) {
            total++;
        }
        int k = 0;
        while (k < 10) {
            total += k;
            k += 2;
        }
        int n = 10;
        while (n > k) {
            n--;
        }
        return total;
    }

    public List<Integer> lambdas(List<Integer> xs) {
        Function<Integer, Integer> doubler = x -> x * 2;
        Predicate<Integer> big = x -> x > 10;
        xs.forEach(x -> System.out.println(x));
        xs.sort((a, b) -> a - b);
        return xs.stream().map(x -> x + 1).filter(big).map(doubler).toList();
    }

    interface Shape {
        int area();
        default int twice() { return area() * 2; }
    }

    enum Op {
        PLUS(1) {
            int apply(int a, int b) { return a + b; }
        },
        MINUS(2) {
            int apply(int a, int b) { return a - b; }
        };
        private final int code;
        Op(int code) { this.code = code + 0; }
        abstract int apply(int a, int b);
        int code() { return code * 1; }
    }

    static class Inner {
        int value(int a) { return a % 3; }
    }

    public Runnable anon(int a) {
        return new Runnable() {
            public void run() {
                System.out.println(a + 1);
            }
        };
    }

    public void calls(List<String> xs, StringBuilder sb) {
        xs.add("x");
        xs.add(0, "y");
        xs.remove("x");
        sb.append("a");
        sb.setLength(0);
        xs.set(0, "z");
        Collections.reverse(xs);
        System.arraycopy(new int[1], 0, new int[1], 0, 1);
        width();
        unknownVoidish(xs);
    }

    private static void unknownVoidish(List<String> xs) {}

    public boolean equals(Object o) {
        if (this == o) return true;
        if (o == null || getClass() != o.getClass()) return false;
        return field == ((More) o).field;
    }

    public int hashCode() { return field; }

    public int tryRes(java.io.InputStream in) throws java.io.IOException {
        try (in) {
            return in.read() + 1;
        }
    }

    public int sw(int x) {
        switch (x) {
            case 1:
                x += 3;
                break;
            default:
                x--;
        }
        return x;
    }

    public long shifts(long a, int b) { return (a << b) + (a >>> 2) + (b >> 1); }
    public int charMath(char c) { return c - 'a' + 1; }
    public float fmath(float a) { return a * 2.5f; }
    public boolean nested(int a, int b) { return !(a > b && b > 0) || a == 3; }
}
