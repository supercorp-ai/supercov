package com.supercorp.supercov;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import org.testng.IExecutionListener;
import org.testng.IMethodInstance;
import org.testng.IMethodInterceptor;
import org.testng.ITestContext;
import org.testng.ITestResult;
import org.testng.TestListenerAdapter;
import org.testng.TestNG;
import org.testng.xml.XmlClass;
import org.testng.xml.XmlInclude;
import org.testng.xml.XmlSuite;
import org.testng.xml.XmlTest;

/**
 * The strong pass's driver for TestNG (`SupercovStrongPass`). Registered as
 * a TestNG listener in the strong copy: its method interceptor leaves the
 * build's own run empty, and once that run ends it runs the pass, each test
 * a TestNG run of its own holding one class and one method.
 *
 * <p>TestNG names a data provider's invocations by their order, and runs
 * them all when it runs the method: the method is the unit run here, and a
 * failure of any invocation is a kill.
 */
public final class SupercovStrongTestNG implements IExecutionListener, IMethodInterceptor {

  private static volatile boolean inPass = false;

  @Override
  public List<IMethodInstance> intercept(List<IMethodInstance> methods, ITestContext context) {
    if (System.getenv(SupercovStrongPass.PLAN) == null || inPass) {
      return methods;
    }
    return new ArrayList<>();
  }

  @Override
  public void onExecutionStart() {}

  @Override
  public void onExecutionFinish() {
    if (inPass || !SupercovStrongPass.shouldStart()) {
      return;
    }
    inPass = true;
    try {
      SupercovStrongPass.runEverywhere(new Runner(), SupercovStrongTestNG.class.getName());
    } finally {
      inPass = false;
    }
  }

  /** A JVM that shares the pass. */
  public static void main(String[] args) {
    inPass = true;
    SupercovStrongPass.minion(new Runner(), args);
  }

  static final class Runner implements SupercovStrongPass.Executor {
    @Override
    public boolean resolve(SupercovStrongPass.Test test) {
      int hash = test.name.indexOf('#');
      if (hash < 0) {
        return false;
      }
      String method = test.name.substring(hash + 1);
      int paren = method.indexOf('(');
      if (paren >= 0) {
        method = method.substring(0, paren);
      }
      try {
        Class.forName(test.className, false, SupercovStrongPass.fresh());
      } catch (ClassNotFoundException | LinkageError e) {
        return false;
      }
      test.handle = method;
      return true;
    }

    @Override
    public void execute(SupercovStrongPass.Test test, SupercovStrongPass.Outcome outcome) {
      XmlSuite suite = new XmlSuite();
      suite.setName("supercov-strong");
      XmlTest xmlTest = new XmlTest(suite);
      xmlTest.setName("supercov-strong");
      XmlClass xmlClass = new XmlClass(test.className, false);
      xmlClass.setIncludedMethods(
          new ArrayList<>(Collections.singletonList(new XmlInclude((String) test.handle))));
      xmlTest.setXmlClasses(new ArrayList<>(Collections.singletonList(xmlClass)));
      TestNG testng = new TestNG(false);
      testng.setUseDefaultListeners(false);
      testng.setVerbose(0);
      testng.setXmlSuites(new ArrayList<>(Collections.singletonList(suite)));
      TestListenerAdapter results = new TestListenerAdapter();
      testng.addListener(results);
      testng.run();
      for (ITestResult failed : results.getFailedTests()) {
        outcome.fail(failed.getThrowable());
      }
      for (ITestResult failed : results.getConfigurationFailures()) {
        outcome.fail(failed.getThrowable());
      }
      if (results.getPassedTests().isEmpty() && !outcome.failed) {
        // Nothing ran: a method TestNG would not run alone (a dependency
        // left out). As a baseline that is no candidate.
        outcome.failed = true;
        outcome.reason = "the test did not run alone";
      }
    }
  }
}
