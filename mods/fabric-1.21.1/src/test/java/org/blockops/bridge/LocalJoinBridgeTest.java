package org.blockops.bridge;

import java.net.URI;
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
    private static void check(boolean ok, String message) {
        if (!ok) throw new AssertionError(message);
    }

    public static void main(String[] args) throws Exception {
        var received = new AtomicInteger();
        var clock = new AtomicLong();
        try (var bridge = new LocalJoinBridge(received::set, Duration.ofMinutes(1), clock::get)) {
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
        try (var bridge = new LocalJoinBridge(received::set, Duration.ofSeconds(1), clock::get)) {
            clock.addAndGet(Duration.ofSeconds(1).toNanos());
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "expired capability accepted");
        }
        var calls = new AtomicInteger();
        try (var bridge = new LocalJoinBridge(port -> calls.incrementAndGet(), Duration.ofSeconds(30))) {
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
            try (var bridge = new LocalJoinBridge(received::set, Duration.ofSeconds(30))) {
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
        System.out.println("Local bridge tests passed.");
    }
}
