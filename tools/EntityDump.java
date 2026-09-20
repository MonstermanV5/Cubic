import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;

import java.nio.file.Files;
import java.nio.file.Path;

/** Read-only exact-version entity-dimension extractor; never shipped with Cubic. */
public final class EntityDump {
    private EntityDump() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 1) {
            throw new IllegalArgumentException("expected output JSON path");
        }
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var entities = new JsonArray();
        for (var type : BuiltInRegistries.ENTITY_TYPE) {
            var record = new JsonObject();
            record.addProperty("raw_id", BuiltInRegistries.ENTITY_TYPE.getId(type));
            record.addProperty("identifier", BuiltInRegistries.ENTITY_TYPE.getKey(type).toString());
            record.addProperty("width", type.getWidth());
            record.addProperty("height", type.getHeight());
            entities.add(record);
        }
        var root = new JsonObject();
        root.addProperty("schema_version", 1);
        root.addProperty("minecraft_version", "26.1.2");
        root.addProperty("entity_count", entities.size());
        root.add("entities", entities);
        Files.writeString(Path.of(args[0]),
            new GsonBuilder().disableHtmlEscaping().create().toJson(root) + "\n");
    }
}
