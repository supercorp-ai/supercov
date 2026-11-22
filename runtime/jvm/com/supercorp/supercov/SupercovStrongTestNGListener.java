package com.supercorp.supercov;

import java.util.List;
import org.testng.IExecutionListener;
import org.testng.IMethodInstance;
import org.testng.IMethodInterceptor;
import org.testng.ITestContext;

/**
 * The strong pass's entry under Ant's {@code <testng>} task, in place of
 * Supercov's TestNG listener in a strong copy (the build names the
 * listener's class, so this is compiled under that name):
 * `SupercovStrongTestNG` does the work.
 */
public final class SupercovTestNGListener implements IExecutionListener, IMethodInterceptor {
  private final SupercovStrongTestNG strong = new SupercovStrongTestNG();

  @Override
  public List<IMethodInstance> intercept(List<IMethodInstance> methods, ITestContext context) {
    return strong.intercept(methods, context);
  }

  @Override
  public void onExecutionStart() {}

  @Override
  public void onExecutionFinish() {
    strong.onExecutionFinish();
  }
}
