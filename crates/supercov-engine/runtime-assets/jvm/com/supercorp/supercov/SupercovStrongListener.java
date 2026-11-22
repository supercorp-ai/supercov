package com.supercorp.supercov;

import org.junit.platform.launcher.TestExecutionListener;
import org.junit.platform.launcher.TestPlan;

/**
 * The strong pass's entry under Ant's {@code <junitlauncher>} task, in place
 * of Supercov's listener in a strong copy (the build names the listener's
 * class, so this is compiled under that name): `SupercovStrong` does the
 * work.
 */
public final class SupercovListener implements TestExecutionListener {
  private final SupercovStrong strong = new SupercovStrong();

  @Override
  public void testPlanExecutionStarted(TestPlan plan) {
    strong.testPlanExecutionStarted(plan);
  }
}
