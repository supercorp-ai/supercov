package com.supercorp.supercov;

import java.io.OutputStream;
import junit.framework.AssertionFailedError;
import junit.framework.Test;
import org.apache.tools.ant.taskdefs.optional.junit.IgnoredTestListener;
import org.apache.tools.ant.taskdefs.optional.junit.JUnitResultFormatter;
import org.apache.tools.ant.taskdefs.optional.junit.JUnitTest;
import org.apache.tools.ant.taskdefs.optional.junit.JUnitVersionHelper;

/**
 * Binds tests run by Ant's {@code <junit>} task to their evidence.
 *
 * <p>That task runs JUnit 4 itself rather than on the JUnit Platform, so the
 * platform listener never hears of a test. What it offers instead is the
 * result formatter, which it loads in every JVM it forks and tells about each
 * test as it starts and ends. Supercov adds one to each {@code <junit>} task
 * of the copy's build, next to the project's own formatters.
 *
 * <p>Compiled by the copy's build, just before the task runs, against the
 * classpath the task itself is given: it needs Ant's formatter interface and
 * JUnit's test type, and no other compilation of the project has both.
 */
public final class SupercovAntFormatter implements JUnitResultFormatter, IgnoredTestListener {

  private static final String CONFIG = "com.supercorp.supercov.SupercovConfig";

  /** The runner this formatter speaks for, as the frontend declares it. */
  private static final String RUNNER = "ant-junit";

  private static String evidenceDirectory;
  private static boolean armed;
  private static boolean hooked;
  /** A test this JVM entered and has not ended: the one a timeout stopped. */
  private static boolean testOpen;

  /**
   * Whether the task ends a JVM past its timeout without running its shutdown
   * hooks. On Windows {@code Process.destroy} is {@code TerminateProcess}: the
   * hook never runs, and the JVM's whole evidence would be lost with it.
   */
  private static final boolean KILLED_WITHOUT_HOOKS =
      System.getProperty("os.name", "").startsWith("Windows");

  /**
   * The test Ant reports for a forked JVM that ended early -- past its
   * timeout, or crashed -- from its own JVM, under the name of the test that
   * was running. That test's record is the forked JVM's, which closes it on
   * shutdown, so this one records nothing.
   */
  private static final String VM_EXIT_TEST =
      "org.apache.tools.ant.taskdefs.optional.junit.JUnitTaskMirrorImpl$VmExitErrorTest";

  /** How the open test is ending, once something says it is not passing. */
  private String status;
  /** The class the task ran, which a suite runs other classes' tests from. */
  private String suite;
  private boolean open;

  /**
   * Armed once per JVM, however many classes it runs: with
   * {@code forkmode="once"} every class reports through a formatter of its
   * own, and the evidence they all record is the JVM's.
   */
  private static synchronized void armOnce() {
    if (armed) {
      return;
    }
    armed = true;
    try {
      Class<?> config = Class.forName(CONFIG);
      int probes = config.getField("PROBES").getInt(null);
      int[] widths = (int[]) config.getField("WIDTHS").get(null);
      evidenceDirectory = (String) config.getField("EVIDENCE").get(null);
      Supercov.arm(probes, widths);
    } catch (ReflectiveOperationException | ClassCastException e) {
      Supercov.arm(0, new int[0]);
    }
    // A JVM the task kills for running past its timeout never reaches the
    // end of its suite, but on Linux and macOS it is stopped, not destroyed:
    // what it recorded up to then is still written. Windows is startTest's.
    if (!hooked && evidenceDirectory != null && forkedByAnt()) {
      hooked = true;
      Runtime.getRuntime().addShutdownHook(new Thread(SupercovAntFormatter::writeEvidenceOnExit));
    }
  }

  /**
   * Whether this JVM is one Ant forked to run tests, the only kind a timeout
   * stops. With {@code fork="false"} the tests run in Ant's own JVM, each
   * class in a class loader of its own that Ant discards: a hook per loader
   * would outlive it, and its evidence is written at the end of its suite.
   */
  private static boolean forkedByAnt() {
    return System.getProperty("sun.java.command", "")
        .startsWith("org.apache.tools.ant.taskdefs.optional.junit.JUnitTestRunner");
  }

  /** On shutdown: a test still open there was stopped, and did not pass. */
  private static synchronized void writeEvidenceOnExit() {
    if (testOpen) {
      testOpen = false;
      Supercov.exitTest("failed");
    }
    writeEvidence();
  }

  private static synchronized void writeEvidence() {
    if (evidenceDirectory == null) {
      return;
    }
    try {
      Supercov.writeInto(evidenceDirectory);
    } catch (java.io.IOException e) {
      // The suite's outcome is the project's; evidence that could not be
      // written shows as a test with none, which the run reports.
    }
  }

  /**
   * Armed at the first test rather than here: Ant's own JVM starts a suite to
   * report a forked one that ended early, and records nothing of its own.
   */
  @Override
  public void startTestSuite(JUnitTest suite) {
    this.suite = suite.getName();
  }

  @Override
  public void endTestSuite(JUnitTest suite) {
    if (armed) {
      writeEvidence();
    }
  }

  @Override
  public void setOutput(OutputStream out) {}

  @Override
  public void setSystemOutput(String out) {}

  @Override
  public void setSystemError(String err) {}

  /**
   * The name the XML report gives the test, {@code Class#method}: a reader
   * matching coverage against Ant's own report should not have to translate.
   *
   * <p>A suite runs tests of other classes, which the task may also run on
   * their own: Ametys's {@code AllTrashTestCase} runs both trash classes, and
   * matches the pattern that runs each of them too. The same test ran twice,
   * once in each JVM, and two runs cannot share a name. One from a suite is
   * named within it, as the JUnit Platform names it, {@code Suite#Class#method}.
   */
  private String name(Test test) {
    String method = JUnitVersionHelper.getTestCaseName(test);
    String type = JUnitVersionHelper.getTestCaseClassName(test);
    String qualified = suite == null || suite.equals(type) ? type : suite + "#" + type;
    return Supercov.printable(qualified + "#" + method);
  }

  private static boolean reportsAnEndedJvm(Test test) {
    return test != null && VM_EXIT_TEST.equals(test.getClass().getName());
  }

  @Override
  public void startTest(Test test) {
    if (reportsAnEndedJvm(test)) {
      return;
    }
    armOnce();
    Supercov.enterTest(name(test), RUNNER, new String[0]);
    open = true;
    status = "passed";
    synchronized (SupercovAntFormatter.class) {
      testOpen = true;
      // Where no hook will run, what the JVM recorded so far is written
      // before each test, the test standing as not passing until it ends:
      // a timeout then loses only what the stopped test itself reached.
      if (KILLED_WITHOUT_HOOKS && hooked) {
        Supercov.setTestStatus("failed");
        writeEvidence();
      }
    }
  }

  @Override
  public void addFailure(Test test, AssertionFailedError failure) {
    status = "failed";
  }

  @Override
  public void addError(Test test, Throwable error) {
    status = "failed";
  }

  /** An {@code @Ignore}d test is reported without being started. */
  @Override
  public void testIgnored(Test test) {
    if (open) {
      status = "skipped";
      return;
    }
    armOnce();
    Supercov.enterTest(name(test), RUNNER, new String[0]);
    Supercov.exitTest("skipped");
  }

  /** An assumption that did not hold: the test declined to run. */
  @Override
  public void testAssumptionFailure(Test test, Throwable cause) {
    status = "skipped";
  }

  @Override
  public void endTest(Test test) {
    if (!open) {
      return;
    }
    open = false;
    synchronized (SupercovAntFormatter.class) {
      testOpen = false;
      Supercov.exitTest(status);
    }
  }
}
