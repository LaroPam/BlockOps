package org.blockops.bridge;

import static org.blockops.bridge.LocalJoinBridgeTest.check;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.net.http.HttpClient;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.attribute.FileTime;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** Executable without Minecraft, Fabric, Windows or internet access. */
public final class DiscoveryPublisherTest {
    private static final Pattern DESCRIPTOR = Pattern.compile("\\{\"version\":2,\"pid\":4242,\"port\":([0-9]+),"
        + "\"token\":\"([0-9a-f]{64})\",\"expires_at\":([0-9]+),\"state\":\"(lan|menu|busy)\"}");

    private static final class MemoryStore implements DiscoveryPublisher.Store {
        String descriptor;
        boolean failing;
        boolean removed;
        int publications;

        @Override public void publish(byte[] bytes) throws IOException {
            if (failing) throw new IOException("synthetic failure");
            descriptor = new String(bytes, StandardCharsets.US_ASCII);
            publications++;
        }

        @Override public void remove() { removed = true; descriptor = null; }

        Matcher parsed() {
            Matcher matcher = DESCRIPTOR.matcher(descriptor);
            check(matcher.matches(), "descriptor has an unexpected shape");
            return matcher;
        }
    }

    public static void main(String[] args) throws Exception {
        var clock = new AtomicLong();
        var bridges = new ArrayList<LocalJoinBridge>();
        var store = new MemoryStore();
        var state = new AtomicReference<DiscoveryPublisher.GameState>(DiscoveryPublisher.GameState.MENU);
        var publisher = new DiscoveryPublisher(() -> {
            var bridge = new LocalJoinBridge(port -> false, () -> 43210, Duration.ofMinutes(5), clock::get);
            bridges.add(bridge);
            return bridge;
        }, state::get, store, 4242, () -> 1_000_000L);

        publisher.refresh();
        Matcher first = store.parsed();
        check(Integer.parseInt(first.group(1)) == bridges.get(0).port(), "descriptor names another port");
        check(first.group(2).equals(bridges.get(0).token()), "descriptor names another capability");
        check(Long.parseLong(first.group(3)) == 1_000_000L + 270, "expiry does not leave a safety margin");
        check(first.group(4).equals("menu"), "descriptor reports another game state");
        publisher.refresh();
        check(store.publications == 1 && bridges.size() == 1, "fresh bridge was replaced");

        // A state change is announced at once, without spending the advertised bridge.
        state.set(DiscoveryPublisher.GameState.LAN);
        publisher.refresh();
        Matcher changed = store.parsed();
        check(store.publications == 2 && bridges.size() == 1, "state change replaced the bridge");
        check(changed.group(4).equals("lan") && changed.group(2).equals(bridges.get(0).token()),
            "state change was not announced for the same bridge");
        state.set(null);
        publisher.refresh();
        check(store.parsed().group(4).equals("busy"), "unknown state was not reported as busy");
        state.set(DiscoveryPublisher.GameState.LAN);
        publisher.refresh();
        check(store.publications == 4 && bridges.size() == 1, "state changes spent a bridge");

        // A used bridge is replaced at once and never advertised again.
        var response = LocalJoinBridgeTest.world(bridges.get(0).port(), bridges.get(0).token(), LocalJoinBridgeTest.nonce());
        check(response.statusCode() == 200, "advertised bridge rejected the desktop");
        publisher.refresh();
        check(bridges.size() == 2 && store.publications == 5, "spent bridge was not replaced");
        check(store.parsed().group(2).equals(bridges.get(1).token()), "descriptor still names the spent bridge");

        // A bridge close to expiry is replaced before the desktop can pick it.
        clock.addAndGet(Duration.ofMinutes(5).minus(DiscoveryPublisher.MARGIN).toNanos());
        publisher.refresh();
        check(bridges.size() == 3 && store.publications == 6, "expiring bridge was not replaced");

        // A failed publication closes the unadvertised bridge and keeps the previous one.
        store.failing = true;
        clock.addAndGet(Duration.ofMinutes(5).toNanos());
        try {
            publisher.refresh();
            throw new AssertionError("store failure was hidden");
        } catch (IOException expected) {
            check(bridges.get(3).isClosed(), "unpublished bridge stayed open");
        }
        store.failing = false;
        clock.set(0);
        bridges.get(2).close();
        publisher.refresh();
        check(bridges.size() == 5 && store.publications == 7, "publisher did not recover");

        // A replaced bridge that serves a world watch stays open until the publisher closes.
        var watched = bridges.get(4);
        var watch = HttpClient.newHttpClient().send(LocalJoinBridgeTest.signed(watched.port(), "/v1/watch-world",
            watched.token(), LocalJoinBridgeTest.nonce(), "").build(), HttpResponse.BodyHandlers.ofInputStream());
        check(watch.statusCode() == 200, "watch rejected");
        var reader = new BufferedReader(new InputStreamReader(watch.body(), StandardCharsets.US_ASCII));
        check("W 43210".equals(reader.readLine()), "missing initial world port");
        publisher.refresh();
        check(bridges.size() == 6 && !watched.isClosed(), "rotation interrupted an active world watch");
        var ended = new AtomicBoolean();
        var drain = CompletableFuture.runAsync(() -> {
            try { while (reader.readLine() != null) { /* heartbeats */ } } catch (IOException closed) { /* expected */ }
            ended.set(true);
        });
        publisher.close();
        drain.get(5, TimeUnit.SECONDS);
        check(ended.get() && store.removed, "close did not withdraw the descriptor and end the watch");
        for (LocalJoinBridge bridge : bridges) check(bridge.isClosed(), "a bridge outlived the publisher");
        publisher.refresh();
        check(bridges.size() == 6, "closed publisher created a bridge");

        var directory = Files.createTempDirectory("blockops-discovery");
        try {
            long now = System.currentTimeMillis();
            var stale = Files.writeString(directory.resolve("minecraft-1.json"), "{}");
            var staging = Files.writeString(directory.resolve("minecraft-1.tmp"), "{}");
            var fresh = Files.writeString(directory.resolve("minecraft-2.json"), "{}");
            var unrelated = Files.writeString(directory.resolve("notes.json"), "{}");
            FileTime old = FileTime.fromMillis(now - Duration.ofMinutes(11).toMillis());
            for (var file : List.of(stale, staging, unrelated)) Files.setLastModifiedTime(file, old);
            DiscoveryPublisher.removeStale(directory, now);
            check(!Files.exists(stale) && !Files.exists(staging), "stale descriptors were kept");
            check(Files.exists(fresh) && Files.exists(unrelated), "cleanup removed a live or unrelated file");
        } finally {
            try (var files = Files.list(directory)) { for (var file : files.toList()) Files.delete(file); }
            Files.delete(directory);
        }
        System.out.println("Discovery publisher tests passed.");
    }
}
