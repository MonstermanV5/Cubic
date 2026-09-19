import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.shapes.CollisionContext;
import net.minecraft.world.phys.shapes.VoxelShape;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashMap;

/** Read-only exact-version shape oracle generator; never shipped with Cubic. */
public final class ShapeDump {
    private ShapeDump() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 1) {
            throw new IllegalArgumentException("expected output JSON path");
        }
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var states = new JsonArray();
        var shapes = new JsonArray();
        var shapeIndices = new LinkedHashMap<String, Integer>();
        for (var block : BuiltInRegistries.BLOCK) {
            var identifier = BuiltInRegistries.BLOCK.getKey(block).toString();
            for (var state : block.getStateDefinition().getPossibleStates()) {
                var record = new JsonObject();
                record.addProperty("runtime_id", Block.BLOCK_STATE_REGISTRY.getId(state));
                record.addProperty("block", identifier);
                var properties = new JsonObject();
                state.getValues()
                    .sorted(Comparator.comparing(value -> value.property().getName()))
                    .forEach(value -> properties.addProperty(
                        value.property().getName(), value.valueName()));
                record.add("properties", properties);
                record.addProperty("collision_shape", internShape(
                    boxes(state.getCollisionShape(
                        EmptyBlockGetter.INSTANCE, BlockPos.ZERO, CollisionContext.empty())),
                    shapes, shapeIndices));
                record.addProperty("outline_shape", internShape(
                    boxes(state.getShape(
                        EmptyBlockGetter.INSTANCE, BlockPos.ZERO, CollisionContext.empty())),
                    shapes, shapeIndices));
                var context = dynamicContext(identifier);
                if (context != null) {
                    record.addProperty("dynamic_collision", context);
                }
                states.add(record);
            }
        }
        var root = new JsonObject();
        root.addProperty("schema_version", 1);
        root.addProperty("minecraft_version", "26.1.2");
        root.addProperty("state_count", states.size());
        root.add("shapes", shapes);
        root.add("states", states);
        Files.writeString(
            Path.of(args[0]),
            new GsonBuilder().disableHtmlEscaping().create().toJson(root) + "\n");
    }

    private static int internShape(
            JsonArray shape,
            JsonArray shapes,
            LinkedHashMap<String, Integer> indices) {
        var key = shape.toString();
        var existing = indices.get(key);
        if (existing != null) {
            return existing;
        }
        var index = shapes.size();
        shapes.add(shape);
        indices.put(key, index);
        return index;
    }

    private static String dynamicContext(String identifier) {
        return switch (identifier) {
            case "minecraft:scaffolding" -> "scaffolding";
            case "minecraft:powder_snow" -> "powder_snow";
            case "minecraft:moving_piston" -> "moving_piston_block_entity";
            default -> null;
        };
    }

    private static JsonArray boxes(VoxelShape shape) {
        var values = new ArrayList<>(shape.toAabbs());
        values.sort(Comparator
            .comparingDouble((AABB box) -> box.minX)
            .thenComparingDouble(box -> box.minY)
            .thenComparingDouble(box -> box.minZ)
            .thenComparingDouble(box -> box.maxX)
            .thenComparingDouble(box -> box.maxY)
            .thenComparingDouble(box -> box.maxZ));
        var result = new JsonArray();
        for (var box : values) {
            var value = new JsonArray();
            value.add(box.minX);
            value.add(box.minY);
            value.add(box.minZ);
            value.add(box.maxX);
            value.add(box.maxY);
            value.add(box.maxZ);
            result.add(value);
        }
        return result;
    }
}
