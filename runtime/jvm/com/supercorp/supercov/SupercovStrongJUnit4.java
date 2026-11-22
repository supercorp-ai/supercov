package com.supercorp.supercov;

import org.junit.runner.JUnitCore;
import org.junit.runner.Request;
import org.junit.runner.Result;
import org.junit.runner.notification.Failure;

/**
 * The strong pass's executor for JUnit 4 run by Ant's {@code <junit>} task
 * (`SupercovStrongPass`): a test named as Ant's XML names it,
 * {@code Class#method} (or {@code Suite#Class#method}), runs alone through
 * {@code JUnitCore}.
 */
public final class SupercovStrongJUnit4 implements SupercovStrongPass.Executor {

  @Override
  public boolean resolve(SupercovStrongPass.Test test) {
    String[] parts = test.name.split("#");
    if (parts.length < 2) {
      return false;
    }
    String className = parts[parts.length - 2];
    String method = parts[parts.length - 1];
    int bracket = method.indexOf('[');
    if (bracket >= 0) {
      method = method.substring(0, bracket).trim();
    }
    try {
      Class.forName(className, false, Thread.currentThread().getContextClassLoader());
      test.handle = new Object[] {className, method};
      return true;
    } catch (ClassNotFoundException | LinkageError e) {
      return false;
    }
  }

  @Override
  public void execute(SupercovStrongPass.Test test, SupercovStrongPass.Outcome outcome) {
    Object[] handle = (Object[]) test.handle;
    Class<?> type;
    try {
      type = Class.forName((String) handle[0], true, Thread.currentThread().getContextClassLoader());
    } catch (ClassNotFoundException e) {
      outcome.fail(e);
      return;
    }
    Result result = new JUnitCore().run(Request.method(type, (String) handle[1]));
    for (Failure failure : result.getFailures()) {
      outcome.fail(failure.getException());
    }
    if (result.getRunCount() == 0 && !outcome.failed) {
      outcome.failed = true;
      outcome.reason = "the test did not run alone";
    }
  }

  /** A JVM that shares the pass. */
  public static void main(String[] args) {
    SupercovStrongPass.minion(new SupercovStrongJUnit4(), args);
  }
}
