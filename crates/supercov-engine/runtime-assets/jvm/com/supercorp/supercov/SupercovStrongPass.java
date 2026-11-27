package com.supercorp.supercov;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.Writer;
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
  /**
   * Where a JVM lists the mutants whose test ran past the first, short time
   * limit; set to retry them, the round that runs them alone with the
   * generous limit ({@code SUPERCOV_STRONG_RETRY=1}).
   */
  static final String SLOW = "SUPERCOV_STRONG_SLOW";
  static final String RETRY = "SUPERCOV_STRONG_RETRY";
  /**
   * Where the JVMs of a pass share their baselines, one line a test:
   * {@code index, passed, ms, unrepeatable}. A test's baseline is run once
   * for the whole pass, every round included, not once in each JVM: each
   * JVM runs a test in a fresh class loader and a working directory of its
   * own, so another JVM's baseline is what its own would be, and a kill is
   * still confirmed in the JVM that saw it.
   */
  static final String BASELINES = "SUPERCOV_STRONG_BASELINES";
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
    /** The module whose JVM runs it. */
    final String module;
    /** What the executor found for it (a unique id, a method). */
    public Object handle;
    Boolean baseline;
    long baselineMillis;
    /** Whether it failed when run a second time with no mutant. */
    boolean unrepeatable;

    Test(int index, String className, String name, long measuredMillis, String module) {
      this.index = index;
      this.className = className;
      this.name = name;
      this.measuredMillis = measuredMillis;
      this.module = module;
    }

    /**
     * The JavaScript drivers' two limits: first 1.5 times the longer of its
     * measured run and its baseline, and 1 s; a mutant past it is tried
     * again in a round of its own, with twice StrykerJS's limit (3 times,
     * and 10 s), before it is called a timeout.
     */
    long limitMillis(boolean generous) {
      long longer = Math.max(measuredMillis, baselineMillis);
      return generous ? longer * 3 + 10_000L : longer * 3 / 2 + 1_000L;
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
    /** The test's thread, where it ran past its time. */
    Thread worker;

    /** Records a failure, keeping the first one's reason. */
    public void fail(Throwable error) {
      failed = true;
      if (error != null && isMutantTimeout(error)) {
        mutantTimeout = true;
      }
      if (reason == null && error != null) {
        String message = String.valueOf(error);
        reason = message.length() > 300 ? message.substring(0, 300) : message;
      }
    }
  }

  /** A mutant's timeout thrown by any loader's copy of the runtime. */
  static boolean isMutantTimeout(Throwable error) {
    for (Throwable cause = error; cause != null; cause = cause.getCause()) {
      if (cause.getClass().getName().equals("com.supercorp.supercov.Supercov$MutantTimeoutError")) {
        return true;
      }
      if (cause.getCause() == cause) {
        break;
      }
    }
    return false;
  }

  private final Map<Integer, Test> tests = new HashMap<>();
  private final List<Mutant> mutants = new ArrayList<>();
  private final String results;
  /** Where mutants past the short limit are listed, and whether this is their retry. */
  private final File slow;
  private final boolean retry;
  /** A test thread that ran past the short limit and was left running. */
  private Thread abandoned;
  private final File claims;
  private final File baselines;
  private final String module;
  private final Executor executor;
  /** Where this JVM's time went, written as the pass's summary line. */
  private final Profile profile = new Profile();

  SupercovStrongPass(Executor executor, String module) throws IOException {
    this.executor = executor;
    this.module = module;
    this.results = System.getenv(RESULTS);
    String slowFile = System.getenv(SLOW);
    this.slow = slowFile == null ? null : new File(slowFile);
    this.retry = "1".equals(System.getenv(RETRY));
    String claimed = System.getenv(CLAIMS);
    this.claims = claimed == null ? null : new File(claimed);
    String shared = System.getenv(BASELINES);
    this.baselines = shared == null ? null : new File(shared);
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
              new Test(
                  index,
                  unescape(parts[2]),
                  unescape(parts[3]),
                  Long.parseLong(parts[4]),
                  parts.length > 5 ? unescape(parts[5]) : "."));
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
      for (File tree : trees) {
        removeTree(tree);
      }
      trees.clear();
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
    // Surefire enables assertions through its class loader, not `-ea`: a
    // minion asks for them on its command line instead.
    if (assertionsOn()) {
      command.add("-ea");
    }
    command.add("-D" + MINION + "=true");
    File cwd = new File(System.getProperty("user.dir")).getAbsoluteFile();
    File base = new File(System.getenv(CLAIMS)).getAbsoluteFile().getParentFile();
    for (int i = 1; i < jvms; i++) {
      List<String> own = new ArrayList<>(command);
      ProcessBuilder builder = new ProcessBuilder(own).inheritIO();
      // What it says on stderr, beside the pass's other files: a build may
      // keep its test JVM's output to itself.
      builder.redirectError(ProcessBuilder.Redirect.appendTo(new File(base, "jvm-" + i + ".log")));
      // A directory of its own: tests that write files where they run
      // (`new File("test.txt")`) would otherwise see the files another
      // JVM's test made or deleted meanwhile.
      String name = "jvm-" + Long.toHexString(System.nanoTime()) + "-" + i;
      File directory = new File(base, name);
      File temporary = new File(base, name + "-tmp");
      if (linkedTree(cwd, directory, base) && temporary.mkdirs()) {
        builder.directory(directory);
        own.add("-Duser.dir=" + directory.getPath());
        own.add("-Djava.io.tmpdir=" + temporary.getPath());
        trees.add(directory);
        trees.add(temporary);
      }
      own.add("-cp");
      own.add(System.getProperty("java.class.path"));
      own.add(framework);
      own.add(module());
      try {
        started.add(builder.start());
      } catch (IOException e) {
        System.err.println("supercov: could not start another JVM for the strong pass: " + e);
        break;
      }
    }
    return started;
  }

  /** The working directories made for the other JVMs, removed once they end. */
  private static final List<File> trees = new ArrayList<>();

  /** How many links a JVM's working directory is made of at most. */
  private static final int TREE_LINKS = 50_000;

  /**
   * {@code to} made as {@code from} is: its directories made afresh and
   * every file in them a link to the original, so a test reads the
   * project's files as they are and the files it makes or deletes are its
   * JVM's alone. Past {@link #TREE_LINKS} a directory is linked whole. False
   * where links cannot be made (Windows without the right): the JVM then
   * shares the directory, as before.
   */
  static boolean linkedTree(File from, File to, File skip) {
    try {
      int[] links = {0};
      java.nio.file.Files.createDirectories(skip.toPath());
      link(from.toPath(), to.toPath(), skip.toPath().toRealPath(), links);
      return true;
    } catch (IOException | UnsupportedOperationException | SecurityException e) {
      System.err.println(
          "supercov: the strong pass's JVMs share one working directory: " + e.getMessage());
      return false;
    }
  }

  private static void link(
      java.nio.file.Path from, java.nio.file.Path to, java.nio.file.Path skip, int[] links)
      throws IOException {
    java.nio.file.Files.createDirectories(to);
    try (java.nio.file.DirectoryStream<java.nio.file.Path> entries =
        java.nio.file.Files.newDirectoryStream(from)) {
      for (java.nio.file.Path entry : entries) {
        java.nio.file.Path target = to.resolve(entry.getFileName().toString());
        boolean directory =
            java.nio.file.Files.isDirectory(entry, java.nio.file.LinkOption.NOFOLLOW_LINKS);
        // The pass's own directory, where these trees are made.
        if (directory && entry.toRealPath().equals(skip)) {
          continue;
        }
        links[0]++;
        if (links[0] < TREE_LINKS && directory) {
          link(entry, target, skip, links);
        } else {
          java.nio.file.Files.createSymbolicLink(target, entry);
        }
      }
    }
  }

  /** Removes a working directory made of links, never what they point to. */
  static void removeTree(File tree) {
    try {
      java.nio.file.Files.walkFileTree(
          tree.toPath(),
          new java.nio.file.SimpleFileVisitor<java.nio.file.Path>() {
            @Override
            public java.nio.file.FileVisitResult visitFile(
                java.nio.file.Path file, java.nio.file.attribute.BasicFileAttributes attributes)
                throws IOException {
              java.nio.file.Files.delete(file);
              return java.nio.file.FileVisitResult.CONTINUE;
            }

            @Override
            public java.nio.file.FileVisitResult postVisitDirectory(
                java.nio.file.Path directory, IOException error) throws IOException {
              java.nio.file.Files.delete(directory);
              return java.nio.file.FileVisitResult.CONTINUE;
            }
          });
    } catch (IOException e) {
      // Left for the engine, which removes the pass's directory.
    }
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
    profile.begin();
    try {
      runMutants();
    } finally {
      profile.write(results, module);
    }
  }

  private void runMutants() throws IOException {
    for (Mutant mutant : mutants) {
      if (!mutant.modules.contains(module)) {
        continue;
      }
      List<Test> mine = new ArrayList<>();
      List<String> missing = new ArrayList<>();
      for (int candidate : mutant.candidates) {
        Test test = tests.get(candidate);
        if (test == null || !test.module.equals(module)) {
          continue;
        }
        if (executor.resolve(test)) {
          mine.add(test);
        } else {
          missing.add(test.name);
        }
      }
      if ((mine.isEmpty() && missing.isEmpty()) || !claim(mutant)) {
        continue;
      }
      if (!runMutant(mutant, mine, missing)) {
        // This JVM cannot be trusted with another mutant.
        System.err.println("supercov: this JVM takes no more mutants after " + mutant.id);
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
  private boolean runMutant(Mutant mutant, List<Test> candidates, List<String> missing)
      throws IOException {
    long started = System.nanoTime();
    profile.mutants++;
    ClassLoader mutated = fresh();
    int ran = 0;
    String unrepeatable = null;
    for (Test test : candidates) {
      if (test.baseline == null && sharedBaseline(test)) {
        profile.sharedBaselines++;
      }
      if (test.baseline == null) {
        long before = System.nanoTime();
        Outcome baseline = execute(test, false, -1, -1);
        test.baselineMillis = (System.nanoTime() - before) / 1_000_000L;
        profile.baselines++;
        profile.baselineNanos += System.nanoTime() - before;
        if (baseline.timedOut) {
          write(mutant, "error", null, ran, started,
              "its test " + test.name + " ran past its time without a mutant");
          return false;
        }
        test.baseline = !baseline.failed;
        if (test.baseline) {
          // Once more: a test that leaves what fails it the next time (a
          // file it makes and never deletes) cannot tell a mutant from its
          // own leftovers. It is no candidate, and says so.
          long second = System.nanoTime();
          Outcome again = execute(test, false, -1, -1);
          profile.baselines++;
          profile.baselineNanos += System.nanoTime() - second;
          if (again.failed || again.timedOut) {
            test.baseline = false;
            test.unrepeatable = true;
            System.err.println(
                "supercov: " + test.name + " fails when run a second time: " + again.reason);
          }
        }
        shareBaseline(test);
      }
      if (!test.baseline) {
        if (test.unrepeatable && unrepeatable == null) {
          unrepeatable = test.name;
        }
        continue;
      }
      ran++;
      long run = System.nanoTime();
      Outcome outcome = execute(test, true, mutant.file, mutant.index, mutated);
      profile.runs++;
      profile.runNanos += System.nanoTime() - run;
      if (ran == 1) {
        profile.firstRunNanos += System.nanoTime() - run;
      }
      if (outcome.timedOut && !retry && slow != null) {
        // Past the short limit: tried again alone, with the generous one,
        // in a round of its own. Its thread is left running; one such
        // thread at a time, or the JVM stops taking mutants.
        markSlow(mutant);
        boolean another = abandoned != null && abandoned.isAlive();
        abandoned = outcome.worker;
        return !another;
      }
      if (outcome.timedOut) {
        write(mutant, "timeout", test.name, ran, started, "ran past its time limit");
        return false;
      }
      if (outcome.mutantTimeout && !retry && slow != null) {
        // Past its evaluation limit once past the short time limit: tried
        // again in the round with the generous one, as a test past its time
        // is. Its thread ended with the error, so this JVM goes on.
        markSlow(mutant);
        return true;
      }
      if (outcome.mutantTimeout) {
        write(mutant, "timeout", test.name, ran, started, "the mutant passed its evaluation limit");
        return true;
      }
      if (outcome.failed) {
        long confirm = System.nanoTime();
        Outcome confirmation = execute(test, false, -1, -1);
        profile.confirmations++;
        profile.confirmNanos += System.nanoTime() - confirm;
        if (confirmation.failed || confirmation.timedOut) {
          // The mutant left the JVM so that the test fails without it: no
          // verdict here, and a fresh JVM for the rest.
          System.err.println(
              "supercov: " + test.name + " fails without the mutant too after it ran: "
                  + confirmation.reason);
          return false;
        }
        write(mutant, "killed", test.name, ran, started, outcome.reason);
        return true;
      }
    }
    if (!missing.isEmpty()) {
      // A candidate this JVM could not find may be the one that kills it.
      write(mutant, "error", null, ran, started,
          "its candidate test " + missing.get(0) + " was not found in the build's test JVM"
              + (missing.size() > 1 ? " (and " + (missing.size() - 1) + " more)" : ""));
      return true;
    }
    String reason = ran == 0 ? "no-passing-candidate" : null;
    if (unrepeatable != null) {
      reason = "its candidate " + unrepeatable + " fails when run a second time, so it was not run";
    }
    write(mutant, "survived", null, ran, started, reason);
    return true;
  }

  /**
   * Where this JVM's time went: from its start to the pass's, the
   * baselines, the runs with a mutant (the first test of each, in a loader
   * of its own, apart), the confirmations, the classes loaded and the time
   * the JIT took. One JSON line beside the verdicts, {@code summary} set.
   */
  static final class Profile {
    long began;
    long uptimeAtBegin;
    long classesAtBegin;
    long compileAtBegin;
    int mutants;
    int baselines;
    int sharedBaselines;
    long baselineNanos;
    int runs;
    long runNanos;
    long firstRunNanos;
    int confirmations;
    long confirmNanos;

    void begin() {
      began = System.nanoTime();
      try {
        uptimeAtBegin = java.lang.management.ManagementFactory.getRuntimeMXBean().getUptime();
        classesAtBegin =
            java.lang.management.ManagementFactory.getClassLoadingMXBean().getTotalLoadedClassCount();
        compileAtBegin = compileMillis();
      } catch (Throwable e) {
        // No management API: those fields stay 0.
      }
    }

    private static long compileMillis() {
      java.lang.management.CompilationMXBean compiler =
          java.lang.management.ManagementFactory.getCompilationMXBean();
      return compiler != null && compiler.isCompilationTimeMonitoringSupported()
          ? compiler.getTotalCompilationTime()
          : 0;
    }

    void write(String results, String module) {
      if (results == null) {
        return;
      }
      long classes = 0;
      long compile = 0;
      try {
        classes =
            java.lang.management.ManagementFactory.getClassLoadingMXBean().getTotalLoadedClassCount()
                - classesAtBegin;
        compile = compileMillis() - compileAtBegin;
      } catch (Throwable e) {
        // No management API.
      }
      StringBuilder line = new StringBuilder("{\"summary\":true,\"driver\":\"jvm\",\"module\":");
      json(line, module);
      line.append(",\"minion\":").append(Boolean.getBoolean(MINION));
      line.append(",\"jvmStartMs\":").append(uptimeAtBegin);
      line.append(",\"passMs\":").append((System.nanoTime() - began) / 1_000_000L);
      line.append(",\"mutants\":").append(mutants);
      line.append(",\"baselines\":").append(baselines);
      line.append(",\"baselineMs\":").append(baselineNanos / 1_000_000L);
      line.append(",\"sharedBaselines\":").append(sharedBaselines);
      line.append(",\"runs\":").append(runs);
      line.append(",\"runMs\":").append(runNanos / 1_000_000L);
      line.append(",\"firstRunMs\":").append(firstRunNanos / 1_000_000L);
      line.append(",\"confirmations\":").append(confirmations);
      line.append(",\"confirmMs\":").append(confirmNanos / 1_000_000L);
      line.append(",\"classesLoaded\":").append(classes);
      line.append(",\"jitMs\":").append(compile);
      line.append("}\n");
      try (FileOutputStream out = new FileOutputStream(results, true)) {
        out.write(line.toString().getBytes(StandardCharsets.UTF_8));
      } catch (IOException e) {
        // The profile is for reading, not for verdicts.
      }
    }
  }

  /** Takes another JVM's baseline of {@code test}, where one ran: whether it did. */
  private boolean sharedBaseline(Test test) {
    if (baselines == null || !baselines.isFile()) {
      return false;
    }
    String prefix = test.index + "\t";
    try (BufferedReader reader =
        new BufferedReader(
            new InputStreamReader(new FileInputStream(baselines), StandardCharsets.UTF_8))) {
      String line;
      while ((line = reader.readLine()) != null) {
        String[] parts = line.split("\t", -1);
        // A line cut short by a JVM still writing it says nothing yet.
        if (!line.startsWith(prefix) || parts.length < 4) {
          continue;
        }
        test.baselineMillis = Long.parseLong(parts[2]);
        test.unrepeatable = parts[3].equals("1");
        test.baseline = parts[1].equals("1");
        return true;
      }
    } catch (IOException | NumberFormatException e) {
      // Run it here instead.
    }
    return false;
  }

  /** Gives this JVM's baseline of {@code test} to the others, in one write. */
  private void shareBaseline(Test test) {
    if (baselines == null) {
      return;
    }
    String line =
        test.index + "\t" + (test.baseline ? "1" : "0") + "\t" + test.baselineMillis + "\t"
            + (test.unrepeatable ? "1" : "0") + "\n";
    try (FileOutputStream out = new FileOutputStream(baselines, true)) {
      out.write(line.getBytes(StandardCharsets.UTF_8));
    } catch (IOException e) {
      // The others run it themselves.
    }
  }

  /** Lists the mutant for the retry round. */
  private void markSlow(Mutant mutant) {
    try (Writer out =
        new OutputStreamWriter(new FileOutputStream(slow, true), StandardCharsets.UTF_8)) {
      out.write(mutant.id + "\n");
    } catch (IOException e) {
      // Not listed: the next round runs it with the short limit again.
    }
  }

  private Outcome execute(final Test test, boolean mutated, int file, int index) {
    return execute(test, mutated, file, index, fresh());
  }

  /**
   * Runs one test in {@code loader}, the mutant on in that loader's copy of
   * the runtime. A loader of its own for each mutant gives the project's
   * classes fresh static state, as a fresh JVM would: a cache a test filled
   * without the mutant (or with another) no longer hides the mutant's code.
   */
  private Outcome execute(
      final Test test, boolean mutated, int file, int index, final ClassLoader loader) {
    final Outcome outcome = new Outcome();
    Switches switches = Switches.of(loader);
    if (mutated) {
      switches.on(file, index, (int) Math.min(Integer.MAX_VALUE, test.limitMillis(retry)));
    } else {
      switches.off();
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
    worker.setContextClassLoader(loader);
    worker.start();
    try {
      // A baseline gets a minute more than the generous limit: nothing has
      // measured it yet.
      worker.join(mutated ? test.limitMillis(retry) : test.limitMillis(true) + 60_000L);
    } catch (InterruptedException e) {
      Thread.currentThread().interrupt();
    }
    if (worker.isAlive()) {
      outcome.timedOut = true;
      outcome.worker = worker;
    }
    if (mutated && switches.limitReached()) {
      outcome.mutantTimeout = true;
    }
    switches.off();
    return outcome;
  }

  /** A loader's copy of the runtime's switches, reached reflectively. */
  static final class Switches {
    private final Class<?> runtime;

    private Switches(Class<?> runtime) {
      this.runtime = runtime;
    }

    static Switches of(ClassLoader loader) {
      try {
        return new Switches(Class.forName("com.supercorp.supercov.Supercov", true, loader));
      } catch (ClassNotFoundException e) {
        return new Switches(Supercov.class);
      }
    }

    // The JVM's own copy is switched too: code loaded from a jar (another
    // module's, packaged) is not reloaded, and calls that copy.
    void on(int file, int index, int budgetMillis) {
      call("setActiveMutant", file, index, budgetMillis);
      if (runtime != Supercov.class) {
        Supercov.setActiveMutant(file, index, budgetMillis);
      }
    }

    void off() {
      call("clearActiveMutant");
      if (runtime != Supercov.class) {
        Supercov.clearActiveMutant();
      }
    }

    boolean limitReached() {
      Object reached = call("mutantHitLimitReached");
      return Boolean.TRUE.equals(reached) || Supercov.mutantHitLimitReached();
    }

    private Object call(String name, int... arguments) {
      try {
        Class<?>[] types = new Class<?>[arguments.length];
        Object[] values = new Object[arguments.length];
        for (int i = 0; i < arguments.length; i++) {
          types[i] = int.class;
          values[i] = arguments[i];
        }
        return runtime.getMethod(name, types).invoke(null, values);
      } catch (ReflectiveOperationException e) {
        throw new IllegalStateException(e);
      }
    }
  }

  /** The directories the project's own classes load from: reloaded per mutant. */
  private static java.net.URL[] roots;

  private static synchronized java.net.URL[] roots() {
    if (roots != null) {
      return roots;
    }
    // Every directory on the class path: the project's compiled classes
    // (Gradle keeps Java's and Kotlin's apart, and a reactor's other
    // modules are directories too); jars are libraries, loaded once. A
    // surefire fork's class path is the manifest of its booter jar.
    List<java.net.URL> found = new ArrayList<>();
    String path = System.getProperty("java.class.path", "");
    for (String entry : path.split(java.io.File.pathSeparator)) {
      if (entry.isEmpty()) {
        continue;
      }
      File file = new File(entry);
      if (file.isDirectory()) {
        add(found, file);
      } else if (file.getName().startsWith("surefirebooter") && file.isFile()) {
        try (java.util.jar.JarFile jar = new java.util.jar.JarFile(file)) {
          java.util.jar.Manifest manifest = jar.getManifest();
          String classPath =
              manifest == null ? null : manifest.getMainAttributes().getValue("Class-Path");
          if (classPath != null) {
            for (String url : classPath.trim().split("\\s+")) {
              try {
                File listed = new File(new java.net.URI(url));
                if (listed.isDirectory()) {
                  add(found, listed);
                }
              } catch (java.net.URISyntaxException | IllegalArgumentException e) {
                // Not a file URL.
              }
            }
          }
        } catch (IOException e) {
          // No manifest to read.
        }
      }
    }
    // Where the runtime and the configuration were loaded from, whatever
    // the class path says.
    for (String name :
        new String[] {"com.supercorp.supercov.Supercov", "com.supercorp.supercov.SupercovConfig"}) {
      try {
        java.net.URL location =
            Class.forName(name, false, SupercovStrongPass.class.getClassLoader())
                .getProtectionDomain()
                .getCodeSource()
                .getLocation();
        if (location != null) {
          File file = new File(location.toURI());
          if (file.isDirectory()) {
            add(found, file);
          }
        }
      } catch (ClassNotFoundException | SecurityException | NullPointerException
          | java.net.URISyntaxException | IllegalArgumentException e) {
        // That half of the project has no root of its own here.
      }
    }
    roots = found.toArray(new java.net.URL[0]);
    return roots;
  }

  private static void add(List<java.net.URL> found, File directory) {
    try {
      java.net.URL url = directory.getAbsoluteFile().toURI().toURL();
      if (!found.contains(url)) {
        found.add(url);
      }
    } catch (java.net.MalformedURLException e) {
      // A directory always has a URL.
    }
  }

  /** A loader that loads the project's classes afresh, and the rest as the JVM does. */
  static ClassLoader fresh() {
    return new Fresh(roots(), SupercovStrongPass.class.getClassLoader());
  }

  /**
   * Whether the project's classes run with assertions: Gradle passes `-ea`,
   * Surefire sets them on its class loader, which a new loader does not
   * inherit, and Java `assert` statements are how many tests check.
   */
  static boolean assertionsOn() {
    return SupercovStrongPass.class.desiredAssertionStatus();
  }

  private static final class Fresh extends java.net.URLClassLoader {
    Fresh(java.net.URL[] roots, ClassLoader parent) {
      super(roots, parent);
      setDefaultAssertionStatus(assertionsOn());
    }

    @Override
    protected Class<?> loadClass(String name, boolean resolve) throws ClassNotFoundException {
      synchronized (getClassLoadingLock(name)) {
        Class<?> loaded = findLoadedClass(name);
        // The drivers stay the JVM's own: they hold the pass.
        if (loaded == null && !name.startsWith("com.supercorp.supercov.SupercovStrong")) {
          try {
            loaded = findClass(name);
          } catch (ClassNotFoundException notTheProjects) {
            loaded = null;
          }
        }
        if (loaded == null) {
          return super.loadClass(name, resolve);
        }
        if (resolve) {
          resolveClass(loaded);
        }
        return loaded;
      }
    }
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
