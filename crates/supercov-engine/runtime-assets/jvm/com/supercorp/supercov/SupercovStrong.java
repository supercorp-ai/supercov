package com.supercorp.supercov;

import java.util.HashMap;
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
      test.handle = byName.get(test.name);
      if (test.handle == null) {
        // An invocation's display name prints its arguments, which may
        // print differently in another JVM (an identity hash, a time): its
        // index says the same invocation.
        test.handle = byName.get(invocationKey(test.name));
      }
      return test.handle != null;
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
      }
      for (TestIdentifier child : plan.getChildren(identifier)) {
        collect(plan, child, byName);
      }
    }

    @Override
    public void execute(SupercovStrongPass.Test test, final SupercovStrongPass.Outcome outcome) {
      LauncherDiscoveryRequest request =
          LauncherDiscoveryRequestBuilder.request()
              .selectors(DiscoverySelectors.selectUniqueId((String) test.handle))
              .build();
      launcher.execute(
          request,
          new TestExecutionListener() {
            @Override
            public void executionFinished(TestIdentifier identifier, TestExecutionResult result) {
              if (result.getStatus() == TestExecutionResult.Status.FAILED) {
                outcome.fail(result.getThrowable().orElse(null));
              }
            }
          });
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
