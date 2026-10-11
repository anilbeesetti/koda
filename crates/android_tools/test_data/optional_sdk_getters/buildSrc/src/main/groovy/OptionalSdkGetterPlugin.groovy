import org.gradle.api.Action
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.provider.Provider

// This supplemental fixture implements only the SDK surface read by the adapter.
// It is not an AGP replacement, rendering runtime, or reference parity fixture.
class OptionalSdkGetterPlugin implements Plugin<Project> {
    void apply(Project project) {
        def getter = project.providers.gradleProperty('fixtureGetter').get()
        def failure = project.providers.gradleProperty('fixtureFailure').get()
        if (!(getter in ['getPluginVersion', 'getVersion']) || !(failure in ['exception', 'linkage'])) {
            throw new IllegalArgumentException('Unknown optional SDK getter failure scenario')
        }
        def directory = project.layout.projectDirectory.dir('src/main/java')
        def java = new FixtureSourceDirectories(
            project.provider { [directory] }, project.provider { [directory] }, directory.asFile)
        def manifests = new FixtureManifestSources(all: project.provider {
            [project.layout.projectDirectory.file('src/main/AndroidManifest.xml')]
        })
        def configuration = project.configurations.create('debugCompileClasspath') {
            canBeConsumed = false
            canBeResolved = true
        }
        def variant = new FixtureVariant(name: 'debug', buildType: 'debug', productFlavors: [],
            namespace: project.provider { 'example' }, compileConfiguration: configuration,
            sources: new FixtureComponentSources(java: java, manifests: manifests))
        project.extensions.add('android', new FixtureAndroidDsl(namespace: 'example', sourceSets: [],
            buildTypes: [], productFlavors: []))
        project.extensions.add('androidComponents', new FixtureAndroidComponents(variant, getter, failure))
    }
}

class FixtureAndroidDsl {
    String namespace
    List sourceSets
    List buildTypes
    List productFlavors
}

class FixtureVariant {
    String name
    String buildType
    List productFlavors
    Provider namespace
    org.gradle.api.artifacts.Configuration compileConfiguration
    FixtureComponentSources sources
}

class FixtureComponentSources {
    FixtureSourceDirectories java
    Object kotlin
    Object res
    Object assets
    FixtureManifestSources manifests
}

class FixtureManifestSources {
    Provider all
}

class FixtureSourceDirectories {
    final Provider all
    final Provider staticDirectories
    final File directory

    FixtureSourceDirectories(Provider all, Provider staticDirectories, File directory) {
        this.all = all
        this.staticDirectories = staticDirectories
        this.directory = directory
    }

    Provider getStatic() { staticDirectories }

    List<File> 'variantSourcesForModel$gradle_core'(kotlin.jvm.functions.Function1 predicate) {
        [directory]
    }
}

class FixtureAndroidComponents {
    final Object variant
    final String getter
    final String failure

    FixtureAndroidComponents(Object variant, String getter, String failure) {
        this.variant = variant
        this.getter = getter
        this.failure = failure
    }

    FixtureVariantSelector selector() { new FixtureVariantSelector() }

    void onVariants(FixtureVariantSelector selector, Action action) { action.execute(variant) }

    FixturePluginVersion getPluginVersion() {
        if (getter == 'getPluginVersion') FixturePluginVersion.fail(getter, failure)
        new FixturePluginVersion(getter, failure)
    }
}

class FixtureVariantSelector {
    FixtureVariantSelector all() { this }
}

class FixturePluginVersion {
    final String getter
    final String failure

    FixturePluginVersion(String getter, String failure) {
        this.getter = getter
        this.failure = failure
    }

    int getMajor() { 9 }
    int getMinor() { 4 }
    int getMicro() { 0 }
    int getPreview() { 0 }
    String getPreviewType() { null }

    String getVersion() {
        if (getter == 'getVersion') fail(getter, failure)
        '9.4.0'
    }

    static void fail(String getter, String failure) {
        def detail = 'Fixture ' + getter + ' ' + failure + ' failure'
        if (failure == 'linkage') throw new NoClassDefFoundError(detail)
        throw new IllegalStateException(detail)
    }
}
