package org.blockops.forge;

import java.io.IOException;
import java.time.Duration;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.components.Button;
import net.minecraft.client.gui.screens.ConnectScreen;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.gui.screens.TitleScreen;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import net.minecraft.client.server.IntegratedServer;
import net.minecraft.commands.Commands;
import net.minecraft.network.chat.Component;
import net.minecraftforge.client.event.RegisterClientCommandsEvent;
import net.minecraftforge.client.event.ScreenEvent;
import net.minecraftforge.common.MinecraftForge;
import net.minecraftforge.event.GameShuttingDownEvent;
import net.minecraftforge.event.TickEvent;
import org.blockops.bridge.DiscoveryPublisher;
import org.blockops.bridge.LocalJoinBridge;
import org.blockops.bridge.WindowsDescriptorFile;
import org.blockops.bridge.WorldIdentity;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/**
 * The Forge companion on the client, behaving as the Fabric one: on Windows it advertises a
 * one-use bridge through a private descriptor; `/blockops` and a title-screen button open a
 * bridge for a copied code as a fallback. Loaded only by {@link BlockOpsForge} on a client.
 */
final class BlockOpsForgeClient {
    private static final Logger LOGGER = LoggerFactory.getLogger("blockops");
    private static LocalJoinBridge bridge;
    private static DiscoveryPublisher discovery;
    private static ScheduledExecutorService discoveryTimer;
    private static final AtomicBoolean openPairing = new AtomicBoolean();
    /** Observed on the client thread, read by the discovery thread. */
    private static volatile DiscoveryPublisher.GameState gameState = DiscoveryPublisher.GameState.BUSY;

    private BlockOpsForgeClient() {}

    static void init() {
        startDiscovery();
        MinecraftForge.EVENT_BUS.addListener(BlockOpsForgeClient::onTick);
        MinecraftForge.EVENT_BUS.addListener(BlockOpsForgeClient::onCommands);
        MinecraftForge.EVENT_BUS.addListener(BlockOpsForgeClient::onScreen);
        MinecraftForge.EVENT_BUS.addListener(BlockOpsForgeClient::onShutdown);
    }

    private static void onTick(TickEvent.ClientTickEvent event) {
        if (event.phase != TickEvent.Phase.END) return;
        Minecraft client = Minecraft.getInstance();
        if (openPairing.compareAndSet(true, false)) client.setScreen(new PairingScreen(null));
        gameState = observeState(client);
    }

    private static void onCommands(RegisterClientCommandsEvent event) {
        event.getDispatcher().register(Commands.literal("blockops").executes(context -> {
            openPairing.set(true);
            return 1;
        }));
    }

    private static void onScreen(ScreenEvent.Init.Post event) {
        if (event.getScreen() instanceof TitleScreen title) {
            event.addListener(Button.builder(Component.literal("BlockOps"),
                button -> Minecraft.getInstance().setScreen(new PairingScreen(title)))
                .bounds(title.width - 92, 8, 84, 20).build());
        }
    }

    private static void onShutdown(GameShuttingDownEvent event) {
        stopPairing();
        stopDiscovery();
    }

    static synchronized String startPairing() throws IOException {
        stopPairing();
        WorldIdentity<IntegratedServer> watchedWorld = new WorldIdentity<>();
        bridge = new LocalJoinBridge(BlockOpsForgeClient::startJoin, () -> openWorldPort(watchedWorld), Duration.ofMinutes(5));
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
                return new LocalJoinBridge(BlockOpsForgeClient::startJoin, () -> openWorldPort(watchedWorld), Duration.ofMinutes(5));
            }, () -> gameState, store);
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

    /** Mirrors the checks of {@link #openWorldPort} and {@link #startJoin}, which still decide. */
    private static DiscoveryPublisher.GameState observeState(Minecraft client) {
        IntegratedServer server = client.getSingleplayerServer();
        if (client.level != null && server != null && server.isPublished()) return DiscoveryPublisher.GameState.LAN;
        if (client.level == null && client.allowsMultiplayer() && joinableScreen(client.screen)) {
            return DiscoveryPublisher.GameState.MENU;
        }
        return DiscoveryPublisher.GameState.BUSY;
    }

    private static boolean joinableScreen(Screen screen) {
        return screen instanceof TitleScreen || screen instanceof JoinMultiplayerScreen || screen instanceof PairingScreen;
    }

    private static int openWorldPort(WorldIdentity<IntegratedServer> watchedWorld) {
        Minecraft client = Minecraft.getInstance();
        CompletableFuture<Integer> result = new CompletableFuture<>();
        AtomicBoolean pending = new AtomicBoolean(true);
        client.execute(() -> {
            if (!pending.compareAndSet(true, false)) return;
            IntegratedServer server = client.getSingleplayerServer();
            if (client.level == null || server == null || !server.isPublished()) { result.complete(0); return; }
            int port = server.getPort();
            if (port < 1 || port > 65535 || !watchedWorld.accepts(server)) { result.complete(0); return; }
            result.complete(port);
        });
        try { return result.get(3, TimeUnit.SECONDS); }
        catch (Exception failure) { pending.set(false); return 0; }
    }

    private static boolean startJoin(int port) {
        Minecraft client = Minecraft.getInstance();
        CompletableFuture<Boolean> result = new CompletableFuture<>();
        AtomicBoolean pending = new AtomicBoolean(true);
        client.execute(() -> {
            if (!pending.compareAndSet(true, false)) return;
            if (!client.allowsMultiplayer() || client.level != null || !joinableScreen(client.screen)) {
                result.complete(false);
                return;
            }
            try {
                String address = "127.0.0.1:" + port;
                ServerData info = new ServerData("BlockOps", address, false);
                Screen parent = client.screen instanceof PairingScreen screen ? screen.parentScreen() : client.screen;
                ConnectScreen.startConnecting(parent, client, ServerAddress.parseString(address), info, false);
                result.complete(true);
            } catch (RuntimeException failure) {
                result.complete(false);
            }
        });
        try { return result.get(3, TimeUnit.SECONDS); }
        catch (Exception failure) { pending.set(false); return false; }
    }
}
