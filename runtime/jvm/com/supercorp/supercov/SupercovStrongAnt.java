package com.supercorp.supercov;

import java.io.OutputStream;
import junit.framework.AssertionFailedError;
import junit.framework.Test;
import org.apache.tools.ant.taskdefs.optional.junit.JUnitResultFormatter;
import org.apache.tools.ant.taskdefs.optional.junit.JUnitTest;

/**
 * The strong pass's entry under Ant's {@code <junit>} task, in place of
 * Supercov's result formatter in a strong copy (the build names the
 * formatter's class, so this is compiled under that name). Each JVM the task
 * forks runs the pass when its first suite starts; the claims share the
 * mutants among them, and the task's own tests then run as they do.
 */
public final class SupercovAntFormatter implements JUnitResultFormatter {

  @Override
  public void startTestSuite(JUnitTest suite) {
    if (SupercovStrongPass.shouldStart()) {
      SupercovStrongPass.runEverywhere(
          new SupercovStrongJUnit4(), SupercovStrongJUnit4.class.getName());
    }
  }

  @Override
  public void endTestSuite(JUnitTest suite) {}

  @Override
  public void setOutput(OutputStream out) {}

  @Override
  public void setSystemOutput(String out) {}

  @Override
  public void setSystemError(String err) {}

  @Override
  public void addError(Test test, Throwable error) {}

  @Override
  public void addFailure(Test test, AssertionFailedError failure) {}

  @Override
  public void endTest(Test test) {}

  @Override
  public void startTest(Test test) {}
}
