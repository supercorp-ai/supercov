package com.supercorp.supercov;

import java.util.HashMap;
import java.util.Map;
import java.util.regex.Pattern;
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
 * The strong pass's driver on the JUnit Platform (`SupercovStrongPass`):
 * every engine on the platform, Jupiter, Vintage's JUnit 4, Kotest and
 * Spock among them, runs through it.
 *
 * <p>It is a discovery filter and a listener, registered through the
 * platform's service files in the strong copy. The filter leaves the build's
 * own discovery empty, so the build runs none of its tests; the listener,
 * told the empty plan starts, runs the pass with a launcher of its own,
 * selecting each test by its unique id, one test at a time.
 */
public final class SupercovStrong implements PostDiscoveryFilter, TestExecutionListener {

  /** Set while the pass's own launcher runs: its discovery is left alone. */
  private static volatile boolean inPass = false;

  @Override
  public FilterResult apply(TestDescriptor descriptor) {
    if (System.getenv(SupercovStrongPass.PLAN) == null || inPass) {
      return FilterResult.included("supercov");
    }
    return FilterResult.excluded("the strong pass runs the tests itself");
  }

  @Override
  public void testPlanExecutionStarted(TestPlan plan) {
    if (inPass || !SupercovStrongPass.shouldStart()) {
      return;
    }
    inPass = true;
    try {
      SupercovStrongPass.runEverywhere(new Platform(), SupercovStrong.class.getName());
    } finally {
      inPass = false;
    }
  }

  /** A JVM that shares the pass. */
  public static void main(String[] args) {
    inPass = true;
    SupercovStrongPass.minion(new Platform(), args);
  }

  /** Runs a test by the unique id its name has in its class's plan. */
  static final class Platform implements SupercovStrongPass.Executor {
    private final Launcher launcher = LauncherFactory.create();
    private final Map<String, Map<String, String>> discovered = new HashMap<>();

    @Override
    public boolean resolve(SupercovStrongPass.Test test) {
      if (test.handle != null) {
        return true;
      }
      Map<String, String> byName = discovered.get(test.className);
      if (byName == null) {
        byName = new HashMap<>();
        discovered.put(test.className, byName);
        ClassLoader saved = Thread.currentThread().getContextClassLoader();
        ClassLoader loader = SupercovStrongPass.fresh();
        try {
          Thread.currentThread().setContextClassLoader(loader);
          Class.forName(test.className, false, loader);
          LauncherDiscoveryRequest request =
              LauncherDiscoveryRequestBuilder.request()
                  .selectors(DiscoverySelectors.selectClass(test.className))
                  .build();
          TestPlan plan = launcher.discover(request);
          for (TestIdentifier root : plan.getRoots()) {
            collect(plan, root, byName);
            if (!plan.getChildren(root).isEmpty()) {
              byName.put(CLASS_FOUND, "");
            }
          }
        } catch (ClassNotFoundException | LinkageError e) {
          // Another module's test.
        } finally {
          Thread.currentThread().setContextClassLoader(saved);
        }
      }
      test.handle = byName.get(test.name);
      if (test.handle == null) {
        // An invocation's display name prints its arguments, which may
        // print differently in another JVM (an identity hash, a time): its
        // index says the same invocation.
        test.handle = byName.get(invocationKey(test.name));
      }
      if (test.handle == null) {
        // A JUnit 4 parameter printed by Object's toString names its
        // identity hash, which differs from JVM to JVM and from one
        // discovery to the next, unique id and all: the name with every
        // such hash left out says the same test, unless two of the class's
        // tests read alike without them. It is run by its class, filtered
        // to that name.
        String key = stableKey(test.name);
        String stable = byName.get(key);
        if (stable != null && !AMBIGUOUS.equals(stable)) {
          test.handle = new String[] {test.className, key};
        }
      }
      if (test.handle == null) {
        // A parameterized or dynamic test's invocations are not in a
        // discovered plan: they appear as it runs. The template is, and an
        // invocation is selected by the template's id and its index.
        test.handle = invocationId(test.name, byName);
      }
      if (test.handle == null && byName.containsKey(CLASS_FOUND)) {
        // An engine that registers its tests only as it runs them (Kotest's
        // specs, a Spock feature's iterations): the class runs, and the
        // test is the one of that name among those it registered.
        test.handle = new Object[] {test.className, test.name};
      }
      return test.handle != null;
    }

    /** `Class#template(..)#[3] display`: the template's id with invocation #3. */
    static String invocationId(String name, Map<String, String> byName) {
      int open = name.lastIndexOf("#[");
      if (open < 0) {
        return null;
      }
      int close = name.indexOf(']', open);
      if (close < 0) {
        return null;
      }
      String index = name.substring(open + 2, close);
      for (int i = 0; i < index.length(); i++) {
        if (!Character.isDigit(index.charAt(i))) {
          return null;
        }
      }
      String template = byName.get("\u0001" + name.substring(0, open));
      if (template == null) {
        return null;
      }
      String segment = template.contains("[test-factory:") ? "dynamic-test" : "test-template-invocation";
      return template + "/[" + segment + ":#" + index + "]";
    }

    /** Marks a class the platform discovered, its tests or not. */
    static final String CLASS_FOUND = "\u0005";

    /** A name that two tests share once their identity hashes are left out. */
    static final String AMBIGUOUS = "\u0003";

    private static final Pattern IDENTITY_HASH = Pattern.compile("([\\w$])@[0-9a-f]{1,8}(?![\\w$])");

    /** A name with each identity hash (`Builder@6c0d7c83`) left out. */
    static String stableKey(String name) {
      String stable = IDENTITY_HASH.matcher(name).replaceAll("$1@");
      return stable.equals(name) ? null : "\u0002" + stable;
    }

    /** A name cut after its last invocation index: `Class#method(..)#[3]`. */
    static String invocationKey(String name) {
      int open = name.lastIndexOf("#[");
      if (open < 0) {
        return null;
      }
      int close = name.indexOf(']', open);
      return close < 0 ? null : "\u0000" + name.substring(0, close + 1);
    }

    private void collect(TestPlan plan, TestIdentifier identifier, Map<String, String> byName) {
      if (identifier.isTest()) {
        String name = name(plan, identifier);
        byName.put(name, identifier.getUniqueId());
        String key = invocationKey(name);
        if (key != null) {
          byName.put(key, identifier.getUniqueId());
        }
        String stable = stableKey(name);
        if (stable != null) {
          byName.put(stable, byName.containsKey(stable) ? AMBIGUOUS : identifier.getUniqueId());
        }
      } else if (plan.getParent(identifier).isPresent()) {
        // A container: a template whose invocations appear as it runs.
        byName.put("\u0001" + name(plan, identifier), identifier.getUniqueId());
      }
      for (TestIdentifier child : plan.getChildren(identifier)) {
        collect(plan, child, byName);
      }
    }

    @Override
    public void execute(SupercovStrongPass.Test test, final SupercovStrongPass.Outcome outcome) {
      if (test.handle instanceof Object[] && !(test.handle instanceof String[])) {
        executeByClass(test, outcome);
        return;
      }
      LauncherDiscoveryRequest request;
      if (test.handle instanceof String[]) {
        final String key = ((String[]) test.handle)[1];
        request =
            LauncherDiscoveryRequestBuilder.request()
                .selectors(DiscoverySelectors.selectClass(((String[]) test.handle)[0]))
                .filters(
                    (PostDiscoveryFilter)
                        descriptor ->
                            !descriptor.isTest() || key.equals(stableKey(name(descriptor)))
                                ? FilterResult.included("the test")
                                : FilterResult.excluded("another test"))
                .build();
      } else {
        request =
            LauncherDiscoveryRequestBuilder.request()
                .selectors(DiscoverySelectors.selectUniqueId((String) test.handle))
                .build();
      }
      final boolean[] ran = {false};
      launcher.execute(
          request,
          new TestExecutionListener() {
            @Override
            public void executionFinished(TestIdentifier identifier, TestExecutionResult result) {
              if (identifier.isTest()) {
                ran[0] = true;
              }
              if (result.getStatus() == TestExecutionResult.Status.FAILED) {
                outcome.fail(result.getThrowable().orElse(null));
              }
            }
          });
      if (!ran[0] && test.handle instanceof String[] && !outcome.failed) {
        outcome.failed = true;
        outcome.reason = "the test did not run alone";
      }
    }

    /**
     * Runs the test's class and reads the outcome of the test of its name, as
     * the engine registers it during the run: the class's other tests run
     * too, and their outcomes say nothing of this one.
     */
    private void executeByClass(
        SupercovStrongPass.Test test, final SupercovStrongPass.Outcome outcome) {
      final Object[] handle = (Object[]) test.handle;
      final String wanted = (String) handle[1];
      final String wantedKey = stableKey(wanted);
      final TestPlan[] running = {null};
      final boolean[] ran = {false};
      launcher.execute(
          LauncherDiscoveryRequestBuilder.request()
              .selectors(DiscoverySelectors.selectClass((String) handle[0]))
              .build(),
          new TestExecutionListener() {
            @Override
            public void testPlanExecutionStarted(TestPlan plan) {
              running[0] = plan;
            }

            @Override
            public void executionFinished(TestIdentifier identifier, TestExecutionResult result) {
              if (!identifier.isTest()) {
                return;
              }
              String name = name(running[0], identifier);
              if (!name.equals(wanted)
                  && (wantedKey == null || !wantedKey.equals(stableKey(name)))) {
                return;
              }
              ran[0] = true;
              if (result.getStatus() == TestExecutionResult.Status.FAILED) {
                outcome.fail(result.getThrowable().orElse(null));
              }
            }
          });
      if (!ran[0] && !outcome.failed) {
        outcome.failed = true;
        outcome.reason = "the test did not run with its class";
      }
    }
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

  /** A discovered descriptor's name, as `name` gives its identifier's. */
  static String name(TestDescriptor descriptor) {
    StringBuilder qualified = new StringBuilder(invocation(TestIdentifier.from(descriptor)));
    TestDescriptor current = descriptor;
    while (true) {
      TestDescriptor parent = current.getParent().orElse(null);
      if (parent == null || !parent.getParent().isPresent()) {
        break;
      }
      qualified.insert(0, containerName(TestIdentifier.from(parent)) + "#");
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
}
