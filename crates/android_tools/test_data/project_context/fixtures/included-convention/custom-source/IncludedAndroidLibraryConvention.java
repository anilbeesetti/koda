import org.gradle.api.Plugin;
import org.gradle.api.Project;

public final class IncludedAndroidLibraryConvention implements Plugin<Project> {
    @Override
    public void apply(Project project) {
        project.getPlugins().apply("com.android.library");
    }
}
