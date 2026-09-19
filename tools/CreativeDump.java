import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import io.netty.buffer.Unpooled;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.core.Registry;
import net.minecraft.network.RegistryFriendlyByteBuf;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.contents.TranslatableContents;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.RegistryLayer;
import net.minecraft.server.packs.PackLocationInfo;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.VanillaPackResourcesBuilder;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.item.CreativeModeTabs;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.block.entity.BannerPattern;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Base64;
import java.util.List;
import java.util.Optional;

/** Read-only exact-version evidence extractor; it is not shipped with Cubic. */
public final class CreativeDump {
    private CreativeDump() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 1) {
            throw new IllegalArgumentException("expected output JSON path");
        }
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var location = new PackLocationInfo(
            "vanilla", Component.literal("vanilla"), PackSource.BUILT_IN, Optional.empty());
        var pack = new VanillaPackResourcesBuilder()
            .pushJarResources()
            .exposeNamespace("minecraft")
            .build(location);
        try (var resources = new MultiPackResourceManager(PackType.SERVER_DATA, List.of(pack))) {
            var layered = RegistryLayer.createRegistryAccess();
            var staticAccess = layered.getLayer(RegistryLayer.STATIC);
            var staticTags = TagLoader.loadTagsForExistingRegistries(resources, staticAccess);
            staticTags.forEach(net.minecraft.core.Registry.PendingTags::apply);
            var worldgen = RegistryDataLoader.load(
                resources,
                TagLoader.buildUpdatedLookups(staticAccess, staticTags),
                RegistryDataLoader.WORLDGEN_REGISTRIES,
                Runnable::run).join();
            layered = layered.replaceFrom(RegistryLayer.WORLDGEN, worldgen);
            var access = layered.compositeAccess();
            var pendingTags = TagLoader.loadTagsForExistingRegistries(resources, access);
            pendingTags.forEach(net.minecraft.core.Registry.PendingTags::apply);
            BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(access)
                .forEach(net.minecraft.core.component.DataComponentInitializers.PendingComponents::apply);

            var root = new JsonObject();
            root.addProperty("schema_version", 1);
            root.addProperty("minecraft_version", "26.1.2");
            var bannerPatterns = new JsonArray();
            Registry<BannerPattern> bannerPatternRegistry =
                access.lookupOrThrow(Registries.BANNER_PATTERN);
            for (int rawId = 0; rawId < bannerPatternRegistry.size(); rawId++) {
                bannerPatterns.add(bannerPatternRegistry.getKey(
                    bannerPatternRegistry.byId(rawId)).toString());
            }
            root.add("banner_patterns", bannerPatterns);
            root.add("without_permissions", capture(access, false));
            root.add("with_permissions", capture(access, true));
            Files.writeString(
                Path.of(args[0]),
                new GsonBuilder().setPrettyPrinting().create().toJson(root) + "\n");
        }
    }

    private static JsonArray capture(net.minecraft.core.RegistryAccess access, boolean permissions) {
        CreativeModeTabs.tryRebuildTabContents(FeatureFlags.DEFAULT_FLAGS, permissions, access);
        var result = new JsonArray();
        for (var tab : CreativeModeTabs.allTabs()) {
            var object = new JsonObject();
            object.addProperty("id", BuiltInRegistries.CREATIVE_MODE_TAB.getKey(tab).toString());
            var contents = tab.getDisplayName().getContents();
            if (!(contents instanceof TranslatableContents translatable)) {
                throw new IllegalStateException("Creative tab title is not translatable");
            }
            object.addProperty("title_key", translatable.getKey());
            object.addProperty("row", tab.row().name().toLowerCase());
            object.addProperty("column", tab.column());
            object.addProperty("type", tab.getType().name().toLowerCase());
            object.addProperty("background", tab.getBackgroundTexture().toString());
            object.addProperty("aligned_right", tab.isAlignedRight());
            object.addProperty("show_title", tab.showTitle());
            object.addProperty("can_scroll", tab.canScroll());
            object.addProperty("should_display", tab.shouldDisplay());
            object.add("icon", stack(access, tab.getIconItem(), true));
            var search = tab.getSearchTabDisplayItems();
            var items = new JsonArray();
            for (var item : tab.getDisplayItems()) {
                items.add(stack(access, item, search.contains(item)));
            }
            object.add("items", items);
            result.add(object);
        }
        return result;
    }

    @SuppressWarnings({"rawtypes", "unchecked"})
    private static JsonObject stack(
        net.minecraft.core.RegistryAccess access, ItemStack stack, boolean searchVisible) {
        var object = new JsonObject();
        object.addProperty("item", BuiltInRegistries.ITEM.getKey(stack.getItem()).toString());
        object.addProperty("count", stack.getCount());
        object.addProperty("search_visible", searchVisible);
        object.addProperty(
            "effective_item_model",
            stack.get(net.minecraft.core.component.DataComponents.ITEM_MODEL).toString());
        var features = new JsonArray();
        FeatureFlags.REGISTRY.toNames(stack.getItem().requiredFeatures()).stream()
            .sorted()
            .forEach(name -> features.add(name.toString()));
        object.add("required_features", features);
        var components = new JsonArray();
        stack.getComponentsPatch().entrySet().stream()
            .sorted(java.util.Comparator.comparing(entry ->
                BuiltInRegistries.DATA_COMPONENT_TYPE.getKey(entry.getKey()).toString()))
            .forEach(entry -> {
                var component = new JsonObject();
                component.addProperty(
                    "id", BuiltInRegistries.DATA_COMPONENT_TYPE.getKey(entry.getKey()).toString());
                component.addProperty("removed", entry.getValue().isEmpty());
                if (entry.getValue().isPresent()) {
                    var buffer = new RegistryFriendlyByteBuf(Unpooled.buffer(), access);
                    ((net.minecraft.network.codec.StreamCodec) entry.getKey().streamCodec())
                        .encode(buffer, entry.getValue().get());
                    var bytes = new byte[buffer.readableBytes()];
                    buffer.getBytes(buffer.readerIndex(), bytes);
                    component.addProperty("value_base64", Base64.getEncoder().encodeToString(bytes));
                }
                components.add(component);
            });
        object.add("components", components);
        return object;
    }
}
