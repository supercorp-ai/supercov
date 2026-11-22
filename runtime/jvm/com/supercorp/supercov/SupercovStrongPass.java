package com.supercorp.supercov;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The strong pass inside the build's own test JVM, as PIT's minion runs it:
 * the build compiles the strong copy once and starts its test JVM, and this
 * runs each mutant of the plan against its candidate tests in that JVM,
 * switching mutants between test executions. A framework's driver
 * ({@code SupercovStrong} for the JUnit Platform, {@code SupercovStrongTestNG})
 * starts it and gives it an {@link Executor} that runs one test.
 *
 * <p>A test is run once without a mutant before it is run with one (its
 * baseline), and a test that fails then is no candidate. A kill is confirmed
 * by running the killing test again without the mutant: a JVM whose state a
 * mutant changed for good (a static cache filled with a mutated value) fails
 * it too, and the pass stops so that the engine starts a fresh JVM for the
 * rest. A test past its time limit leaves a thread that will not stop: the
 * verdict is written and this JVM takes no more mutants.
 *
 * <p>The plan is {@code SUPERCOV_STRONG_JVM_PLAN}, written by the engine:
 * {@code T} lines name tests ({@code T, index, class, name, measured ms}),
 * {@code M} lines mutants ({@code M, row, id, file slot, index, modules,
 * candidates}). Verdicts are appended to {@code SUPERCOV_STRONG_RESULTS} as
 * JSON lines, one write per line. Several JVMs share a pass through
 * {@code SUPERCOV_STRONG_CLAIMS}: each mutant goes to the first that claims
 * it.
 */
public final class SupercovStrongPass {

  static final String PLAN = "SUPERCOV_STRONG_JVM_PLAN";
  static final String RESULTS = "SUPERCOV_STRONG_RESULTS";
  static final String CLAIMS = "SUPERCOV_STRONG_CLAIMS";
  /** Set in a JVM a driver started to share the pass. */
  static final String MINION = "supercov.strong.minion";

  /** Runs one test of a framework. */
  public interface Executor {
    /** Whether the test can run in this JVM: its class is this module's. */
    boolean resolve(Test test);

    /** Runs the test, recording how it ended in {@code outcome}. */
    void execute(Test test, Outcome outcome) throws Throwable;
  }

  public static final class Test {
    final int index;
    public final String className;
    public final String name;
    /** How long the measured run took for it, where it said; 0 where not. */
    final long measuredMillis;
    /** What the executor found for it (a unique id, a method). */
    public Object handle;
    Boolean baseline;
    long baselineMillis;

    Test(int index, String className, String name, long measuredMillis) {
      this.index = index;
      this.className = className;
      this.name = name;
      this.measuredMillis = measuredMillis;
    }

    /**
     * Twice StrykerJS's limit, as the JavaScript drivers give: the longer of
     * its measured run and its baseline, times 3, and 10 s.
     */
    long limitMillis() {
      return Math.max(measuredMillis, baselineMillis) * 3 + 10_000L;
    }
  }

  static final class Mutant {
    final int row;
    final String id;
    final int file;
    final int index;
    final List<String> modules;
    final List<Integer> candidates;

    Mutant(int row, String id, int file, int index, List<String> modules, List<Integer> candidates) {
      this.row = row;
      this.id = id;
      this.file = file;
      this.index = index;
      this.modules = modules;
      this.candidates = candidates;
    }
  }

  /** How one execution of one test ended. */
  public static final class Outcome {
    public boolean failed;
    boolean timedOut;
    public boolean mutantTimeout;
    public String reason;

    /** Records a failure, keeping the first one's reason. */
    public void fail(Throwable error) {
      failed = true;
      if (error != null && Supercov.isMutantTimeout(error)) {
        mutantTimeout = true;
      }
      if (reason == null && error != null) {
        String message = String.valueOf(error);
        reason = message.length() > 300 ? message.substring(0, 300) : message;
      }
    }
  }

  private final Map<Integer, Test> tests = new HashMap<>();
  private final List<Mutant> mutants = new ArrayList<>();
  private final String results;
  private final File claims;
  private final String module;
  private final Executor executor;

  SupercovStrongPass(Executor executor, String module) throws IOException {
    this.executor = executor;
    this.module = module;
    this.results = System.getenv(RESULTS);
    String claimed = System.getenv(CLAIMS);
    this.claims = claimed == null ? null : new File(claimed);
    String limit = System.getenv("SUPERCOV_MUTANT_HIT_LIMIT");
    if (limit != null) {
      try {
        Supercov.setMutantHitLimit(Long.parseLong(limit.trim()));
      } catch (NumberFormatException e) {
        // The default limit.
      }
    }
    try (BufferedReader reader =
        new BufferedReader(
            new InputStreamReader(new FileInputStream(System.getenv(PLAN)), StandardCharsets.UTF_8))) {
      String line;
      while ((line = reader.readLine()) != null) {
        String[] parts = line.split("\t", -1);
        if (parts[0].equals("T") && parts.length >= 5) {
          int index = Integer.parseInt(parts[1]);
          tests.put(
              index,
              new Test(index, unescape(parts[2]), unescape(parts[3]), Long.parseLong(parts[4])));
        } else if (parts[0].equals("M") && parts.length >= 7) {
          List<String> modules = new ArrayList<>();
          for (String each : parts[5].split(",")) {
            if (!each.isEmpty()) {
              modules.add(each);
            }
          }
          List<Integer> candidates = new ArrayList<>();
          for (String each : parts[6].split(",")) {
            if (!each.isEmpty()) {
              candidates.add(Integer.parseInt(each));
            }
          }
          mutants.add(
              new Mutant(
                  Integer.parseInt(parts[1]),
                  unescape(parts[2]),
                  Integer.parseInt(parts[3]),
                  Integer.parseInt(parts[4]),
                  modules,
                  candidates));
        }
      }
    }
  }

  /** The module this JVM tests, as the strong copy's configuration names it. */
  static String module() {
    try {
      return (String)
          Class.forName("com.supercorp.supercov.SupercovConfig").getField("MODULE").get(null);
    } catch (ReflectiveOperationException | ClassCastException e) {
      return ".";
    }
  }

  /** Whether a strong pass asks this JVM to run it, once. */
  private static boolean started = false;

  static synchronized boolean shouldStart() {
    if (System.getenv(PLAN) == null || started || Boolean.getBoolean(MINION)) {
      return false;
    }
    started = true;
    return true;
  }

  /** Runs the pass here and in the other JVMs it starts, and waits for them. */
  static void runEverywhere(Executor executor, String framework) {
    List<Process> minions = startMinions(framework);
    try {
      new SupercovStrongPass(executor, module()).run();
    } catch (Throwable error) {
      System.err.println("supercov: the strong pass stopped: " + error);
      error.printStackTrace();
    } finally {
      Supercov.clearActiveMutant();
      for (Process minion : minions) {
        try {
          minion.waitFor();
        } catch (InterruptedException e) {
          Thread.currentThread().interrupt();
        }
      }
    }
  }

  /**
   * More JVMs for the pass, as PIT runs more minions: each claims mutants
   * as this one does, so none runs a mutant twice. They are this JVM again
   * (its java, its options, its class path) with the framework driver's
   * {@code main} as the entry. {@code SUPERCOV_STRONG_JVMS} says how many
   * run in all; one by default.
   */
  private static List<Process> startMinions(String framework) {
    List<Process> started = new ArrayList<>();
    int jvms = 1;
    try {
      String asked = System.getenv("SUPERCOV_STRONG_JVMS");
      if (asked != null) {
        jvms = Math.max(1, Integer.parseInt(asked.trim()));
      }
    } catch (NumberFormatException e) {
      jvms = 1;
    }
    if (jvms <= 1 || System.getenv(CLAIMS) == null) {
      return started;
    }
    List<String> command = new ArrayList<>();
    command.add(new File(new File(System.getProperty("java.home"), "bin"), "java").getPath());
    try {
      for (String argument :
          java.lang.management.ManagementFactory.getRuntimeMXBean().getInputArguments()) {
        // A debugger's agent listens on one port, which a second JVM cannot.
        if (!argument.startsWith("-agentlib:jdwp") && !argument.startsWith("-Xrunjdwp")) {
          command.add(argument);
        }
      }
    } catch (Throwable e) {
      // No management API: the minions run with defaults.
    }
    command.add("-D" + MINION + "=true");
    command.add("-cp");
    command.add(System.getProperty("java.class.path"));
    command.add(framework);
    command.add(module());
    for (int i = 1; i < jvms; i++) {
      try {
        started.add(new ProcessBuilder(command).inheritIO().start());
      } catch (IOException e) {
        System.err.println("supercov: could not start another JVM for the strong pass: " + e);
        break;
      }
    }
    return started;
  }

  /** A minion's whole life: the pass, then the JVM's end. */
  static void minion(Executor executor, String[] args) {
    started = true;
    try {
      new SupercovStrongPass(executor, args.length > 0 ? args[0] : module()).run();
    } catch (Throwable error) {
      System.err.println("supercov: a strong pass JVM stopped: " + error);
    }
    // A test may have left threads running: the JVM ends with the pass.
    Runtime.getRuntime().halt(0);
  }

  void run() throws IOException {
    for (Mutant mutant : mutants) {
      if (!mutant.modules.contains(module)) {
        continue;
      }
      List<Test> mine = new ArrayList<>();
      for (int candidate : mutant.candidates) {
        Test test = tests.get(candidate);
        if (test != null && executor.resolve(test)) {
          mine.add(test);
        }
      }
      if (mine.isEmpty() || !claim(mutant)) {
        continue;
      }
      if (!runMutant(mutant, mine)) {
        // This JVM cannot be trusted with another mutant.
        return;
      }
    }
  }

  /** Whether this JVM took the mutant; another may have. */
  private boolean claim(Mutant mutant) {
    if (claims == null) {
      return true;
    }
    try {
      claims.mkdirs();
      return new File(claims, module.replaceAll("[^A-Za-z0-9]", "_") + "-" + mutant.row)
          .createNewFile();
    } catch (IOException e) {
      return true;
    }
  }

  /** Runs one mutant; false when the JVM must stop after it. */
  private boolean runMutant(Mutant mutant, List<Test> candidates) throws IOException {
    long started = System.nanoTime();
    int ran = 0;
    for (Test test : candidates) {
      if (test.baseline == null) {
        long before = System.nanoTime();
        Outcome baseline = execute(test, false, -1, -1);
        test.baselineMillis = (System.nanoTime() - before) / 1_000_000L;
        if (baseline.timedOut) {
          write(mutant, "error", null, ran, started,
              "its test " + test.name + " ran past its time without a mutant");
          return false;
        }
        test.baseline = !baseline.failed;
      }
      if (!test.baseline) {
        continue;
      }
      ran++;
      Outcome outcome = execute(test, true, mutant.file, mutant.index);
      if (outcome.timedOut) {
        write(mutant, "timeout", test.name, ran, started, "ran past its time limit");
        return false;
      }
      if (outcome.mutantTimeout) {
        write(mutant, "timeout", test.name, ran, started, "the mutant passed its evaluation limit");
        return true;
      }
      if (outcome.failed) {
        Outcome confirmation = execute(test, false, -1, -1);
        if (confirmation.failed || confirmation.timedOut) {
          // The mutant left the JVM so that the test fails without it: no
          // verdict here, and a fresh JVM for the rest.
          return false;
        }
        write(mutant, "killed", test.name, ran, started, outcome.reason);
        return true;
      }
    }
    write(mutant, "survived", null, ran, started, ran == 0 ? "no-passing-candidate" : null);
    return true;
  }

  private Outcome execute(final Test test, boolean mutated, int file, int index) {
    final Outcome outcome = new Outcome();
    if (mutated) {
      Supercov.setActiveMutant(file, index);
    } else {
      Supercov.clearActiveMutant();
    }
    Thread worker =
        new Thread(
            () -> {
              try {
                executor.execute(test, outcome);
              } catch (Throwable error) {
                outcome.fail(error);
              }
            },
            "supercov-strong-" + test.index);
    worker.setDaemon(true);
    worker.setContextClassLoader(Thread.currentThread().getContextClassLoader());
    worker.start();
    try {
      // A baseline gets a minute more: nothing has measured it yet.
      worker.join(mutated ? test.limitMillis() : test.limitMillis() + 60_000L);
    } catch (InterruptedException e) {
      Thread.currentThread().interrupt();
    }
    if (worker.isAlive()) {
      outcome.timedOut = true;
    }
    if (mutated && Supercov.mutantHitLimitReached()) {
      outcome.mutantTimeout = true;
    }
    Supercov.clearActiveMutant();
    return outcome;
  }

  private void write(Mutant mutant, String status, String killedBy, int ran, long started, String reason)
      throws IOException {
    boolean partial = mutant.modules.size() > 1;
    StringBuilder line = new StringBuilder();
    line.append('{');
    if (partial) {
      line.append("\"partial\":true,\"row\":").append(mutant.row).append(",\"module\":");
      json(line, module);
      line.append(",\"mutant\":");
    } else {
      line.append("\"id\":");
    }
    json(line, mutant.id);
    line.append(",\"status\":");
    json(line, status);
    if (killedBy != null) {
      line.append(",\"killedBy\":");
      json(line, killedBy);
    }
    line.append(",\"tests\":").append(ran);
    line.append(",\"durationMs\":").append((System.nanoTime() - started) / 1_000_000L);
    if (reason != null) {
      line.append(",\"reason\":");
      json(line, reason);
    }
    line.append("}\n");
    byte[] bytes = line.toString().getBytes(StandardCharsets.UTF_8);
    try (FileOutputStream out = new FileOutputStream(results, true)) {
      out.write(bytes);
    }
  }

  static void json(StringBuilder out, String text) {
    out.append('"');
    for (int i = 0; i < text.length(); i++) {
      char c = text.charAt(i);
      switch (c) {
        case '"':
          out.append("\\\"");
          break;
        case '\\':
          out.append("\\\\");
          break;
        case '\n':
          out.append("\\n");
          break;
        case '\r':
          out.append("\\r");
          break;
        case '\t':
          out.append("\\t");
          break;
        default:
          if (c < 0x20) {
            out.append(String.format("\\u%04x", (int) c));
          } else {
            out.append(c);
          }
      }
    }
    out.append('"');
  }

  static String unescape(String text) {
    if (text.indexOf('\\') < 0) {
      return text;
    }
    StringBuilder out = new StringBuilder(text.length());
    for (int i = 0; i < text.length(); i++) {
      char c = text.charAt(i);
      if (c == '\\' && i + 1 < text.length()) {
        char next = text.charAt(++i);
        out.append(next == 't' ? '\t' : next == 'n' ? '\n' : next);
      } else {
        out.append(c);
      }
    }
    return out.toString();
  }
}
