package org.blockops.fabric;

import java.io.IOException;
import java.time.Duration;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandRegistrationCallback;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandManager;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientLifecycleEvents;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.screen.TitleScreen;
import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.screen.multiplayer.ConnectScreen;
import net.minecraft.client.gui.screen.multiplayer.MultiplayerScreen;
import net.minecraft.client.network.ServerAddress;
import net.minecraft.client.network.ServerInfo;
import net.minecraft.server.integrated.IntegratedServer;
import net.minecraft.text.Text;
import org.blockops.bridge.DiscoveryPublisher;
import org.blockops.bridge.LocalJoinBridge;
import org.blockops.bridge.WindowsDescriptorFile;
import org.blockops.bridge.WorldIdentity;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * Client-only companion. On Windows it advertises a one-use bridge through a private
 * descriptor that only this Windows user can read; elsewhere, and as a fallback, a bridge
 * opens only after a local player requests pairing.
 */
public final class BlockOpsClient implements ClientModInitializer {
    private static final Logger LOGGER = LoggerFactory.getLogger("blockops");
    private static LocalJoinBridge bridge;
    private static DiscoveryPublisher discovery;
    private static ScheduledExecutorService discoveryTimer;
    private static final AtomicBoolean openPairing = new AtomicBoolean();

    @Override public void onInitializeClient() {
        startDiscovery();
        ClientLifecycleEvents.CLIENT_STOPPING.register(client -> { stopPairing(); stopDiscovery(); });
        ClientTickEvents.END_CLIENT_TICK.register(client -> {
            if (openPairing.compareAndSet(true, false)) client.setScreen(new PairingScreen(null));
        });
        ClientCommandRegistrationCallback.EVENT.register((dispatcher, registryAccess) ->
            dispatcher.register(ClientCommandManager.literal("blockops").executes(context -> {
                openPairing.set(true);
                return 1;
            })));
    }

    static synchronized String startPairing() throws IOException {
        stopPairing();
        WorldIdentity<IntegratedServer> watchedWorld = new WorldIdentity<>();
        bridge = new LocalJoinBridge(BlockOpsClient::startJoin, () -> openWorldPort(watchedWorld), Duration.ofMinutes(5));
        return bridge.port() + ":" + bridge.token();
    }

    private static synchronized void stopPairing() {
        if (bridge != null) { bridge.close(); bridge = null; }
    }

    private static synchronized void startDiscovery() {
        if (!System.getProperty("os.name", "").startsWith("Windows")) return;
        try {
            var store = new WindowsDescriptorFile(WindowsDescriptorFile.defaultDirectory(), ProcessHandle.current().pid());
            discovery = new DiscoveryPublisher(() -> {
                WorldIdentity<IntegratedServer> watchedWorld = new WorldIdentity<>();
                return new LocalJoinBridge(BlockOpsClient::startJoin, () -> openWorldPort(watchedWorld), Duration.ofMinutes(5));
            }, store);
        } catch (IOException | RuntimeException | LinkageError failure) {
            LOGGER.warn("BlockOps automatic discovery is unavailable; use the pairing code instead");
            return;
        }
        DiscoveryPublisher publisher = discovery;
        AtomicBoolean failing = new AtomicBoolean();
        discoveryTimer = Executors.newSingleThreadScheduledExecutor(runnable -> {
            Thread thread = new Thread(runnable, "blockops-discovery");
            thread.setDaemon(true);
            return thread;
        });
        discoveryTimer.scheduleWithFixedDelay(() -> {
            try {
                publisher.refresh();
                failing.set(false);
            } catch (Exception | LinkageError failure) {
                // Usually the desktop is reading the previous descriptor; retried next second.
                if (failing.compareAndSet(false, true)) LOGGER.warn("BlockOps could not refresh automatic discovery");
            }
        }, 0, 1, TimeUnit.SECONDS);
    }

    private static synchronized void stopDiscovery() {
        if (discoveryTimer != null) { discoveryTimer.shutdownNow(); discoveryTimer = null; }
        if (discovery != null) { discovery.close(); discovery = null; }
    }

    private static int openWorldPort(WorldIdentity<IntegratedServer> watchedWorld) {
        MinecraftClient client = MinecraftClient.getInstance();
        CompletableFuture<Integer> result = new CompletableFuture<>();
        AtomicBoolean pending = new AtomicBoolean(true);
        client.execute(() -> {
            if (!pending.compareAndSet(true, false)) return;
            var server = client.getServer();
            if (client.world == null || server == null || !server.isRemote()) { result.complete(0); return; }
            int port = server.getServerPort();
            if (port < 1 || port > 65535) { result.complete(0); return; }
            if (!watchedWorld.accepts(server)) { result.complete(0); return; }
            result.complete(port);
        });
        try { return result.get(3, TimeUnit.SECONDS); }
        catch (Exception failure) { pending.set(false); return 0; }
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
