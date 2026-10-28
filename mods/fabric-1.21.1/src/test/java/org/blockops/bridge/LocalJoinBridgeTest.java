package org.blockops.bridge;

import java.net.URI;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;

/** Executable without Minecraft, Fabric, third-party test frameworks, or internet access. */
public final class LocalJoinBridgeTest {
    private static final HttpClient HTTP = HttpClient.newBuilder()
        .version(HttpClient.Version.HTTP_1_1).connectTimeout(Duration.ofSeconds(3)).build();

    private static int request(LocalJoinBridge bridge, String path, String token, String port, String origin) throws Exception {
        var builder = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + bridge.port() + path))
            .timeout(Duration.ofSeconds(3)).header("Authorization", "Bearer " + token)
            .header("X-BlockOps-Port", port).POST(HttpRequest.BodyPublishers.noBody());
        if (origin != null) builder.header("Origin", origin);
        return HTTP.send(builder.build(), HttpResponse.BodyHandlers.discarding()).statusCode();
    }
    private static HttpResponse<String> world(LocalJoinBridge bridge, String token) throws Exception {
        var request = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + bridge.port() + "/v1/world"))
            .timeout(Duration.ofSeconds(3)).header("Authorization", "Bearer " + token)
            .POST(HttpRequest.BodyPublishers.noBody()).build();
        return HTTP.send(request, HttpResponse.BodyHandlers.ofString());
    }
    private static void check(boolean ok, String message) {
        if (!ok) throw new AssertionError(message);
    }

    public static void main(String[] args) throws Exception {
        var received = new AtomicInteger();
        var clock = new AtomicLong();
        try (var bridge = new LocalJoinBridge(port -> { received.set(port); return true; }, Duration.ofMinutes(1), clock::get)) {
            check(bridge.token().matches("[0-9a-f]{64}"), "capability is not 256-bit hex");
            check(request(bridge, "/v1/join", "0".repeat(64), "25565", null) == 403, "wrong token accepted");
            check(request(bridge, "/v1/join", bridge.token(), "0", null) == 400, "zero port accepted");
            check(request(bridge, "/v1/join", bridge.token(), "65536", null) == 400, "out-of-range port accepted");
            check(request(bridge, "/v1/join", bridge.token(), "127.0.0.1:25565", null) == 400, "address accepted in port");
            check(request(bridge, "/v1/join?x=1", bridge.token(), "25565", null) == 400, "query accepted");
            check(request(bridge, "/v1/join", bridge.token(), "25565", "https://example.org") == 400, "browser origin accepted");
            check(received.get() == 0, "rejected request reached callback");
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 202, "valid request rejected");
            check(received.get() == 25565, "valid port not delivered");
            check(request(bridge, "/v1/join", bridge.token(), "12345", null) == 403, "replay accepted");
            check(received.get() == 25565, "replay reached callback");
        }
        try (var bridge = new LocalJoinBridge(port -> { received.set(port); return true; }, Duration.ofSeconds(100), clock::get)) {
            clock.addAndGet(Duration.ofSeconds(100).toNanos());
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "expired capability accepted");
        }
        try (var bridge = new LocalJoinBridge(port -> false, Duration.ofSeconds(30))) {
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 409, "unavailable game accepted request");
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "failed one-use request was replayed");
        }
        var worldCalls = new AtomicInteger();
        try (var bridge = new LocalJoinBridge(port -> false, () -> { worldCalls.incrementAndGet(); return 43210; }, Duration.ofSeconds(30))) {
            check(world(bridge, "0".repeat(64)).statusCode() == 403, "wrong token exposed the world port");
            check(worldCalls.get() == 0, "unauthorized world request reached callback");
            var response = world(bridge, bridge.token());
            check(response.statusCode() == 200 && response.body().equals("43210"), "world port not returned");
            check(world(bridge, bridge.token()).statusCode() == 403, "world request replay accepted");
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "world token reused to join");
        }
        try (var bridge = new LocalJoinBridge(port -> false, () -> 0, Duration.ofSeconds(30))) {
            check(world(bridge, bridge.token()).statusCode() == 409, "closed world reported as open");
        }
        var watchedPort = new AtomicInteger(43210);
        try (var bridge = new LocalJoinBridge(port -> false, watchedPort::get, Duration.ofSeconds(30))) {
            var request = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + bridge.port() + "/v1/watch-world"))
                .timeout(Duration.ofSeconds(8)).header("Authorization", "Bearer " + bridge.token())
                .POST(HttpRequest.BodyPublishers.noBody()).build();
            var response = HTTP.send(request, HttpResponse.BodyHandlers.ofInputStream());
            check(response.statusCode() == 200, "watch rejected");
            try (var reader = new BufferedReader(new InputStreamReader(response.body(), StandardCharsets.US_ASCII))) {
                check("W 43210".equals(reader.readLine()), "missing initial world port");
                watchedPort.set(0);
                var result = java.util.concurrent.CompletableFuture.supplyAsync(() -> {
                    try {
                        String line;
                        while ("A".equals(line = reader.readLine())) { /* buffered heartbeat */ }
                        return line;
                    } catch (Exception failure) { throw new RuntimeException(failure); }
                });
                check("C".equals(result.get(5, TimeUnit.SECONDS)), "world closure not reported");
            }
        }
        var calls = new AtomicInteger();
        try (var bridge = new LocalJoinBridge(port -> { calls.incrementAndGet(); return true; }, Duration.ofSeconds(30))) {
            var one = java.util.concurrent.CompletableFuture.supplyAsync(() -> {
                try { return request(bridge, "/v1/join", bridge.token(), "12345", null); }
                catch (Exception error) { throw new RuntimeException(error); }
            });
            var two = java.util.concurrent.CompletableFuture.supplyAsync(() -> {
                try { return request(bridge, "/v1/join", bridge.token(), "12345", null); }
                catch (Exception error) { throw new RuntimeException(error); }
            });
            int first = one.get(5, TimeUnit.SECONDS);
            int second = two.get(5, TimeUnit.SECONDS);
            check((first == 202 && second == 403) || (first == 403 && second == 202), "concurrent requests were not one-use");
            check(calls.get() == 1, "callback was not exactly once");
        }
        String rustClient = System.getenv("BLOCKOPS_BRIDGE_TEST_CLIENT");
        if (rustClient != null && !rustClient.isBlank()) {
            received.set(0);
            try (var bridge = new LocalJoinBridge(port -> { received.set(port); return true; }, Duration.ofSeconds(30))) {
                // The test-only capability travels through stdin, never a process argument or log.
                Process process = new ProcessBuilder(rustClient, Integer.toString(bridge.port()), "34567")
                    .redirectErrorStream(true).start();
                try {
                    try (var stdin = process.getOutputStream()) {
                        stdin.write((bridge.token() + "\n").getBytes(StandardCharsets.US_ASCII));
                    }
                    check(process.waitFor(10, TimeUnit.SECONDS), "Rust client timed out");
                    if (process.exitValue() != 0) {
                        String output = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8)
                            .replace(bridge.token(), "[redacted]");
                        throw new AssertionError("Rust client failed: " + output);
                    }
                    check(received.get() == 34567, "Rust request did not reach Java callback");
                } finally {
                    if (process.isAlive()) process.destroyForcibly();
                }
            }
        }
        if (rustClient != null && !rustClient.isBlank()) {
            try (var bridge = new LocalJoinBridge(port -> false, () -> 43210, Duration.ofSeconds(30))) {
                Process process = new ProcessBuilder(rustClient, Integer.toString(bridge.port()), "43210", "world").redirectErrorStream(true).start();
                try {
                    try (var stdin = process.getOutputStream()) {
                        stdin.write((bridge.token() + "\n").getBytes(StandardCharsets.US_ASCII));
                    }
                    check(process.waitFor(10, TimeUnit.SECONDS), "Rust world detection timed out");
                    if (process.exitValue() != 0) {
                        String output = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8).replace(bridge.token(), "[redacted]");
                        throw new AssertionError("Rust world detection failed: " + output);
                    }
                } finally { if (process.isAlive()) process.destroyForcibly(); }
            }
        }
        if (rustClient != null && !rustClient.isBlank()) {
            var port = new AtomicInteger(43210);
            var callsToWorld = new AtomicInteger();
            try (var bridge = new LocalJoinBridge(value -> false, () -> { int current = port.get(); callsToWorld.incrementAndGet(); return current; }, Duration.ofSeconds(30))) {
                Process process = new ProcessBuilder(rustClient, Integer.toString(bridge.port()), "43210", "watch").redirectErrorStream(true).start();
                try {
                    try (var stdin = process.getOutputStream()) { stdin.write((bridge.token() + "\n").getBytes(StandardCharsets.US_ASCII)); }
                    long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5);
                    while (callsToWorld.get() == 0 && System.nanoTime() < deadline) Thread.sleep(20);
                    check(callsToWorld.get() > 0, "Rust watch did not start");
                    port.set(0);
                    check(process.waitFor(10, TimeUnit.SECONDS), "Rust watch did not observe closure");
                    if (process.exitValue() != 0) {
                        String output = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8).replace(bridge.token(), "[redacted]");
                        throw new AssertionError("Rust watch failed: " + output);
                    }
                } finally { if (process.isAlive()) process.destroyForcibly(); }
            }
        }
        System.out.println("Local bridge tests passed.");
    }
}
