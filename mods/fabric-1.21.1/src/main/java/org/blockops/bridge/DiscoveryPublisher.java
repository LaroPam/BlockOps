package org.blockops.bridge;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Callable;
import java.util.function.LongSupplier;

/**
 * Keeps one unused bridge advertised through a private descriptor, so the desktop on the
 * same computer can find Minecraft without a copied code. A spent or nearly expired bridge
 * is replaced; a replaced bridge stays open only while it still serves a world watch.
 */
public final class DiscoveryPublisher implements AutoCloseable {
    /** Where the descriptor goes; it must become readable by this user only before any secret is written. */
    public interface Store {
        void publish(byte[] descriptor) throws IOException;
        void remove();
    }

    static final Duration MARGIN = Duration.ofSeconds(30);
    private final Callable<LocalJoinBridge> factory;
    private final Store store;
    private final long pid;
    private final LongSupplier epochSeconds;
    private final List<LocalJoinBridge> retired = new ArrayList<>();
    private LocalJoinBridge current;
    private boolean closed;

    public DiscoveryPublisher(Callable<LocalJoinBridge> factory, Store store) {
        this(factory, store, ProcessHandle.current().pid(), () -> System.currentTimeMillis() / 1000);
    }

    DiscoveryPublisher(Callable<LocalJoinBridge> factory, Store store, long pid, LongSupplier epochSeconds) {
        if (factory == null || store == null || pid <= 0 || pid > 0xffffffffL || epochSeconds == null) {
            throw new IllegalArgumentException("Invalid discovery publisher");
        }
        this.factory = factory;
        this.store = store;
        this.pid = pid;
        this.epochSeconds = epochSeconds;
    }

    /** Call periodically. Replaces the advertised bridge once it is used or about to expire. */
    public synchronized void refresh() throws Exception {
        if (closed) return;
        retired.removeIf(LocalJoinBridge::isClosed);
        if (current != null && !current.isSpent() && current.remaining().compareTo(MARGIN) > 0) return;
        LocalJoinBridge next = factory.call();
        try {
            long expiresAt = epochSeconds.getAsLong() + next.remaining().minus(MARGIN).toSeconds();
            String json = "{\"version\":1,\"pid\":" + pid + ",\"port\":" + next.port() + ",\"token\":\""
                + next.token() + "\",\"expires_at\":" + expiresAt + "}";
            store.publish(json.getBytes(StandardCharsets.US_ASCII));
        } catch (Exception | Error failure) {
            next.close();
            throw failure;
        }
        if (current != null) retired.add(current);
        current = next;
    }

    /**
     * Delete descriptors left by crashed games. A running game rewrites its own at least
     * every five minutes, so anything older than ten minutes is stale.
     */
    public static void removeStale(Path directory, long nowMillis) {
        long cutoff = nowMillis - Duration.ofMinutes(10).toMillis();
        try (var files = Files.newDirectoryStream(directory, "minecraft-*.{json,tmp}")) {
            for (Path file : files) {
                try {
                    if (Files.getLastModifiedTime(file, LinkOption.NOFOLLOW_LINKS).toMillis() < cutoff) {
                        Files.deleteIfExists(file);
                    }
                } catch (IOException ignored) {
                    // Another game may own or be replacing it; the desktop ignores expired files.
                }
            }
        } catch (IOException ignored) {
            // Cleanup is best effort.
        }
    }

    @Override public synchronized void close() {
        if (closed) return;
        closed = true;
        store.remove();
        if (current != null) current.close();
        retired.forEach(LocalJoinBridge::close);
        retired.clear();
        current = null;
    }
}
