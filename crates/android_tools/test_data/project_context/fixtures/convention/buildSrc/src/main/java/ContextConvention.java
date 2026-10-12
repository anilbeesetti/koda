import org.gradle.api.Plugin;
import org.gradle.api.Project;

public class ContextConvention implements Plugin<Project> {
    @Override
    public void apply(Project project) {
        project.getPluginManager().apply("java-library");
    }
}
