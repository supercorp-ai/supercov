package com.supercorp.supercov;

import java.io.BufferedOutputStream;
import java.io.DataOutputStream;
import java.io.File;
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
   * How many probes the instrumented sources hold, substituted by Supercov
   * when it writes this file into a workspace.
   *
   * <p>The array has to be the right size before a single line of product code
   * runs, because a probe is a bare store and nothing checks its bounds -- that
   * is what makes it cost one instruction. If arming were the only thing that
   * sized it, any run where the listener did not start would not merely lose
   * coverage: the first instrumented line would throw, and Supercov would have
   * turned a passing suite into a failing one.
   */
  static final int PROBE_COUNT = 0; // supercov:probe-count

  /**
   * Conditions per decision, for a server a browser suite starts: it has no
   * listener to arm it with them. Substituted with {@link #PROBE_COUNT}.
   */
  static final int[] SERVER_WIDTHS = new int[0]; // supercov:server-widths

  /**
   * Per-probe bitmasks: bit 0 for false, bit 1 for true. A point or a branch
   * alternative only ever sets bit 1.
   */
  public static int[] HITS = new int[PROBE_COUNT];

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
  private static List<long[]> runWide = new ArrayList<>();
  private static final List<Record> records = new ArrayList<>();
  private static int current = -1;
  /// The class-level work open around the running test, innermost last: a
  /// class's `@BeforeAll`, its parameter sources, a nested class's own.
  /// What runs while no test does is credited to the innermost.
  private static final List<Integer> setups = new ArrayList<>();
  /// Each class's setup record, by its name: a class opened again (a
  /// nested class's parent, a TestNG class its tests return to) adds to it.
  private static final java.util.Map<String, Integer> setupRecords = new java.util.HashMap<>();
  private static int open = 0;
  private static boolean overlapped = false;
  private static boolean armed = false;

  private static final int VALUE_SHIFT = 24;
  private static final int OUTCOME_SHIFT = 48;
  private static final int MAX_WIDTH = 24;

  private static final class Record {
    final String name;
    /// Which framework announced this test. A project can run JUnit and
    /// TestNG in one JVM, and a report that named the wrong one would say the
    /// test was attributed by a lifecycle that never saw it.
    String runner = "";
    /// The tags the framework gives the test: a JUnit `@Tag`, a TestNG group.
    String[] tags = new String[0];
    // How the test ended, as the framework saw it. Defaulted rather than
    // required: a test that reports nothing finished normally, and the
    // runtime should not need the framework's cooperation to say so.
    String status = "passed";
    /// Whether what this test reached could be credited to it. False for every
    /// record once tests have been seen running at once: the test ran and has
    /// an outcome, and its coverage is in the run-wide totals rather than its
    /// own. Leaving such a test out entirely reads downstream as a test that
    /// never ran, and a suite of nothing but concurrent tests then published
    /// no tests at all.
    boolean unattributed = false;
    /// Whether this is a test class's own work (`@BeforeAll`, `@AfterAll`,
    /// a parameter source, a static initializer) rather than a test's: every
    /// test of the class runs after it, so every one is credited with it.
    boolean setup = false;
    final List<int[]> probes = new ArrayList<>();
    final List<long[]> vectors = new ArrayList<>();

    Record(String name) {
      this.name = name;
    }
  }

  /** Prepares the runtime for a run of {@code probes} probes. */
  public static synchronized void arm(int probes, int[] conditions) {
    // A project running both JUnit and TestNG installs two listeners in one
    // JVM, and each arms the runtime at its own start. Re-arming would throw
    // away everything the framework that went first had recorded, so a second
    // call describing the same run is left alone.
    if (armed && HITS.length == probes && widths.length == conditions.length) {
      return;
    }
    // Already the right size from PROBE_COUNT in the ordinary case; this is
    // what makes a differently-sized run, or a second one, start clean.
    armed = true;
    HITS = new int[probes];
    global = new int[probes];
    widths = conditions;
    evaluating = new long[conditions.length];
    truth = new long[conditions.length];
    recentFirst = new long[conditions.length];
    recentSecond = new long[conditions.length];
    seen = new ArrayList<>();
    runWide = new ArrayList<>();
    for (int i = 0; i < conditions.length; i++) {
      seen.add(new long[0]);
      runWide.add(new long[0]);
    }
    records.clear();
    outsideRecord = null;
    setups.clear();
    setupRecords.clear();
    current = -1;
    open = 0;
    overlapped = false;
  }

  /**
   * Binds every probe that fires next to this test.
   *
   * <p>Attribution is a harvest at the boundary rather than a lookup on every
   * probe: whatever the array holds now belongs to whoever was running, so it
   * is swept into their record and cleared.
   */
  public static synchronized void enterTest(String name) {
    enterTest(name, "");
  }

  /** Binds every probe that fires next to this test of this runner. */
  public static synchronized void enterTest(String name, String runner) {
    enterTest(name, runner, new String[0]);
  }

  /** Binds every probe that fires next to this test, which carries these tags. */
  public static synchronized void enterTest(String name, String runner, String[] tags) {
    harvest();
    claimOutside(runner);
    if (open > 0) {
      overlap();
    }
    open++;
    Record record = new Record(name);
    record.runner = runner;
    record.tags = tags == null ? new String[0] : tags;
    if (overlapped) {
      // Named and credited with nothing. `current` is left alone so no probe
      // is attributed to it, and the thread remembers which record is its own
      // so the outcome can still be written when the test ends -- tests that
      // overlap do not finish in the order they started.
      record.unattributed = true;
      records.add(record);
      concurrentRecord.set(records.size() - 1);
      return;
    }
    records.add(record);
    current = records.size() - 1;
  }

  /**
   * Records how the running test stands so far, without ending it: a JVM that
   * may be killed outright writes its evidence with the test as not passing,
   * and the test's own end then records how it really ended.
   */
  public static synchronized void setTestStatus(String status) {
    if (current >= 0 && current < records.size()) {
      records.get(current).status = status;
    }
  }

  /** Ends the running test, harvesting what it reached. */
  public static synchronized void exitTest() {
    exitTest("passed");
  }

  /** Ends the running test, recording how the framework says it ended. */
  public static synchronized void exitTest(String status) {
    if (overlapped) {
      Integer mine = concurrentRecord.get();
      if (mine != null && mine >= 0 && mine < records.size()) {
        records.get(mine).status = status;
      }
      concurrentRecord.remove();
    } else if (current >= 0 && current < records.size()) {
      records.get(current).status = status;
    }
    harvest();
    open = Math.max(0, open - 1);
    current = overlapped || setups.isEmpty() ? -1 : setups.get(setups.size() - 1);
  }

  /** The status a class's setup record is written with: it is no test. */
  static final String SETUP = "setup:";

  /**
   * Opens a test class's own work: what runs from here until a test starts,
   * and again between its tests and after the last, until {@link
   * #exitSetup}, is the class's (`@BeforeAll`, a `@MethodSource`, a static
   * initializer, `@AfterAll`). The engine credits it to every test of the
   * class's file: each of them runs after it, and a mutant there changes
   * what they see.
   */
  public static synchronized void enterSetup(String name, String runner) {
    harvest();
    claimOutside(runner);
    if (overlapped) {
      return;
    }
    String recordName = name + "#" + SETUP_NAME;
    Integer at = setupRecords.get(recordName);
    if (at == null) {
      Record record = new Record(recordName);
      record.runner = runner;
      record.setup = true;
      records.add(record);
      at = records.size() - 1;
      setupRecords.put(recordName, at);
    }
    setups.add(at);
    // Inside a running test (an engine that reports a class within one) the
    // test keeps what it reaches; the class takes over once it ends.
    if (open == 0) {
      current = at;
    }
  }

  /**
   * Passes from one test class to the next where the framework reports no
   * class boundary (TestNG's tests alone): what ran since the previous
   * class's last test is its `@AfterClass` or the next one's `@BeforeClass`,
   * and no telling which, so both classes are credited with it.
   */
  public static synchronized void switchSetup(String name, String runner) {
    if (overlapped || open > 0 || current < 0 || !records.get(current).setup) {
      exitSetup("passed");
      enterSetup(name, runner);
      return;
    }
    Record previous = records.get(current);
    int probes = previous.probes.size();
    int vectors = previous.vectors.size();
    harvest();
    List<int[]> between = new ArrayList<>(previous.probes.subList(probes, previous.probes.size()));
    List<long[]> evaluated =
        new ArrayList<>(previous.vectors.subList(vectors, previous.vectors.size()));
    exitSetup("passed");
    enterSetup(name, runner);
    Record next = records.get(current);
    next.probes.addAll(between);
    next.vectors.addAll(evaluated);
  }

  /// What ran while no test and no test class did: a suite's or an
  /// engine's own setup (`@BeforeSuite`), a class loaded between two others.
  /// Any test may depend on it, so it names no class.
  private static Record outside() {
    if (outsideRecord == null) {
      outsideRecord = new Record(OUTSIDE);
      outsideRecord.setup = true;
      records.add(outsideRecord);
    }
    return outsideRecord;
  }

  private static Record outsideRecord;

  /// The framework whose tests ran around what ran outside them.
  private static void claimOutside(String runner) {
    if (outsideRecord != null && outsideRecord.runner.isEmpty() && runner != null) {
      outsideRecord.runner = runner;
    }
  }

  static final String OUTSIDE = "#<outside every test>";

  /// What a setup record's name ends in, after its class's.
  static final String SETUP_NAME = "<class setup>";

  /** Closes the innermost class's own work, as the framework says it ended. */
  public static synchronized void exitSetup(String status) {
    harvest();
    if (overlapped || setups.isEmpty()) {
      return;
    }
    Record record = records.get(setups.remove(setups.size() - 1));
    if (!"passed".equals(status)) {
      record.status = "failed";
    }
    if (open == 0) {
      current = setups.isEmpty() ? -1 : setups.get(setups.size() - 1);
    }
  }

  /// Which record belongs to the test this thread is running, once records
  /// stopped being attributable. Tests that overlap do not end in the order
  /// they began, so the outcome cannot be written to whichever was last.
  private static final ThreadLocal<Integer> concurrentRecord = new ThreadLocal<>();

  /**
   * Gives up on per-test attribution, once, for the whole run.
   *
   * <p>Probes are a store into one shared array precisely so they cost a
   * single instruction, and attribution is a sweep of that array at each test
   * boundary. Two tests running at once share the array, so the sweep credits
   * whatever it finds to whoever happens to be current — every record from
   * then on is a guess, and records already closed may have had their hits
   * swept into a neighbour. Which ones are wrong is not knowable from here.
   *
   * <p>Run-wide totals are unaffected: they are a union, and a union does not
   * care which test contributed what. So the run still measures what the suite
   * reaches; it just cannot say which test reached it, which is the truth
   * rather than a guess dressed as a measurement.
   *
   * <p>Supercov disables parallel execution in the workspace it generates, so
   * reaching this means something else turned it back on.
   */
  private static void overlap() {
    if (overlapped) {
      return;
    }
    overlapped = true;
    // What was recorded before the overlap was noticed may have had a
    // neighbour's hits swept into it, so no record keeps its probes. They keep
    // their names and outcomes: those tests ran, and saying otherwise is how a
    // concurrent suite came to publish nothing at all.
    for (Record earlier : records) {
      earlier.unattributed = true;
      earlier.probes.clear();
      earlier.vectors.clear();
    }
    current = -1;
    // Condition state is read-modify-write, so concurrent evaluations lose
    // updates and the vectors they produce describe an evaluation that never
    // happened. Unlike the probe array — where every write stores the same
    // constant and concurrency cannot change the result — these cannot be
    // salvaged, so the run keeps statements and branches and drops MC/DC.
    runWide = new ArrayList<>();
    for (int id = 0; id < widths.length; id++) {
      runWide.add(new long[0]);
    }
    System.err.println(
        "supercov: tests ran concurrently. Statement and branch coverage are still recorded"
            + " run-wide, but they cannot be attributed to individual tests, and condition"
            + " coverage is dropped because concurrent evaluations corrupt it. For per-test and"
            + " condition coverage, run the suite sequentially"
            + " (JUnit: junit.jupiter.execution.parallel.enabled=false).");
  }

  private static void harvest() {
    Record into = null;
    if (!overlapped) {
      into = current >= 0 && current < records.size() ? records.get(current) : outside();
    }
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
      if (!overlapped) {
        for (long key : keys) {
          if (into != null) {
            into.vectors.add(new long[] {id, key});
          }
          runWide.set(id, including(runWide.get(id), key));
        }
      }
      if (keys.length > 0) {
        seen.set(id, new long[0]);
      }
      recentFirst[id] = 0L;
      recentSecond[id] = 0L;
    }
  }

  /** {@code keys} with {@code key} in it, which may be {@code keys} itself. */
  private static long[] including(long[] keys, long key) {
    for (long existing : keys) {
      if (existing == key) {
        return keys;
      }
    }
    long[] grown = new long[keys.length + 1];
    System.arraycopy(keys, 0, grown, 0, keys.length);
    grown[keys.length] = key;
    return grown;
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

  /**
   * Writes this JVM's evidence into {@code directory}, under a name no other
   * JVM will choose.
   *
   * <p>A build may fork more than one JVM to run its tests in parallel --
   * Gradle's maxParallelForks and surefire's forkCount both do, and RxJava
   * sets the first to the number of processors. Every one of them runs this
   * listener, so a single agreed path means each fork overwrites the last and
   * the run keeps whichever finished last: 49 tests of the 406 that ran.
   *
   * <p>The name is a UUID rather than the process id. Nothing reads the name
   * -- the engine merges every file it finds -- so all it has to be is unique
   * per JVM and the same each time this JVM writes. ProcessHandle would say
   * the same thing and is Java 9, and a great many libraries still compile
   * their main source at 8: moshi, gson and OkHttp all do, and the runtime is
   * compiled by the project's own javac, at whatever release the project set.
   */
  private static final String JVM = java.util.UUID.randomUUID().toString();

  /**
   * A test name with its control characters written out, so it reads back as
   * one line. Shared by every listener and formatter that names a test.
   */
  public static String printable(String name) {
    StringBuilder out = null;
    for (int i = 0; i < name.length(); i++) {
      char c = name.charAt(i);
      if (!Character.isISOControl(c)) {
        if (out != null) {
          out.append(c);
        }
        continue;
      }
      if (out == null) {
        out = new StringBuilder(name.length() + 8).append(name, 0, i);
      }
      switch (c) {
        case '\n':
          out.append("\\n");
          break;
        case '\t':
          out.append("\\t");
          break;
        case '\r':
          out.append("\\r");
          break;
        default:
          out.append(String.format("\\u%04x", (int) c));
      }
    }
    return out == null ? name : out.toString();
  }
  public static synchronized void writeInto(String directory) throws IOException {
    java.io.File target = new java.io.File(directory);
    if (!target.isDirectory() && !target.mkdirs() && !target.isDirectory()) {
      throw new IOException("could not create " + directory);
    }
    // Written beside and moved into place: a JVM stopped while writing --
    // a timeout ends a forked one, and each suite rewrites the whole file --
    // leaves the evidence it had rather than a file cut short.
    java.io.File finished = new java.io.File(target, JVM + ".bin");
    java.io.File writing = new java.io.File(target, JVM + ".bin.partial");
    write(writing.getPath());
    if (!writing.renameTo(finished)) {
      finished.delete();
      if (!writing.renameTo(finished)) {
        throw new IOException("could not move " + writing + " into place");
      }
    }
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
      // A class whose own work reached nothing has nothing to credit.
      List<Record> written = new ArrayList<>();
      for (Record record : records) {
        if (!record.setup || !record.probes.isEmpty() || !record.vectors.isEmpty()) {
          written.add(record);
        }
      }
      putLong(out, written.size());
      for (Record record : written) {
        byte[] name = record.name.getBytes("UTF-8");
        putLong(out, name.length);
        out.write(name);
        byte[] status = ((record.setup ? SETUP : "") + record.status).getBytes("UTF-8");
        putLong(out, status.length);
        out.write(status);
        // Whether the probes below are this test's own. Every JUnit Platform
        // and TestNG lifecycle this attaches to reports one test at a time, so
        // it always is -- but the reader is shared with frontends where that
        // is not true, and a field written by one writer and not the other is
        // not a format, it is a guess about which wrote the file.
        putLong(out, record.unattributed ? 1 : 0);
        byte[] runner = record.runner.getBytes("UTF-8");
        putLong(out, runner.length);
        out.write(runner);
        putLong(out, record.tags.length);
        for (String tag : record.tags) {
          byte[] bytes = tag.getBytes("UTF-8");
          putLong(out, bytes.length);
          out.write(bytes);
        }
        // A record harvested more than once (a class's setup, around each
        // of its tests) holds a probe once per harvest: one entry each, with
        // every bit any harvest saw.
        java.util.Map<Integer, Integer> probes = new java.util.TreeMap<>();
        for (int[] hit : record.probes) {
          Integer seen = probes.get(hit[0]);
          probes.put(hit[0], seen == null ? hit[1] : seen | hit[1]);
        }
        putLong(out, probes.size());
        for (java.util.Map.Entry<Integer, Integer> hit : probes.entrySet()) {
          putLong(out, hit.getKey());
          putLong(out, hit.getValue());
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
        // Run-wide rather than a union over the records: the table describes
        // what the run evaluated, which stands even when attribution does not.
        long[] union = id < runWide.size() ? runWide.get(id) : new long[0];
        putLong(out, union.length);
        for (long key : union) {
          putLong(out, key);
        }
      }
    }
  }

  // --- Strong mutation: the mutant switches ---------------------------------
  //
  // A strong copy calls these where it holds a mutant (`jvm_schemata.rs`):
  // `on(file, index)` says whether that mutant is the active one, and each
  // helper applies its operator or the mutant's replacement. One mutant is
  // active at a time, for the whole JVM; none while the file is -1.

  private static int activeFile = -1;
  private static int activeIndex = -1;
  private static long mutantHits = 0;
  private static long mutantHitLimit = 1_000_000L;
  private static long mutantHitsAllowed = 1_000_000L;
  private static boolean mutantHitLimitReached = false;
  /** Until when the test may run on, in nanoseconds; 0 for no time. */
  private static long mutantDeadline = 0;

  /** Thrown once the active mutant has been evaluated past its limit. */
  public static final class MutantTimeoutError extends Error {
    private static final long serialVersionUID = 1L;

    public MutantTimeoutError() {
      super("SUPERCOV_MUTANT_TIMEOUT: the active mutant passed its evaluation limit");
    }
  }

  /** Switches one mutant on: every switch of it reads true until cleared. */
  public static void setActiveMutant(int file, int index) {
    activeFile = file;
    activeIndex = index;
    mutantHits = 0;
    mutantHitsAllowed = mutantHitLimit;
    mutantHitLimitReached = false;
    mutantDeadline = 0;
  }

  /**
   * Switches one mutant on for a test with {@code budgetMillis} to run: past
   * the evaluation limit within that time, the mutant is busy, not stuck
   * (a hot loop of a heavy algorithm), and is allowed as many again.
   */
  public static void setActiveMutant(int file, int index, int budgetMillis) {
    setActiveMutant(file, index);
    mutantDeadline = System.nanoTime() + budgetMillis * 1_000_000L;
  }

  public static void clearActiveMutant() {
    activeFile = -1;
    activeIndex = -1;
    mutantHits = 0;
  }

  public static void setMutantHitLimit(long limit) {
    mutantHitLimit = limit;
  }

  public static long mutantHitCount() {
    return mutantHits;
  }

  /** Whether the limit was passed since the mutant was switched on. */
  public static boolean mutantHitLimitReached() {
    return mutantHitLimitReached;
  }

  public static boolean isMutantTimeout(Throwable error) {
    for (Throwable cause = error; cause != null; cause = cause.getCause()) {
      if (cause instanceof MutantTimeoutError) {
        return true;
      }
      if (cause.getCause() == cause) {
        break;
      }
    }
    return false;
  }

  /** Whether mutant {@code index} of file {@code file} is on; counts it if so. */
  public static boolean on(int file, int index) {
    if (index != activeIndex || file != activeFile) {
      return false;
    }
    if (++mutantHits > mutantHitsAllowed) {
      if (mutantDeadline != 0 && System.nanoTime() < mutantDeadline) {
        mutantHitsAllowed += mutantHitLimit;
        return true;
      }
      mutantHitLimitReached = true;
      throw new MutantTimeoutError();
    }
    return true;
  }

  /** A condition, negated while its mutant is on. */
  public static boolean n(int file, int index, boolean value) {
    return on(file, index) ? !value : value;
  }

  /** Evaluates a choice made as a statement. */
  public static void v(boolean value) {}

  public static void v(char value) {}

  public static void v(byte value) {}

  public static void v(short value) {}

  public static void v(int value) {}

  public static void v(long value) {}

  public static void v(float value) {}

  public static void v(double value) {}

  public static void v(Object value) {}

  public static boolean lt(int file, int index, int a, int b) {
    return on(file, index) ? a <= b : a < b;
  }

  public static boolean lt(int file, int index, long a, long b) {
    return on(file, index) ? a <= b : a < b;
  }

  public static boolean lt(int file, int index, float a, float b) {
    return on(file, index) ? a <= b : a < b;
  }

  public static boolean lt(int file, int index, double a, double b) {
    return on(file, index) ? a <= b : a < b;
  }

  public static boolean le(int file, int index, int a, int b) {
    return on(file, index) ? a < b : a <= b;
  }

  public static boolean le(int file, int index, long a, long b) {
    return on(file, index) ? a < b : a <= b;
  }

  public static boolean le(int file, int index, float a, float b) {
    return on(file, index) ? a < b : a <= b;
  }

  public static boolean le(int file, int index, double a, double b) {
    return on(file, index) ? a < b : a <= b;
  }

  public static boolean gt(int file, int index, int a, int b) {
    return on(file, index) ? a >= b : a > b;
  }

  public static boolean gt(int file, int index, long a, long b) {
    return on(file, index) ? a >= b : a > b;
  }

  public static boolean gt(int file, int index, float a, float b) {
    return on(file, index) ? a >= b : a > b;
  }

  public static boolean gt(int file, int index, double a, double b) {
    return on(file, index) ? a >= b : a > b;
  }

  public static boolean ge(int file, int index, int a, int b) {
    return on(file, index) ? a > b : a >= b;
  }

  public static boolean ge(int file, int index, long a, long b) {
    return on(file, index) ? a > b : a >= b;
  }

  public static boolean ge(int file, int index, float a, float b) {
    return on(file, index) ? a > b : a >= b;
  }

  public static boolean ge(int file, int index, double a, double b) {
    return on(file, index) ? a > b : a >= b;
  }

  public static int add(int file, int index, int a, int b) {
    return on(file, index) ? a - b : a + b;
  }

  public static long add(int file, int index, long a, long b) {
    return on(file, index) ? a - b : a + b;
  }

  public static float add(int file, int index, float a, float b) {
    return on(file, index) ? a - b : a + b;
  }

  public static double add(int file, int index, double a, double b) {
    return on(file, index) ? a - b : a + b;
  }

  public static int sub(int file, int index, int a, int b) {
    return on(file, index) ? a + b : a - b;
  }

  public static long sub(int file, int index, long a, long b) {
    return on(file, index) ? a + b : a - b;
  }

  public static float sub(int file, int index, float a, float b) {
    return on(file, index) ? a + b : a - b;
  }

  public static double sub(int file, int index, double a, double b) {
    return on(file, index) ? a + b : a - b;
  }

  public static int mul(int file, int index, int a, int b) {
    return on(file, index) ? a / b : a * b;
  }

  public static long mul(int file, int index, long a, long b) {
    return on(file, index) ? a / b : a * b;
  }

  public static float mul(int file, int index, float a, float b) {
    return on(file, index) ? a / b : a * b;
  }

  public static double mul(int file, int index, double a, double b) {
    return on(file, index) ? a / b : a * b;
  }

  public static int div(int file, int index, int a, int b) {
    return on(file, index) ? a * b : a / b;
  }

  public static long div(int file, int index, long a, long b) {
    return on(file, index) ? a * b : a / b;
  }

  public static float div(int file, int index, float a, float b) {
    return on(file, index) ? a * b : a / b;
  }

  public static double div(int file, int index, double a, double b) {
    return on(file, index) ? a * b : a / b;
  }

  public static int rem(int file, int index, int a, int b) {
    return on(file, index) ? a * b : a % b;
  }

  public static long rem(int file, int index, long a, long b) {
    return on(file, index) ? a * b : a % b;
  }

  public static float rem(int file, int index, float a, float b) {
    return on(file, index) ? a * b : a % b;
  }

  public static double rem(int file, int index, double a, double b) {
    return on(file, index) ? a * b : a % b;
  }

  public static boolean and(int file, int index, boolean a, boolean b) {
    return on(file, index) ? a | b : a & b;
  }

  public static int and(int file, int index, int a, int b) {
    return on(file, index) ? a | b : a & b;
  }

  public static long and(int file, int index, long a, long b) {
    return on(file, index) ? a | b : a & b;
  }

  public static boolean or(int file, int index, boolean a, boolean b) {
    return on(file, index) ? a & b : a | b;
  }

  public static int or(int file, int index, int a, int b) {
    return on(file, index) ? a & b : a | b;
  }

  public static long or(int file, int index, long a, long b) {
    return on(file, index) ? a & b : a | b;
  }

  public static boolean xor(int file, int index, boolean a, boolean b) {
    return on(file, index) ? a & b : a ^ b;
  }

  public static int xor(int file, int index, int a, int b) {
    return on(file, index) ? a & b : a ^ b;
  }

  public static long xor(int file, int index, long a, long b) {
    return on(file, index) ? a & b : a ^ b;
  }

  public static int shl(int file, int index, int a, int b) {
    return on(file, index) ? a >> b : a << b;
  }

  public static int shl(int file, int index, int a, long b) {
    return on(file, index) ? a >> b : a << b;
  }

  public static long shl(int file, int index, long a, int b) {
    return on(file, index) ? a >> b : a << b;
  }

  public static long shl(int file, int index, long a, long b) {
    return on(file, index) ? a >> b : a << b;
  }

  public static int shr(int file, int index, int a, int b) {
    return on(file, index) ? a << b : a >> b;
  }

  public static int shr(int file, int index, int a, long b) {
    return on(file, index) ? a << b : a >> b;
  }

  public static long shr(int file, int index, long a, int b) {
    return on(file, index) ? a << b : a >> b;
  }

  public static long shr(int file, int index, long a, long b) {
    return on(file, index) ? a << b : a >> b;
  }

  public static int ushr(int file, int index, int a, int b) {
    return on(file, index) ? a << b : a >>> b;
  }

  public static int ushr(int file, int index, int a, long b) {
    return on(file, index) ? a << b : a >>> b;
  }

  public static long ushr(int file, int index, long a, int b) {
    return on(file, index) ? a << b : a >>> b;
  }

  public static long ushr(int file, int index, long a, long b) {
    return on(file, index) ? a << b : a >>> b;
  }

  public static int inv(int file, int index, int a) {
    return on(file, index) ? a : -a;
  }

  public static long inv(int file, int index, long a) {
    return on(file, index) ? a : -a;
  }

  public static float inv(int file, int index, float a) {
    return on(file, index) ? a : -a;
  }

  public static double inv(int file, int index, double a) {
    return on(file, index) ? a : -a;
  }

  public static int rZ(int file, int index, int value) {
    return on(file, index) ? 0 : value;
  }

  public static long rZ(int file, int index, long value) {
    return on(file, index) ? (long) 0 : value;
  }

  public static float rZ(int file, int index, float value) {
    return on(file, index) ? (float) 0 : value;
  }

  public static double rZ(int file, int index, double value) {
    return on(file, index) ? (double) 0 : value;
  }

  public static byte rZ(int file, int index, byte value) {
    return on(file, index) ? (byte) 0 : value;
  }

  public static short rZ(int file, int index, short value) {
    return on(file, index) ? (short) 0 : value;
  }

  public static char rZ(int file, int index, char value) {
    return on(file, index) ? (char) 0 : value;
  }

  public static boolean rT(int file, int index, boolean value) {
    return on(file, index) ? true : value;
  }

  public static boolean rF(int file, int index, boolean value) {
    return on(file, index) ? false : value;
  }

  // The object returns are generic so that one helper serves every declared
  // type: the caller casts the replacement to the type it returns, which the
  // replacement has (an empty list is a Collection and an Iterable). Each
  // call names its type argument: Java 7 infers none from a conditional.

  @SuppressWarnings("unchecked")
  private static <T> T as(Object value) {
    return (T) value;
  }

  public static <T> T rBT(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Boolean.TRUE) : value;
  }

  public static <T> T rBF(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Boolean.FALSE) : value;
  }

  public static <T> T rN(int file, int index, T value) {
    return on(file, index) ? null : value;
  }

  public static <T> T rBI(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Integer.valueOf(0)) : value;
  }

  public static <T> T rBS(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Short.valueOf((short) 0)) : value;
  }

  public static <T> T rBC(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Character.valueOf((char) 0)) : value;
  }

  public static <T> T rBL(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Long.valueOf(0L)) : value;
  }

  public static <T> T rBF0(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Float.valueOf(0f)) : value;
  }

  public static <T> T rBD(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(Double.valueOf(0d)) : value;
  }

  public static String rS(int file, int index, String value) {
    return on(file, index) ? "" : value;
  }

  public static <T> T rList(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(java.util.Collections.emptyList()) : value;
  }

  public static <T> T rSet(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(java.util.Collections.emptySet()) : value;
  }

  public static <T> T rMap(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(java.util.Collections.emptyMap()) : value;
  }

  // Optional and Stream by name, so that the runtime still compiles where a
  // project's main source is Java 7.
  public static <T> T rOpt(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(empty("java.util.Optional")) : value;
  }

  public static <T> T rStream(int file, int index, T value) {
    return on(file, index) ? Supercov.<T>as(empty("java.util.stream.Stream")) : value;
  }

  private static Object empty(String type) {
    try {
      return Class.forName(type).getMethod("empty").invoke(null);
    } catch (ReflectiveOperationException error) {
      throw new IllegalStateException(error);
    }
  }


  // --- Weak mutation: value probes -------------------------------------------
  //
  // A run that lists its mutants (SUPERCOV_MUTANTS=1) calls these where a
  // mutant's value can be told from the original's: each returns exactly
  // what the code as written computes, and ORs into its slot a bit for what
  // decides the mutant (`jvm_strong::value_outcomes` names the bits). The
  // slot is swept at each test boundary as every probe is.

  private static void value(int slot, int bit) {
    if (slot >= 0 && slot < HITS.length) {
      HITS[slot] |= bit;
    }
  }

  private static int relation(long a, long b) {
    return a < b ? 1 : a == b ? 2 : 4;
  }

  private static int relation(double a, double b) {
    return a < b ? 1 : a == b ? 2 : a > b ? 4 : 8;
  }

  private static int sameOrDiffers(boolean same) {
    return same ? 1 : 2;
  }

  public static boolean wlt(int slot, int a, int b) {
    value(slot, relation(a, b));
    return a < b;
  }

  public static boolean wlt(int slot, long a, long b) {
    value(slot, relation(a, b));
    return a < b;
  }

  public static boolean wlt(int slot, float a, float b) {
    value(slot, relation((double) a, (double) b));
    return a < b;
  }

  public static boolean wlt(int slot, double a, double b) {
    value(slot, relation(a, b));
    return a < b;
  }

  public static boolean wle(int slot, int a, int b) {
    value(slot, relation(a, b));
    return a <= b;
  }

  public static boolean wle(int slot, long a, long b) {
    value(slot, relation(a, b));
    return a <= b;
  }

  public static boolean wle(int slot, float a, float b) {
    value(slot, relation((double) a, (double) b));
    return a <= b;
  }

  public static boolean wle(int slot, double a, double b) {
    value(slot, relation(a, b));
    return a <= b;
  }

  public static boolean wgt(int slot, int a, int b) {
    value(slot, relation(a, b));
    return a > b;
  }

  public static boolean wgt(int slot, long a, long b) {
    value(slot, relation(a, b));
    return a > b;
  }

  public static boolean wgt(int slot, float a, float b) {
    value(slot, relation((double) a, (double) b));
    return a > b;
  }

  public static boolean wgt(int slot, double a, double b) {
    value(slot, relation(a, b));
    return a > b;
  }

  public static boolean wge(int slot, int a, int b) {
    value(slot, relation(a, b));
    return a >= b;
  }

  public static boolean wge(int slot, long a, long b) {
    value(slot, relation(a, b));
    return a >= b;
  }

  public static boolean wge(int slot, float a, float b) {
    value(slot, relation((double) a, (double) b));
    return a >= b;
  }

  public static boolean wge(int slot, double a, double b) {
    value(slot, relation(a, b));
    return a >= b;
  }

  public static int wadd(int slot, int a, int b) {
    int result = a + b;
    boolean same;
    try {
      int mutant = a - b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static long wadd(int slot, long a, long b) {
    long result = a + b;
    boolean same;
    try {
      long mutant = a - b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static float wadd(int slot, float a, float b) {
    float result = a + b;
    boolean same;
    try {
      float mutant = a - b;
      same = Float.floatToIntBits(result) == Float.floatToIntBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static double wadd(int slot, double a, double b) {
    double result = a + b;
    boolean same;
    try {
      double mutant = a - b;
      same = Double.doubleToLongBits(result) == Double.doubleToLongBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static int wsub(int slot, int a, int b) {
    int result = a - b;
    boolean same;
    try {
      int mutant = a + b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static long wsub(int slot, long a, long b) {
    long result = a - b;
    boolean same;
    try {
      long mutant = a + b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static float wsub(int slot, float a, float b) {
    float result = a - b;
    boolean same;
    try {
      float mutant = a + b;
      same = Float.floatToIntBits(result) == Float.floatToIntBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static double wsub(int slot, double a, double b) {
    double result = a - b;
    boolean same;
    try {
      double mutant = a + b;
      same = Double.doubleToLongBits(result) == Double.doubleToLongBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static int wmul(int slot, int a, int b) {
    int result = a * b;
    boolean same;
    try {
      int mutant = a / b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static long wmul(int slot, long a, long b) {
    long result = a * b;
    boolean same;
    try {
      long mutant = a / b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static float wmul(int slot, float a, float b) {
    float result = a * b;
    boolean same;
    try {
      float mutant = a / b;
      same = Float.floatToIntBits(result) == Float.floatToIntBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static double wmul(int slot, double a, double b) {
    double result = a * b;
    boolean same;
    try {
      double mutant = a / b;
      same = Double.doubleToLongBits(result) == Double.doubleToLongBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static int wdiv(int slot, int a, int b) {
    int result = a / b;
    boolean same;
    try {
      int mutant = a * b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static long wdiv(int slot, long a, long b) {
    long result = a / b;
    boolean same;
    try {
      long mutant = a * b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static float wdiv(int slot, float a, float b) {
    float result = a / b;
    boolean same;
    try {
      float mutant = a * b;
      same = Float.floatToIntBits(result) == Float.floatToIntBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static double wdiv(int slot, double a, double b) {
    double result = a / b;
    boolean same;
    try {
      double mutant = a * b;
      same = Double.doubleToLongBits(result) == Double.doubleToLongBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static int wrem(int slot, int a, int b) {
    int result = a % b;
    boolean same;
    try {
      int mutant = a * b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static long wrem(int slot, long a, long b) {
    long result = a % b;
    boolean same;
    try {
      long mutant = a * b;
      same = result == mutant;
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static float wrem(int slot, float a, float b) {
    float result = a % b;
    boolean same;
    try {
      float mutant = a * b;
      same = Float.floatToIntBits(result) == Float.floatToIntBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static double wrem(int slot, double a, double b) {
    double result = a % b;
    boolean same;
    try {
      double mutant = a * b;
      same = Double.doubleToLongBits(result) == Double.doubleToLongBits(mutant);
    } catch (ArithmeticException thrown) {
      same = false;
    }
    value(slot, sameOrDiffers(same));
    return result;
  }

  public static boolean wand(int slot, boolean a, boolean b) {
    boolean result = a & b;
    value(slot, sameOrDiffers(result == (a | b)));
    return result;
  }

  public static int wand(int slot, int a, int b) {
    int result = a & b;
    value(slot, sameOrDiffers(result == (a | b)));
    return result;
  }

  public static long wand(int slot, long a, long b) {
    long result = a & b;
    value(slot, sameOrDiffers(result == (a | b)));
    return result;
  }

  public static boolean wor(int slot, boolean a, boolean b) {
    boolean result = a | b;
    value(slot, sameOrDiffers(result == (a & b)));
    return result;
  }

  public static int wor(int slot, int a, int b) {
    int result = a | b;
    value(slot, sameOrDiffers(result == (a & b)));
    return result;
  }

  public static long wor(int slot, long a, long b) {
    long result = a | b;
    value(slot, sameOrDiffers(result == (a & b)));
    return result;
  }

  public static boolean wxor(int slot, boolean a, boolean b) {
    boolean result = a ^ b;
    value(slot, sameOrDiffers(result == (a & b)));
    return result;
  }

  public static int wxor(int slot, int a, int b) {
    int result = a ^ b;
    value(slot, sameOrDiffers(result == (a & b)));
    return result;
  }

  public static long wxor(int slot, long a, long b) {
    long result = a ^ b;
    value(slot, sameOrDiffers(result == (a & b)));
    return result;
  }

  public static int wshl(int slot, int a, int b) {
    int result = a << b;
    value(slot, sameOrDiffers(result == (a >> b)));
    return result;
  }

  public static int wshl(int slot, int a, long b) {
    int result = a << b;
    value(slot, sameOrDiffers(result == (a >> b)));
    return result;
  }

  public static long wshl(int slot, long a, int b) {
    long result = a << b;
    value(slot, sameOrDiffers(result == (a >> b)));
    return result;
  }

  public static long wshl(int slot, long a, long b) {
    long result = a << b;
    value(slot, sameOrDiffers(result == (a >> b)));
    return result;
  }

  public static int wshr(int slot, int a, int b) {
    int result = a >> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static int wshr(int slot, int a, long b) {
    int result = a >> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static long wshr(int slot, long a, int b) {
    long result = a >> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static long wshr(int slot, long a, long b) {
    long result = a >> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static int wushr(int slot, int a, int b) {
    int result = a >>> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static int wushr(int slot, int a, long b) {
    int result = a >>> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static long wushr(int slot, long a, int b) {
    long result = a >>> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static long wushr(int slot, long a, long b) {
    long result = a >>> b;
    value(slot, sameOrDiffers(result == (a << b)));
    return result;
  }

  public static int wneg(int slot, int a) {
    int result = -a;
    value(slot, sameOrDiffers(result == a));
    return result;
  }

  public static long wneg(int slot, long a) {
    long result = -a;
    value(slot, sameOrDiffers(result == a));
    return result;
  }

  public static float wneg(int slot, float a) {
    float result = -a;
    value(slot, sameOrDiffers(Float.floatToIntBits(result) == Float.floatToIntBits(a)));
    return result;
  }

  public static double wneg(int slot, double a) {
    double result = -a;
    value(slot, sameOrDiffers(Double.doubleToLongBits(result) == Double.doubleToLongBits(a)));
    return result;
  }

  // A returned value: bit 0 false, 1 true, 2 zero, 3 not zero (a float's
  // zero is +0.0 alone, as the mutant returns), 4 null, then the replacements
  // the return mutants make where the value is that very object.

  public static boolean wr(int slot, boolean value) {
    value(slot, value ? 2 : 1);
    return value;
  }

  public static int wr(int slot, int value) {
    value(slot, value == 0 ? 4 : 8);
    return value;
  }

  public static long wr(int slot, long value) {
    value(slot, value == 0 ? 4 : 8);
    return value;
  }

  public static byte wr(int slot, byte value) {
    value(slot, value == 0 ? 4 : 8);
    return value;
  }

  public static short wr(int slot, short value) {
    value(slot, value == 0 ? 4 : 8);
    return value;
  }

  public static char wr(int slot, char value) {
    value(slot, value == 0 ? 4 : 8);
    return value;
  }

  public static float wr(int slot, float value) {
    value(slot, Float.floatToIntBits(value) == 0 ? 4 : 8);
    return value;
  }

  public static double wr(int slot, double value) {
    value(slot, Double.doubleToLongBits(value) == 0L ? 4 : 8);
    return value;
  }

  public static <T> T wr(int slot, T value) {
    value(slot, returned(value));
    return value;
  }

  private static Object emptyOptional;
  private static boolean emptyOptionalRead;

  private static int returned(Object value) {
    if (value == null) {
      return 1 << 4;
    }
    if (value == "") {
      return 1 << 5;
    }
    if (value == java.util.Collections.emptyList()) {
      return 1 << 6;
    }
    if (value == java.util.Collections.emptySet()) {
      return 1 << 7;
    }
    if (value == java.util.Collections.emptyMap()) {
      return 1 << 8;
    }
    if (!emptyOptionalRead) {
      emptyOptionalRead = true;
      try {
        emptyOptional = Class.forName("java.util.Optional").getMethod("empty").invoke(null);
      } catch (ReflectiveOperationException | LinkageError absent) {
        emptyOptional = null;
      }
    }
    if (emptyOptional != null && value == emptyOptional) {
      return 1 << 9;
    }
    if (value == Integer.valueOf(0)
        || value == Long.valueOf(0L)
        || value == Short.valueOf((short) 0)
        || value == Character.valueOf((char) 0)) {
      return 1 << 10;
    }
    if (value == Boolean.TRUE) {
      return 1 << 11;
    }
    if (value == Boolean.FALSE) {
      return 1 << 12;
    }
    return 1 << 13;
  }

  /** Little-endian, matching every other Supercov transport. */
  private static void putLong(DataOutputStream out, long value) throws IOException {
    for (int shift = 0; shift < 64; shift += 8) {
      out.write((int) ((value >>> shift) & 0xFF));
    }
  }

  /** Names the directory a server a browser suite starts writes its windows to. */
  static final String SERVER_EVIDENCE_ENV = "SUPERCOV_JVM_SERVER_EVIDENCE_DIR";

  /** How often a server's probes are swept into a window of their own. */
  private static final long SERVER_WINDOW_MILLIS = 5;

  // Last, so every field the server mode reads is initialised before it runs.
  // This class initialises when instrumented code first touches HITS.
  static {
    String directory = System.getenv(SERVER_EVIDENCE_ENV);
    if (directory != null && !directory.isEmpty()) {
      serve(directory);
    }
  }

  /**
   * Measures a program that is not a test run: a server a browser suite
   * starts. It has no test boundaries to harvest at, and its requests arrive
   * from tests it cannot see -- and one shared array cannot tell two
   * concurrent requests apart -- so it harvests on a clock instead. Every
   * window that reached anything becomes one line with when it began and
   * ended, and the engine gives each to the browser test that was running
   * throughout it, if only one was.
   *
   * <p>Each window is appended as it closes, and the JVM's shutdown hook
   * writes the last one, so a server stopped by SIGTERM loses nothing. A sweep
   * reads a slot and then clears it, so a probe storing a different bit in
   * that instant can lose that bit; rare, and only between windows.
   */
  private static void serve(String directory) {
    arm(PROBE_COUNT, SERVER_WIDTHS);
    final File file =
        new File(
            directory,
            "server-" + System.nanoTime() + "-" + Long.toHexString(Double.doubleToLongBits(Math.random())) + ".windows");
    final long[] start = {System.currentTimeMillis()};
    // Anonymous classes, not lambdas: this class compiles with the
    // project's own sources, and a project may build for Java 7.
    Thread sweeper =
        new Thread(
            new Runnable() {
              @Override
              public void run() {
                while (true) {
                  try {
                    Thread.sleep(SERVER_WINDOW_MILLIS);
                  } catch (InterruptedException interrupted) {
                    return;
                  }
                  window(file, start);
                }
              }
            },
            "supercov-server-windows");
    sweeper.setDaemon(true);
    sweeper.start();
    Runtime.getRuntime()
        .addShutdownHook(
            new Thread(
                new Runnable() {
                  @Override
                  public void run() {
                    window(file, start);
                  }
                }));
  }

  /** Sweeps the array into one window and appends it, if it reached anything. */
  private static synchronized void window(File file, long[] start) {
    Record record = new Record("window");
    records.add(record);
    current = records.size() - 1;
    harvest();
    records.remove(records.size() - 1);
    current = -1;
    long end = System.currentTimeMillis();
    if (!record.probes.isEmpty() || !record.vectors.isEmpty()) {
      StringBuilder line = new StringBuilder(64 + record.probes.size() * 12);
      line.append("{\"start\":").append(start[0]).append(",\"end\":").append(end);
      line.append(",\"probes\":[");
      for (int index = 0; index < record.probes.size(); index++) {
        int[] hit = record.probes.get(index);
        line.append(index > 0 ? "," : "").append(hit[0]).append(',').append(hit[1]);
      }
      line.append("],\"vectors\":[");
      for (int index = 0; index < record.vectors.size(); index++) {
        long[] hit = record.vectors.get(index);
        line.append(index > 0 ? "," : "").append(hit[0]).append(',').append(hit[1]);
      }
      line.append("]}\n");
      try (FileOutputStream out = new FileOutputStream(file, true)) {
        out.write(line.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8));
      } catch (IOException error) {
        System.err.println("supercov: could not record the server's coverage: " + error.getMessage());
      }
    }
    start[0] = end;
  }
}
