package com.supercorp.supercov;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.platform.engine.FilterResult;
import org.junit.platform.engine.TestDescriptor;
import org.junit.platform.engine.TestExecutionResult;
import org.junit.platform.engine.discovery.DiscoverySelectors;
import org.junit.platform.engine.support.descriptor.ClassSource;
import org.junit.platform.launcher.Launcher;
import org.junit.platform.launcher.LauncherDiscoveryRequest;
import org.junit.platform.launcher.PostDiscoveryFilter;
import org.junit.platform.launcher.TestExecutionListener;
import org.junit.platform.launcher.TestIdentifier;
import org.junit.platform.launcher.TestPlan;
import org.junit.platform.launcher.core.LauncherDiscoveryRequestBuilder;
import org.junit.platform.launcher.core.LauncherFactory;

/**
 * The strong pass inside the build's own test JVM, as PIT's minion runs it:
 * the build compiles the strong copy once and starts its test JVM, and this
 * runs each mutant of the plan against its candidate tests in that JVM,
 * switching mutants between test executions.
 *
 * <p>It is a discovery filter and a listener, registered through the
 * platform's service files in the strong copy. The filter leaves the build's
 * own discovery empty, so the build runs none of its tests; the listener,
 * told the empty plan starts, runs the pass with a launcher of its own,
 * selecting each test by its unique id, one test at a time.
 *
 * <p>A test is run once without a mutant before it is run with one (its
 * baseline), and a test that fails then is no candidate. A kill is confirmed
 * by running the killing test again without the mutant: a JVM whose state a
 * mutant changed for good (a static cache filled with a mutated value) fails
 * it too, and the pass stops so that the engine starts a fresh JVM for the
 * rest. A test past its time limit leaves a thread that will not stop: the
 * verdict is written and the JVM halts, for the same reason.
 *
 * <p>The plan is {@code SUPERCOV_STRONG_JVM_PLAN}, written by the engine:
 * {@code T} lines name tests ({@code T, index, class, name, limit ms}),
 * {@code M} lines mutants ({@code M, row, id, file slot, index, modules,
 * candidates}). Verdicts are appended to {@code SUPERCOV_STRONG_RESULTS} as
 * JSON lines, one write per line.
 */
public final class SupercovStrong implements PostDiscoveryFilter, TestExecutionListener {

  private static final String PLAN = "SUPERCOV_STRONG_JVM_PLAN";
  private static final String RESULTS = "SUPERCOV_STRONG_RESULTS";
  private static final String CLAIMS = "SUPERCOV_STRONG_CLAIMS";

  /** Set while the pass's own launcher runs: its discovery is left alone. */
  private static volatile boolean inPass = false;

  private static boolean ran = false;

  private static String module() {
    try {
      return (String) Class.forName("com.supercorp.supercov.SupercovConfig").getField("MODULE").get(null);
    } catch (ReflectiveOperationException | ClassCastException e) {
      return ".";
    }
  }

  @Override
  public FilterResult apply(TestDescriptor descriptor) {
    if (System.getenv(PLAN) == null || inPass) {
      return FilterResult.included("supercov");
    }
    return FilterResult.excluded("the strong pass runs the tests itself");
  }

  @Override
  public void testPlanExecutionStarted(TestPlan plan) {
    synchronized (SupercovStrong.class) {
      if (System.getenv(PLAN) == null || inPass || ran) {
        return;
      }
      ran = true;
    }
    inPass = true;
    try {
      new Pass(System.getenv(PLAN), System.getenv(RESULTS), System.getenv(CLAIMS)).run();
    } catch (Throwable error) {
      System.err.println("supercov: the strong pass stopped: " + error);
      error.printStackTrace();
    } finally {
      Supercov.clearActiveMutant();
      inPass = false;
    }
  }

  static final class Test {
    final int index;
    final String className;
    final String name;
    final long limitMillis;
    String uniqueId;
    Boolean baseline;

    Test(int index, String className, String name, long limitMillis) {
      this.index = index;
      this.className = className;
      this.name = name;
      this.limitMillis = limitMillis;
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
  static final class Outcome {
    boolean failed;
    boolean timedOut;
    boolean mutantTimeout;
    String reason;
  }

  static final class Pass {
    private final Map<Integer, Test> tests = new HashMap<>();
    private final List<Mutant> mutants = new ArrayList<>();
    private final String results;
    private final File claims;
    private final String module = module();
    private final Map<String, Map<String, String>> discovered = new HashMap<>();
    private final Launcher launcher = LauncherFactory.create();

    Pass(String plan, String results, String claims) throws IOException {
      this.results = results;
      this.claims = claims == null ? null : new File(claims);
      try (BufferedReader reader =
          new BufferedReader(new InputStreamReader(new FileInputStream(plan), StandardCharsets.UTF_8))) {
        String line;
        while ((line = reader.readLine()) != null) {
          String[] parts = line.split("\t", -1);
          if (parts[0].equals("T") && parts.length >= 5) {
            int index = Integer.parseInt(parts[1]);
            tests.put(index, new Test(index, parts[2], unescape(parts[3]), Long.parseLong(parts[4])));
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

    void run() throws IOException {
      for (Mutant mutant : mutants) {
        if (!mutant.modules.contains(module)) {
          continue;
        }
        List<Test> mine = new ArrayList<>();
        for (int candidate : mutant.candidates) {
          Test test = tests.get(candidate);
          if (test != null && resolve(test)) {
            mine.add(test);
          }
        }
        if (mine.isEmpty() || !claim(mutant)) {
          continue;
        }
        if (!runMutant(mutant, mine)) {
          // The JVM cannot be trusted with another mutant.
          return;
        }
      }
    }

    /** Whether this JVM took the mutant; another fork of the module may have. */
    private boolean claim(Mutant mutant) {
      if (claims == null) {
        return true;
      }
      try {
        claims.mkdirs();
        return new File(claims, safe(module) + "-" + mutant.row).createNewFile();
      } catch (IOException e) {
        return true;
      }
    }

    private static String safe(String text) {
      return text.replaceAll("[^A-Za-z0-9]", "_");
    }

    /** Finds the test's unique id: false where its class is not this module's. */
    private boolean resolve(Test test) {
      if (test.uniqueId != null) {
        return true;
      }
      Map<String, String> byName = discovered.get(test.className);
      if (byName == null) {
        byName = new HashMap<>();
        discovered.put(test.className, byName);
        try {
          Class.forName(test.className, false, Thread.currentThread().getContextClassLoader());
          LauncherDiscoveryRequest request =
              LauncherDiscoveryRequestBuilder.request()
                  .selectors(DiscoverySelectors.selectClass(test.className))
                  .build();
          TestPlan plan = launcher.discover(request);
          for (TestIdentifier root : plan.getRoots()) {
            collect(plan, root, byName);
          }
        } catch (ClassNotFoundException | LinkageError e) {
          // Another module's test.
        }
      }
      test.uniqueId = byName.get(test.name);
      return test.uniqueId != null;
    }

    private void collect(TestPlan plan, TestIdentifier identifier, Map<String, String> byName) {
      if (identifier.isTest()) {
        byName.put(name(plan, identifier), identifier.getUniqueId());
      }
      for (TestIdentifier child : plan.getChildren(identifier)) {
        collect(plan, child, byName);
      }
    }

    /** Runs one mutant; false when the JVM must stop after it. */
    private boolean runMutant(Mutant mutant, List<Test> candidates) throws IOException {
      long started = System.nanoTime();
      int ran = 0;
      for (Test test : candidates) {
        if (test.baseline == null) {
          Outcome baseline = execute(test, false, -1, -1);
          if (baseline.timedOut) {
            write(mutant, "error", null, ran, started, "its test " + test.name + " ran past its time without a mutant");
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
          continue;
        }
        if (outcome.failed) {
          Outcome confirmation = execute(test, false, -1, -1);
          if (confirmation.failed || confirmation.timedOut) {
            // The mutant left the JVM so that the test fails without it:
            // no verdict here, and a fresh JVM for the rest.
            return false;
          }
          write(mutant, "killed", test.name, ran, started, outcome.reason);
          return true;
        }
      }
      if (ran == 0) {
        write(mutant, "survived", null, 0, started, "no-passing-candidate");
      } else {
        write(mutant, "survived", null, ran, started, null);
      }
      return true;
    }

    private Outcome execute(final Test test, boolean mutated, int file, int index) {
      final Outcome outcome = new Outcome();
      final LauncherDiscoveryRequest request =
          LauncherDiscoveryRequestBuilder.request()
              .selectors(DiscoverySelectors.selectUniqueId(test.uniqueId))
              .build();
      final TestExecutionListener listener =
          new TestExecutionListener() {
            @Override
            public void executionFinished(TestIdentifier identifier, TestExecutionResult result) {
              if (result.getStatus() == TestExecutionResult.Status.FAILED) {
                outcome.failed = true;
                Throwable error = result.getThrowable().orElse(null);
                if (error != null && Supercov.isMutantTimeout(error)) {
                  outcome.mutantTimeout = true;
                }
                if (outcome.reason == null && error != null) {
                  String message = String.valueOf(error);
                  outcome.reason = message.length() > 300 ? message.substring(0, 300) : message;
                }
              }
            }
          };
      if (mutated) {
        Supercov.setActiveMutant(file, index);
      } else {
        Supercov.clearActiveMutant();
      }
      final Throwable[] escaped = new Throwable[1];
      Thread worker =
          new Thread(
              () -> {
                try {
                  launcher.execute(request, listener);
                } catch (Throwable error) {
                  escaped[0] = error;
                }
              },
              "supercov-strong-" + test.index);
      worker.setDaemon(true);
      worker.setContextClassLoader(Thread.currentThread().getContextClassLoader());
      worker.start();
      try {
        worker.join(test.limitMillis);
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
      if (escaped[0] != null) {
        outcome.failed = true;
        if (Supercov.isMutantTimeout(escaped[0])) {
          outcome.mutantTimeout = true;
        }
      }
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

  // --- Names, exactly as SupercovListener writes them --------------------------

  static String name(TestPlan plan, TestIdentifier identifier) {
    StringBuilder qualified = new StringBuilder(invocation(identifier));
    TestIdentifier current = identifier;
    while (plan != null) {
      TestIdentifier parent = plan.getParent(current).orElse(null);
      if (parent == null || !plan.getParent(parent).isPresent()) {
        break;
      }
      qualified.insert(0, containerName(parent) + "#");
      current = parent;
    }
    return Supercov.printable(qualified.toString());
  }

  private static String invocation(TestIdentifier identifier) {
    String display = identifier.getDisplayName();
    String id = identifier.getUniqueId();
    int segment = id.lastIndexOf("/[");
    if (segment < 0) {
      return display;
    }
    String last = id.substring(segment + 2, id.length() - (id.endsWith("]") ? 1 : 0));
    int colon = last.indexOf(":#");
    if (colon < 0) {
      return display;
    }
    String index = "[" + last.substring(colon + 2) + "]";
    return display.startsWith(index) ? display : index + " " + display;
  }

  private static String containerName(TestIdentifier identifier) {
    return identifier
        .getSource()
        .filter(source -> source instanceof ClassSource)
        .map(source -> ((ClassSource) source).getClassName())
        .orElseGet(identifier::getDisplayName);
  }

  @SuppressWarnings("unused")
  private static <K, V> Map<K, V> ordered() {
    return Collections.synchronizedMap(new LinkedHashMap<K, V>());
  }
}
