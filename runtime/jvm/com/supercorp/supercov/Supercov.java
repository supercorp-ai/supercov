package com.supercorp.supercov;

import java.io.BufferedOutputStream;
import java.io.DataOutputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.ArrayList;
import java.util.List;

/**
 * The runtime half of Supercov's JVM frontend, shared by Java and Kotlin.
 *
 * <p>A probe is a store into {@link #HITS}, a public static array the
 * instrumented class already holds a reference to. That is the shape JaCoCo
 * settled on — it caches a {@code boolean[]} per class and writes
 * {@code probes[i] = true} — and the shape Go's own cover tool uses, for the
 * same reason: the expensive part of a probe is finding somewhere to write,
 * not the write.
 *
 * <p>Everything that cannot be a store happens at a test boundary, where it is
 * paid once per test rather than once per statement.
 */
public final class Supercov {

  private Supercov() {}

  /** Marks a branch whose condition is a single operand and needs no independence obligation. */
  public static final int NO_DECISION = -1;

  /**
   * Per-probe bitmasks: bit 0 for false, bit 1 for true. A point or a branch
   * alternative only ever sets bit 1.
   */
  public static int[] HITS = new int[0];

  private static int[] global = new int[0];
  private static long[] evaluating = new long[0];
  private static long[] truth = new long[0];
  private static int[] widths = new int[0];
  // The keys most recently recorded *for the running test*, so a decision
  // evaluated in a loop can answer "seen this already" without taking the
  // monitor. Two entries because the common shape is a condition alternating
  // between outcomes; a third distinct vector simply pays for the call.
  // Cleared by harvest alongside the vectors they stand in for — a recorded
  // key always has a condition bit set, so zero is free to mean empty.
  private static long[] recentFirst = new long[0];
  private static long[] recentSecond = new long[0];
  private static List<long[]> seen = new ArrayList<>();
  private static final List<Record> records = new ArrayList<>();
  private static int current = -1;

  private static final int VALUE_SHIFT = 24;
  private static final int OUTCOME_SHIFT = 48;
  private static final int MAX_WIDTH = 24;

  private static final class Record {
    final String name;
    final List<int[]> probes = new ArrayList<>();
    final List<long[]> vectors = new ArrayList<>();

    Record(String name) {
      this.name = name;
    }
  }

  /** Prepares the runtime for a run of {@code probes} probes. */
  public static synchronized void arm(int probes, int[] conditions) {
    HITS = new int[probes];
    global = new int[probes];
    widths = conditions;
    evaluating = new long[conditions.length];
    truth = new long[conditions.length];
    recentFirst = new long[conditions.length];
    recentSecond = new long[conditions.length];
    seen = new ArrayList<>();
    for (int i = 0; i < conditions.length; i++) {
      seen.add(new long[0]);
    }
    records.clear();
    current = -1;
  }

  /**
   * Binds every probe that fires next to this test.
   *
   * <p>Attribution is a harvest at the boundary rather than a lookup on every
   * probe: whatever the array holds now belongs to whoever was running, so it
   * is swept into their record and cleared.
   */
  public static synchronized void enterTest(String name) {
    harvest();
    records.add(new Record(name));
    current = records.size() - 1;
  }

  /** Ends the running test, harvesting what it reached. */
  public static synchronized void exitTest() {
    harvest();
    current = -1;
  }

  private static void harvest() {
    Record into = current >= 0 && current < records.size() ? records.get(current) : null;
    for (int index = 0; index < HITS.length; index++) {
      int value = HITS[index];
      if (value == 0) {
        continue;
      }
      global[index] |= value;
      if (into != null) {
        into.probes.add(new int[] {index, value});
      }
      HITS[index] = 0;
    }
    for (int id = 0; id < seen.size(); id++) {
      long[] keys = seen.get(id);
      if (keys.length > 0 && into != null) {
        for (long key : keys) {
          into.vectors.add(new long[] {id, key});
        }
      }
      if (keys.length > 0) {
        seen.set(id, new long[0]);
      }
      recentFirst[id] = 0L;
      recentSecond[id] = 0L;
    }
  }

  /**
   * Observes one condition of a decision and returns it unchanged, so wrapping
   * an operand cannot change what the expression evaluates to. Java evaluates
   * an argument only when the call is reached, which is what keeps {@code &&}
   * and {@code ||} short-circuiting through the wrapper.
   */
  public static boolean c(int id, int index, boolean value) {
    if (id < 0 || id >= evaluating.length || index >= 64) {
      return value;
    }
    long bit = 1L << index;
    if (index == 0) {
      evaluating[id] = bit;
      truth[id] = 0L;
    } else {
      evaluating[id] |= bit;
    }
    if (value) {
      truth[id] |= bit;
    }
    return value;
  }

  /**
   * Records which arm of a branch a condition selects. The form for a branch
   * whose condition is a single operand: no decision to close.
   */
  public static boolean b(int whenTrue, int whenFalse, boolean value) {
    int probe = value ? whenTrue : whenFalse;
    if (probe >= 0 && probe < HITS.length) {
      HITS[probe] = 2;
    }
    return value;
  }

  /**
   * {@code b} for a branch whose condition has several operands: it also closes
   * the decision those operands opened.
   *
   * <p>Every decision Supercov records sits in a branch, so its outcome is the
   * value this wrapper already holds. A separate outcome wrapper would observe
   * the same fact a second time.
   */
  public static boolean bd(int whenTrue, int whenFalse, int id, boolean value) {
    int probe = value ? whenTrue : whenFalse;
    if (probe >= 0 && probe < HITS.length) {
      HITS[probe] = 2;
    }
    if (id < 0 || id >= evaluating.length) {
      return value;
    }
    long key = evaluating[id] | (truth[id] << VALUE_SHIFT);
    if (value) {
      key |= 1L << OUTCOME_SHIFT;
    }
    evaluating[id] = 0L;
    long mask = key & ((1L << VALUE_SHIFT) - 1);
    if (mask == 0L || widths[id] > MAX_WIDTH) {
      return value;
    }
    // Everything above is a handful of array accesses; `remember` takes a
    // monitor. A decision inside a loop almost always produces the vector it
    // produced last time, so answering from the cache keeps the lock off the
    // path that runs millions of times and leaves it for the rare new vector.
    if (key == recentFirst[id] || key == recentSecond[id]) {
      return value;
    }
    remember(id, key);
    return value;
  }

  private static synchronized void remember(int id, long key) {
    recentSecond[id] = recentFirst[id];
    recentFirst[id] = key;
    long[] keys = seen.get(id);
    for (long existing : keys) {
      if (existing == key) {
        return;
      }
    }
    long[] grown = new long[keys.length + 1];
    System.arraycopy(keys, 0, grown, 0, keys.length);
    grown[keys.length] = key;
    seen.set(id, grown);
  }

  /** Writes the evidence transport the engine reads. */
  public static synchronized void write(String path) throws IOException {
    harvest();
    try (DataOutputStream out =
        new DataOutputStream(new BufferedOutputStream(new FileOutputStream(path), 1 << 20))) {
      putLong(out, global.length);
      for (int value : global) {
        putLong(out, value);
      }
      putLong(out, records.size());
      for (Record record : records) {
        byte[] name = record.name.getBytes("UTF-8");
        putLong(out, name.length);
        out.write(name);
        putLong(out, record.probes.size());
        for (int[] hit : record.probes) {
          putLong(out, hit[0]);
          putLong(out, hit[1]);
        }
        putLong(out, record.vectors.size());
        for (long[] hit : record.vectors) {
          putLong(out, hit[0]);
          putLong(out, hit[1]);
        }
      }
      putLong(out, widths.length);
      for (int id = 0; id < widths.length; id++) {
        putLong(out, widths[id]);
        List<Long> union = new ArrayList<>();
        for (Record record : records) {
          for (long[] hit : record.vectors) {
            if (hit[0] != id) {
              continue;
            }
            if (!union.contains(hit[1])) {
              union.add(hit[1]);
            }
          }
        }
        putLong(out, union.size());
        for (long key : union) {
          putLong(out, key);
        }
      }
    }
  }

  /** Little-endian, matching every other Supercov transport. */
  private static void putLong(DataOutputStream out, long value) throws IOException {
    for (int shift = 0; shift < 64; shift += 8) {
      out.write((int) ((value >>> shift) & 0xFF));
    }
  }
}
