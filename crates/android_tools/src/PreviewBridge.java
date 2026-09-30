import com.android.tools.preview.multipreview.PreviewMethodFinder;
import com.google.gson.*;
import java.io.File;
import java.nio.file.*;
import java.util.*;

public final class PreviewBridge {
    public static void main(String[] args) {
        int status = 0;
        try {
            if (args[0].equals("render")) {
                com.android.tools.render.common.MainKt.main(new String[]{args[1]});
            } else if (args[0].equals("discover")) {
                discover(Path.of(args[1]), Path.of(args[2]));
            } else {
                throw new IllegalArgumentException("Expected discover or render");
            }
        } catch (Throwable error) {
            error.printStackTrace();
            status = 1;
        }
        // Renderer alpha15 disposes its framework but leaves non-daemon worker threads alive.
        System.exit(status);
    }

    private static void discover(Path modelPath, Path output) throws Exception {
        JsonObject model = JsonParser.parseString(Files.readString(modelPath)).getAsJsonObject();
        List<File> directories = new ArrayList<>();
        List<File> jars = new ArrayList<>();
        for (JsonElement path : model.getAsJsonArray("projectClassPath")) {
            File file = new File(path.getAsString());
            if (file.isDirectory()) directories.add(file); else jars.add(file);
        }
        List<File> dependencies = new ArrayList<>();
        for (JsonElement path : model.getAsJsonArray("classPath")) dependencies.add(new File(path.getAsString()));
        var methods = new PreviewMethodFinder(directories, jars, directories, jars, dependencies).findAllPreviewMethods();
        var sorted = methods.stream().sorted(Comparator.comparing(method -> method.getMethod().getMethodFqn())).toList();
        JsonArray previews = new JsonArray();
        Gson gson = new Gson();
        for (var method : sorted) {
            int index = 0;
            var annotations = method.getPreviewAnnotations().stream()
                .sorted(Comparator.comparing(annotation -> new TreeMap<>(annotation.getParameters()).toString())).toList();
            for (var annotation : annotations) {
                JsonObject preview = new JsonObject();
                String name = method.getMethod().getMethodFqn();
                preview.addProperty("methodFQN", name);
                preview.addProperty("previewId", name + "_" + index++);
                JsonObject parameters = new JsonObject();
                annotation.getParameters().forEach((key, value) -> parameters.addProperty(key, String.valueOf(value)));
                preview.add("previewParams", parameters);
                JsonArray methodParameters = new JsonArray();
                for (var parameter : method.getMethod().getParameters()) {
                    methodParameters.add(gson.toJsonTree(parameter.getAnnotationParameters()));
                }
                preview.add("methodParams", methodParameters);
                String wrapper = method.getMethod().getPreviewWrapperFqn();
                if (wrapper != null) preview.addProperty("previewWrapperFqn", wrapper);
                previews.add(preview);
            }
        }
        Files.writeString(output, gson.toJson(previews));
    }
}
