load("//tools/base/bazel:bazel.bzl", "iml_module")
load("//tools/base/bazel:kotlin.bzl", "kotlin_test")
load("//tools/base/bazel:maven.bzl", "maven_library")
load("//tools/base/common:version.bzl", "BASE_VERSION")

# managed by go/iml_to_build
iml_module(
    name = "studio.android.sdktools.testutils",
    iml_files = ["android.sdktools.testutils.iml"],
    # do not sort: must match IML order
    test_deps = [
        "//tools/adt/idea/.idea/libraries:junit4",
        "//tools/base/common:studio.android.sdktools.common",
        "//tools/adt/idea/.idea/libraries:mockito",
        "//tools/adt/idea/.idea/libraries:truth",
        "//tools/adt/idea/.idea/libraries:google-dexlib2",
        "//tools/adt/idea/.idea/libraries:jimfs",
        "//tools/adt/idea/.idea/libraries:asm-tools",
    ],
    test_srcs = ["src/main/java"],
    test_tags = ["manual"],  # src/main/java is TEST scoped but contains no runnable tests
    visibility = ["//visibility:public"],
    # do not sort: must match IML order
    deps = ["@intellij//:intellij-sdk"],
)

# managed by go/iml_to_build
iml_module(
    name = "studio.testutils-tests",
    iml_files = ["testutils-tests.iml"],
    test_data = ["//prebuilts/studio/sdk:platforms/latest"] + glob(["src/test/data/**"]),
    # do not sort: must match IML order
    test_deps = [
        "//tools/adt/idea/.idea/libraries:junit4",
        "//tools/adt/idea/.idea/libraries:kotlin-test",
        "//tools/base/testutils:studio.android.sdktools.testutils",
        "//tools/base/annotations:studio.android.sdktools.android-annotations",
        "//tools/base/common:studio.android.sdktools.common",
        "//tools/adt/idea/.idea/libraries:truth",
        "//tools/adt/idea/.idea/libraries:google-dexlib2",
        "//tools/adt/idea/.idea/libraries:jimfs",
    ],
    test_resources = ["src/test/resources"],
    test_srcs = ["src/test/java"],
    visibility = ["//visibility:public"],
    # do not sort: must match IML order
    deps = [
        "@intellij//:intellij-sdk",
        "@intellij//:com.intellij.java",
        "//tools/adt/idea/.idea/libraries:truth-java8-extension",
    ],
)

maven_library(
    name = "tools.testutils",
    srcs = glob([
        "src/main/java/**/*.kt",
        "src/main/java/**/*.java",
    ]),
    coordinates = "com.android.tools:testutils",
    description = "Internal test utilities for Android Studio and other tools developed alongside Studio",
    lint_baseline = "lint_baseline.xml",
    module_name = "testutils",
    notice = "NOTICE",
    resource_strip_prefix = "tools/base/testutils",
    version = BASE_VERSION,
    visibility = ["//visibility:public"],
    exports = [
        "@maven//:com.google.truth.extensions.truth-java8-extension",
        "@maven//:com.google.truth.truth",
    ],
    deps = [
        "//tools/base/common:tools.common",
        "@maven//:com.android.tools.smali.smali-dexlib2",
        "@maven//:com.google.jimfs.jimfs",
        "@maven//:junit.junit",
        "@maven//:org.apache.commons.commons-compress",
        "@maven//:org.jetbrains.kotlin.kotlin-stdlib-jdk8",
        "@maven//:org.jetbrains.kotlin.kotlin-test",
        "@maven//:org.mockito.mockito-core",
        "@maven//:org.ow2.asm.asm",
    ],
)

kotlin_test(
    name = "tools.tests",
    srcs = glob([
        "src/test/java/**/*.kt",
        "src/test/java/**/*.java",
    ]),
    data = ["//prebuilts/studio/sdk:platforms/latest"] + glob(["src/test/data/**"]),
    jvm_flags = ["-Dtest.suite.jar=tools.tests.jar"],
    lint_baseline = "lint_baseline_tests.xml",
    resources = glob(["src/test/resources/**"]),
    test_class = "com.android.testutils.JarTestSuite",
    deps = [
        ":tools.testutils",
        "//tools/base/annotations",
        "//tools/base/common:tools.common",
        "@maven//:com.android.tools.smali.smali-dexlib2",
        "@maven//:com.google.jimfs.jimfs",
        "@maven//:com.google.truth.extensions.truth-java8-extension",
        "@maven//:com.google.truth.truth",
        "@maven//:org.jetbrains.kotlin.kotlin-stdlib-jdk8",
        "@maven//:org.jetbrains.kotlin.kotlin-test",
    ],
)
