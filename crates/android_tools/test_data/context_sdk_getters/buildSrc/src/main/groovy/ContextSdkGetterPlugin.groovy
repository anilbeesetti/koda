import org.gradle.api.Plugin
import org.gradle.api.Project

// Supplemental API fixture, not an Android plugin or reference parity port.
class ContextSdkGetterPlugin implements Plugin<Project> {
    void apply(Project project) {
        def getter = project.providers.gradleProperty('fixtureGetter').get()
        def failure = project.providers.gradleProperty('fixtureFailure').get()
        def version = project.providers.gradleProperty('fixtureVersion').get()
        if (!(getter in ['none', 'getPluginVersion', 'getVersion']) ||
            !(failure in ['none', 'exception', 'linkage']) ||
            (getter == 'none') != (failure == 'none')) {
            throw new IllegalArgumentException('Unknown context SDK getter scenario')
        }
        project.extensions.add('androidComponents',
            new ContextAndroidComponents(getter, failure, version))
    }
}

class ContextAndroidComponents {
    final String getter
    final String failure
    final String version

    ContextAndroidComponents(String getter, String failure, String version) {
        this.getter = getter
        this.failure = failure
        this.version = version
    }

    ContextPluginVersion getPluginVersion() {
        if (getter == 'getPluginVersion') ContextPluginVersion.fail(getter, failure)
        new ContextPluginVersion(getter, failure, version)
    }
}

class ContextPluginVersion {
    final String getter
    final String failure
    final String version

    ContextPluginVersion(String getter, String failure, String version) {
        this.getter = getter
        this.failure = failure
        this.version = version
    }

    String getVersion() {
        if (getter == 'getVersion') fail(getter, failure)
        version
    }

    String toString() { 'Android Gradle Plugin version ' + version }

    static void fail(String getter, String failure) {
        def detail = 'Fixture ' + getter + ' ' + failure + ' failure'
        if (failure == 'linkage') throw new NoClassDefFoundError(detail)
        throw new IllegalStateException(detail)
    }
}
