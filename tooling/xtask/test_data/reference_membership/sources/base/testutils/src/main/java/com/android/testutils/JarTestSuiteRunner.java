/*
 * Copyright (C) 2016 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package com.android.testutils;

import static java.util.concurrent.TimeUnit.MILLISECONDS;
import static java.util.concurrent.TimeUnit.SECONDS;

import org.junit.runner.Description;
import org.junit.runner.Runner;
import org.junit.runner.notification.Failure;
import org.junit.runner.notification.RunListener;
import org.junit.runner.notification.RunNotifier;
import org.junit.runners.Suite;
import org.junit.runners.model.InitializationError;
import org.junit.runners.model.RunnerBuilder;
import org.junit.runners.model.Statement;

import java.io.IOException;
import java.lang.annotation.ElementType;
import java.lang.annotation.Inherited;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;
import java.lang.management.ManagementFactory;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.Executors;
import java.util.function.Predicate;
import java.util.regex.Pattern;
import java.util.stream.Stream;

import javax.management.ObjectName;

public class JarTestSuiteRunner extends Suite {
    private final Runner finalizerTest;
    private final FinalizerTest finalizerTestAnnotation;

    /** Putatively temporary mechanism to avoid running certain classes. */
    @Retention(RetentionPolicy.RUNTIME)
    @Target(ElementType.TYPE)
    public @interface ExcludeClasses {

        Class<?>[] value();
    }

    /**
     * Mechanism to run an additional test at the end of the suite when all tests pass. This can be
     * used to add assertions about the tests themselves, for example that they do not leak memory.
     * Normally this would be accomplished using suite-level @AfterClass methods, but exceptions
     * thrown from those are handled poorly by the Bazel JUnit runner (b/152757288). The finalizer
     * test will not run if any test in the suite has failed.
     */
    @Inherited
    @Retention(RetentionPolicy.RUNTIME)
    @Target(ElementType.TYPE)
    public @interface FinalizerTest {
        Class<?> value();
    }

    public JarTestSuiteRunner(Class<?> suiteClass, RunnerBuilder builder)
            throws InitializationError, ClassNotFoundException, IOException {
        super(new DelegatingRunnerBuilder(builder), suiteClass, getTestClasses(suiteClass));
        finalizerTestAnnotation = suiteClass.getAnnotation(FinalizerTest.class);
        if (finalizerTestAnnotation != null) {
            System.out.printf("Found finalizer test: %s%n", finalizerTestAnnotation.value());
            finalizerTest = builder.safeRunnerForClass(finalizerTestAnnotation.value());
        } else {
            finalizerTest = null;
        }
        scheduleThreadDumpOnWindows();
    }

    private static Class<?>[] getTestClasses(Class<?> suiteClass)
            throws ClassNotFoundException, IOException {
        String jarSuffix = System.getProperty("test.suite.jar");
        if (jarSuffix == null) {
            if (TestUtils.runningFromBazel()) {
                throw new RuntimeException(
                        "Must set test.suite.jar to the name of the jar containing JUnit tests");
            }

            // When running tests outside of Bazel (such as via JPS or IntelliJ IDE), test.suite.jar
            // is not set.
            // Returning an empty array allows the test suite class to complete gracefully while
            // individual test
            // classes are discovered and executed directly by the IDE test runner.
            return new Class<?>[0];
        }

        long start = System.currentTimeMillis();
        TestGroup testGroup = TestGroup.builder()
                .setClassLoader(suiteClass.getClassLoader())
                .includeJUnit3()
                .excludeClassNames(classNamesToExclude(suiteClass))
                .build();
        List<Class<?>> testClasses = testGroup.scanTestClasses(suiteClass, jarSuffix);
        System.out.printf(
                "Found %d tests in %dms%n",
                testClasses.size(), (System.currentTimeMillis() - start));
        if (testClasses.isEmpty()) {
            throw new RuntimeException("No tests found in class path using suffix: " + jarSuffix);
        }
        String filter = System.getProperty("test_filter");
        String filterExcludes = System.getProperty("test_exclude_filter");
        Stream<Class<?>> stream = testClasses.stream();
        if (filter != null && !filter.isEmpty()) {
            Predicate<String> filterPredicate = Pattern.compile(filter).asPredicate();
            stream = stream.filter(c -> filterPredicate.test(c.getName()));
        }
        if (filterExcludes != null && !filterExcludes.isEmpty()) {
            Predicate<String> excludePredicate = Pattern.compile(filterExcludes).asPredicate();
            stream = stream.filter(c -> !excludePredicate.test(c.getName()));
        }
        Class<?>[] classes = stream.toArray(i -> new Class<?>[i]);
        System.out.printf("Filtered to %d tests%n", classes.length);
        return classes;
    }


    @Override
    protected Statement classBlock(RunNotifier notifier) {
        // Note: we run the finalizer test here instead of adding it to the list of child runners
        // because the finalizer test should not be subject to test filters, sharding, sorting, etc.
        Statement delegate = super.classBlock(notifier);
        boolean[] succeeded = new boolean[] {true};
        notifier.addListener(
                new RunListener() {
                    @Override
                    public void testFailure(Failure failure) throws Exception {
                        succeeded[0] = false;
                    }
                });
        return new Statement() {
            @Override
            public void evaluate() throws Throwable {
                delegate.evaluate();
                if (finalizerTest != null && succeeded[0]) {
                    finalizerTest.run(notifier);
                }
            }
        };
    }

    @Override
    public Description getDescription() {
        // Add the finalizer test to the suite description so that it is included in test.xml.
        Description description = super.getDescription();
        if (finalizerTest != null) {
            description.addChild(finalizerTest.getDescription());
        }
        return description;
    }

    /** Putatively temporary mechanism to avoid running certain classes. */
    private static Set<String> classNamesToExclude(
            Class<?> suiteClass) {
        Set<String> excludeClassNames = new HashSet<>();
        ExcludeClasses annotation = suiteClass.getAnnotation(ExcludeClasses.class);
        if (annotation != null) {
            for (Class<?> classToExclude : annotation.value()) {
                String className = classToExclude.getCanonicalName();
                if (!excludeClassNames.add(className)) {
                    throw new RuntimeException(
                            String.format(
                                    "on %s, %s value duplicated: %s",
                                    suiteClass.getSimpleName(),
                                    ExcludeClasses.class.getSimpleName(),
                                    className));
                }
            }
        }
        return excludeClassNames;
    }

    // On Windows, Bazel sends SIGKILL without sending SIGTERM to the test runner, so we won't
    // get a thread dump (see https://github.com/bazelbuild/bazel/issues/12684,
    // https://github.com/bazelbuild/bazel/issues/14298, etc.). To ease debugging, we trigger a
    // thread dump just before the target is expected to time out.
    private static void scheduleThreadDumpOnWindows() {
        if (!OsType.getHostOs().equals(OsType.WINDOWS)) {
            return;
        }

        long testTimeout; // In seconds.
        try {
            // Based on https://bazel.build/reference/test-encyclopedia.
            testTimeout = Long.parseLong(System.getenv("TEST_TIMEOUT"));
        }
        catch (NumberFormatException e) {
            return;
        }
        long jvmUptime = MILLISECONDS.toSeconds(ManagementFactory.getRuntimeMXBean().getUptime());
        long threadDumpDelay = testTimeout - 10 - jvmUptime;
        if (threadDumpDelay <= 0) {
            return;
        }

        Runnable dumpThreads = () -> {
            try {
                System.out.println("Approaching Bazel test timeout; dumping all threads.");
                System.out.println(ManagementFactory.getPlatformMBeanServer().invoke(
                        ObjectName.getInstance("com.sun.management:type=DiagnosticCommand"),
                        "threadPrint",
                        new Object[]{new String[]{/* locks */ "-l", /* extended */ "-e"}},
                        new String[]{String[].class.getName()}));
            }
            catch (Throwable e) {
                // Catch exceptions here, otherwise the ExecutorService will swallow them.
                //noinspection CallToPrintStackTrace
                e.printStackTrace();
            }
        };

        var daemonExecutor = Executors.newSingleThreadScheduledExecutor(task -> {
            Thread thread = new Thread(task);
            thread.setDaemon(true);
            thread.setName("JarTestSuiteRunner Thread Dumper");
            return thread;
        });

        daemonExecutor.schedule(dumpThreads, threadDumpDelay, SECONDS);
    }
}
