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

  /** How the open test is ending, once something says it is not passing. */
  private String status;
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
    // end of its suite, but it is stopped, not destroyed: what it recorded up
    // to then is still written.
    if (!hooked && evidenceDirectory != null) {
      hooked = true;
      Runtime.getRuntime().addShutdownHook(new Thread(SupercovAntFormatter::writeEvidence));
    }
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

  @Override
  public void startTestSuite(JUnitTest suite) {
    armOnce();
  }

  @Override
  public void endTestSuite(JUnitTest suite) {
    writeEvidence();
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
   */
  private static String name(Test test) {
    String method = JUnitVersionHelper.getTestCaseName(test);
    String type = JUnitVersionHelper.getTestCaseClassName(test);
    return Supercov.printable(type + "#" + method);
  }

  @Override
  public void startTest(Test test) {
    armOnce();
    Supercov.enterTest(name(test), RUNNER, new String[0]);
    open = true;
    status = "passed";
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
    Supercov.exitTest(status);
  }
}
