import com.android.tools.preview.multipreview.PreviewMethodFinder;
import com.android.tools.configurations.Configuration;
import com.android.tools.render.Renderer;
import com.android.tools.render.RenderRequest;
import com.android.tools.render.common.JsonSerializationKt;
import com.android.tools.render.framework.IJFramework;
import com.android.tools.rendering.RenderResult;
import com.android.ide.common.rendering.api.ViewInfo;
import com.intellij.openapi.util.Disposer;
import com.google.gson.*;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.tree.ClassNode;
import kotlin.Unit;
import kotlin.jvm.functions.Function1;
import java.awt.image.BufferedImage;
import java.io.File;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.lang.reflect.Method;
import java.nio.file.*;
import java.util.*;
import java.util.jar.JarFile;
import javax.imageio.ImageIO;

public final class PreviewBridge {
    public static void main(String[] args) {
        int status = 0;
        try {
            if (args.length < 2) throw new IllegalArgumentException("Missing bridge arguments");
            if (args[0].equals("render")) {
                render(Path.of(args[1]));
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
        for (JsonElement path : model.getAsJsonArray(model.has("previewClassPath") ? "previewClassPath" : "projectClassPath")) {
            File file = new File(path.getAsString());
            if (file.isDirectory()) directories.add(file); else jars.add(file);
        }
        List<File> dependencies = new ArrayList<>();
        for (JsonElement path : model.getAsJsonArray("classPath")) dependencies.add(new File(path.getAsString()));
        var methods = new PreviewMethodFinder(directories, jars, directories, jars, dependencies).findAllPreviewMethods();
        var sorted = methods.stream().sorted(Comparator.comparing(method -> method.getMethod().getMethodFqn())).toList();
        JsonArray previews = new JsonArray();
        Map<String, String> ownerSources = new HashMap<>();
        Map<String, Integer> methodOrdinals = new HashMap<>();
        Gson gson = new Gson();
        for (var method : sorted) {
            String name = method.getMethod().getMethodFqn();
            String owner = name.substring(0, name.lastIndexOf('.'));
            if (!ownerSources.containsKey(owner)) ownerSources.put(owner, sourceFile(owner, directories, jars));
            String sourceFile = ownerSources.get(owner);
            if (sourceFile == null) continue;
            String packageName = owner.contains(".") ? owner.substring(0, owner.lastIndexOf('.')) : "";
            int index = methodOrdinals.getOrDefault(name, 0);
            var annotations = method.getPreviewAnnotations().stream()
                .sorted(Comparator.comparing(annotation -> new TreeMap<>(annotation.getParameters()).toString())).toList();
            for (var annotation : annotations) {
                JsonObject preview = new JsonObject();
                preview.addProperty("methodFQN", name);
                preview.addProperty("previewId", name + "_" + index++);
                preview.addProperty("sourceFile", sourceFile);
                preview.addProperty("packageName", packageName);
                JsonObject parameters = new JsonObject();
                annotation.getParameters().forEach((key, value) -> parameters.addProperty(key, String.valueOf(value)));
                preview.add("previewParams", parameters);
                JsonArray methodParameters = new JsonArray();
                for (var parameter : method.getMethod().getParameters()) {
                    JsonObject attributes = new JsonObject();
                    parameter.getAnnotationParameters().forEach((key, value) -> attributes.addProperty(key,
                        value instanceof org.objectweb.asm.Type ? ((org.objectweb.asm.Type) value).getClassName() : String.valueOf(value)));
                    methodParameters.add(attributes);
                }
                preview.add("methodParams", methodParameters);
                String wrapper = method.getMethod().getPreviewWrapperFqn();
                if (wrapper != null) preview.addProperty("previewWrapperFqn", wrapper);
                previews.add(preview);
            }
            methodOrdinals.put(name, index);
        }
        Files.writeString(output, gson.toJson(previews));
    }

    private static String sourceFile(String owner, List<File> directories, List<File> jars) throws Exception {
        String entry = owner.replace('.', '/') + ".class";
        byte[] bytes = null;
        for (File directory : directories) {
            Path path = directory.toPath().resolve(entry);
            if (Files.isRegularFile(path)) { bytes = Files.readAllBytes(path); break; }
        }
        if (bytes == null) {
            for (File file : jars) {
                try (JarFile jar = new JarFile(file)) {
                    var item = jar.getJarEntry(entry);
                    if (item != null) {
                        try (var stream = jar.getInputStream(item)) { bytes = stream.readAllBytes(); }
                        break;
                    }
                }
            }
        }
        if (bytes == null) throw new IllegalStateException("Missing preview class: " + owner);
        ClassNode node = new ClassNode();
        new ClassReader(bytes).accept(node, ClassReader.SKIP_CODE | ClassReader.SKIP_FRAMES);
        if (node.visibleAnnotations != null) {
            for (var annotation : node.visibleAnnotations) {
                if (annotation.desc.equals("Lkotlin/Metadata;") && annotation.values != null) {
                    for (int index = 0; index + 1 < annotation.values.size(); index += 2) {
                        // Multifile facade methods duplicate their implementation part's annotations.
                        if (annotation.values.get(index).equals("k") && annotation.values.get(index + 1).equals(4)) return null;
                    }
                }
            }
        }
        if (node.sourceFile == null) {
            System.err.println("Skipping preview class without source information: " + owner);
            return null;
        }
        return node.sourceFile;
    }

    private static void render(Path settingsPath) throws Exception {
        com.android.tools.render.common.PreviewRendering settings;
        try (var reader = Files.newBufferedReader(settingsPath)) {
            settings = JsonSerializationKt.readPreviewRenderingJson(reader);
        }
        JsonObject manifest = new JsonObject();
        JsonArray results = new JsonArray();
        manifest.add("screenshotResults", results);
        Gson gson = new Gson();
        // Keep Google's configuration, device masking and diagnostic extraction exactly as in
        // the checksum-pinned alpha15 renderer, while retaining the scene before it is disposed.
        Method apply = Class.forName("com.android.tools.preview.PreviewConfigurationKt").getMethod(
            "applyTo$default", com.android.tools.preview.ConfigurablePreviewElement.class,
            Configuration.class, Function1.class, int.class, Object.class);
        Method process = Renderer.class.getDeclaredMethod("postProcessRenderedImage", Configuration.class, RenderResult.class);
        Method extract = Renderer.class.getDeclaredMethod("extractError", RenderResult.class, BufferedImage.class);
        process.setAccessible(true);
        extract.setAccessible(true);
        try (Renderer renderer = new Renderer(settings.getFontsPath(), settings.getResourceApkPath(),
                settings.getNamespace(), settings.getClassPath(), settings.getProjectClassPath(),
                settings.getLayoutlibPath(), List.of(), List.of())) {
            for (var screenshot : settings.getScreenshots()) {
                int parameterIndex = 0;
                try {
                    var element = screenshot.toPreviewElement(renderer.getModule());
                    var request = new RenderRequest(configuration -> {
                        try { apply.invoke(null, element, configuration, null, 2, null); }
                        catch (Exception error) { throw new IllegalStateException("Could not apply preview configuration", error); }
                        return Unit.INSTANCE;
                    }, element::resolveXmlLayouts);
                    var iterator = renderer.render(request).iterator();
                    while (iterator.hasNext()) {
                        var rendered = iterator.next();
                        RenderResult result = rendered.getSecond();
                        JsonObject item = resultIdentity(screenshot.getPreviewId(), parameterIndex++);
                        results.add(item);
                        try {
                            BufferedImage image = (BufferedImage) process.invoke(renderer, rendered.getFirst(), result);
                            Object error = extract.invoke(renderer, result, image);
                            if (error != null) item.add("error", gson.toJsonTree(error));
                            if (image != null) {
                                String imagePath = "image-" + (results.size() - 1) + ".png";
                                Path output = Path.of(settings.getOutputFolder()).resolve(imagePath);
                                Files.createDirectories(output.getParent());
                                if (!ImageIO.write(image, "png", output.toFile())) throw new IllegalStateException("No PNG writer");
                                item.addProperty("imagePath", imagePath);
                                item.addProperty("width", image.getWidth());
                                item.addProperty("height", image.getHeight());
                            }
                            JsonArray nodes = new JsonArray();
                            item.add("nodes", nodes);
                            try {
                                for (ViewInfo root : result.getRootViews()) collectAdapters(root, nodes);
                                if (nodes.isEmpty()) item.addProperty("inspectionError", "Compose did not provide layout source information. Enable source information in the Compose compiler.");
                            } catch (Exception inspectionError) {
                                item.addProperty("inspectionError", "Could not inspect this Compose version: " + inspectionError.getMessage());
                            }
                        } catch (Exception error) { item.add("error", errorJson(error)); }
                        finally { result.dispose(); }
                    }
                    if (parameterIndex == 0) {
                        JsonObject item = resultIdentity(screenshot.getPreviewId(), 0);
                        item.add("error", errorJson(new IllegalStateException("The preview parameter provider returned no values")));
                        results.add(item);
                    }
                } catch (Exception error) {
                    JsonObject item = resultIdentity(screenshot.getPreviewId(), parameterIndex);
                    item.add("error", errorJson(error));
                    results.add(item);
                }
            }
        } catch (Throwable error) { manifest.add("globalError", errorJson(error)); }
        finally {
            Files.writeString(Path.of(settings.getResultsFilePath()), gson.toJson(manifest));
            Disposer.dispose(IJFramework.INSTANCE);
        }
    }

    private static JsonObject resultIdentity(String id, int parameterIndex) {
        JsonObject item = new JsonObject();
        item.addProperty("previewId", id);
        item.addProperty("parameterIndex", parameterIndex);
        return item;
    }

    private static JsonObject errorJson(Throwable error) {
        while (error instanceof java.lang.reflect.InvocationTargetException && error.getCause() != null) error = error.getCause();
        JsonObject item = new JsonObject();
        item.addProperty("message", error.toString());
        StringWriter trace = new StringWriter();
        error.printStackTrace(new PrintWriter(trace));
        item.addProperty("stackTrace", trace.toString());
        return item;
    }

    private static void collectAdapters(ViewInfo view, JsonArray nodes) throws Exception {
        Object adapter = view.getViewObject();
        if (adapter != null && adapter.getClass().getName().equals("androidx.compose.ui.tooling.ComposeViewAdapter")) {
            Method getter = Arrays.stream(adapter.getClass().getDeclaredMethods())
                .filter(method -> method.getName().contains("getViewInfos") && method.getParameterCount() == 0)
                .findFirst().orElseThrow();
            getter.setAccessible(true);
            for (Object node : (List<?>) getter.invoke(adapter)) collectNodes(node, 0, nodes);
        }
        if (view.getChildren() != null) for (ViewInfo child : view.getChildren()) collectAdapters(child, nodes);
    }

    private static Object get(Object object, String name) throws Exception {
        Method method = object.getClass().getMethod(name);
        method.setAccessible(true);
        return method.invoke(object);
    }

    private static void collectNodes(Object value, int depth, JsonArray nodes) throws Exception {
        if (depth > 1000 || nodes.size() >= 10000) throw new IllegalStateException("Compose hierarchy is too large");
        JsonObject node = new JsonObject();
        node.addProperty("fileName", String.valueOf(get(value, "getFileName")));
        node.addProperty("lineNumber", ((Number) get(value, "getLineNumber")).intValue());
        node.addProperty("depth", depth);
        Object bounds = get(value, "getBounds");
        JsonArray rectangle = new JsonArray();
        for (String edge : List.of("Left", "Top", "Right", "Bottom")) rectangle.add(((Number) get(bounds, "get" + edge)).intValue());
        node.add("bounds", rectangle);
        int packageHash = -1;
        try {
            Object location = get(value, "getLocation");
            if (location != null) packageHash = ((Number) get(location, "getPackageHash")).intValue();
        } catch (NoSuchMethodException ignored) { }
        node.addProperty("packageHash", packageHash);
        String name = "";
        try { Object result = get(value, "getName"); if (result != null) name = result.toString(); }
        catch (NoSuchMethodException ignored) { }
        node.addProperty("name", name);
        nodes.add(node);
        for (Object child : (List<?>) get(value, "getChildren")) collectNodes(child, depth + 1, nodes);
    }
}
