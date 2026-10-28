package org.blockops.fabric;

import java.io.IOException;
import java.time.Duration;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandRegistrationCallback;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandManager;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientLifecycleEvents;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.screen.TitleScreen;
import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.screen.multiplayer.ConnectScreen;
import net.minecraft.client.gui.screen.multiplayer.MultiplayerScreen;
import net.minecraft.client.network.ServerAddress;
import net.minecraft.client.network.ServerInfo;
import net.minecraft.text.Text;
import org.blockops.bridge.LocalJoinBridge;

/** Client-only companion. A bridge opens only after a local player requests pairing. */
public final class BlockOpsClient implements ClientModInitializer {
    private static LocalJoinBridge bridge;

    @Override public void onInitializeClient() {
        ClientLifecycleEvents.CLIENT_STOPPING.register(client -> stopPairing());
        ClientCommandRegistrationCallback.EVENT.register((dispatcher, registryAccess) ->
            dispatcher.register(ClientCommandManager.literal("blockops").executes(context -> {
                context.getSource().sendFeedback(Text.literal("BlockOps: мод загружен. Для сопряжения откройте главное меню Minecraft."));
                return 1;
            })));
    }

    static synchronized String startPairing() throws IOException {
        stopPairing();
        bridge = new LocalJoinBridge(BlockOpsClient::startJoin, Duration.ofMinutes(5));
        return bridge.port() + ":" + bridge.token();
    }

    private static synchronized void stopPairing() {
        if (bridge != null) { bridge.close(); bridge = null; }
    }

    private static boolean startJoin(int port) {
        MinecraftClient client = MinecraftClient.getInstance();
        CompletableFuture<Boolean> result = new CompletableFuture<>();
        AtomicBoolean pending = new AtomicBoolean(true);
        client.execute(() -> {
            if (!pending.compareAndSet(true, false)) return;
            if (!client.isMultiplayerEnabled() || client.world != null || !(client.currentScreen instanceof TitleScreen || client.currentScreen instanceof MultiplayerScreen || client.currentScreen instanceof PairingScreen)) {
                result.complete(false);
                return;
            }
            try {
                String address = "127.0.0.1:" + port;
                ServerInfo info = new ServerInfo("BlockOps", address, ServerInfo.ServerType.OTHER);
                Screen parent = client.currentScreen instanceof PairingScreen screen ? screen.parentScreen() : client.currentScreen;
                ConnectScreen.connect(parent, client, ServerAddress.parse(address), info, false, null);
                result.complete(true);
            } catch (RuntimeException failure) {
                result.complete(false);
            }
        });
        try { return result.get(3, TimeUnit.SECONDS); }
        catch (Exception failure) { pending.set(false); return false; }
    }
}
