package org.blockops.bridge;

import java.net.URI;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.HexFormat;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;
import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

/** Executable without Minecraft, Fabric, third-party test frameworks, or internet access. */
public final class LocalJoinBridgeTest {
    private static final HttpClient HTTP = HttpClient.newBuilder()
        .version(HttpClient.Version.HTTP_1_1).connectTimeout(Duration.ofSeconds(3)).build();
    private static final SecureRandom RANDOM = new SecureRandom();

    static String nonce() {
        byte[] bytes = new byte[32];
        RANDOM.nextBytes(bytes);
        return HexFormat.of().formatHex(bytes);
    }

    /** Independent implementation of the desktop's side of the protocol. */
    static String mac(String token, String direction, String path, String nonce, String value) throws Exception {
        Mac mac = Mac.getInstance("HmacSHA256");
        mac.init(new SecretKeySpec(HexFormat.of().parseHex(token), "HmacSHA256"));
        return HexFormat.of().formatHex(mac.doFinal(("blockops-bridge-" + direction + "-v1\n" + path + "\n" + nonce
            + "\n" + value).getBytes(StandardCharsets.US_ASCII)));
    }

    /** A request signed with {@code token}, binding the nonce and {@code signedPort}. */
    static HttpRequest.Builder signed(int bridgePort, String path, String token, String nonce, String signedPort)
            throws Exception {
        String bare = path.contains("?") ? path.substring(0, path.indexOf('?')) : path;
        return HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + bridgePort + path))
            .timeout(Duration.ofSeconds(8))
            .header("Authorization", "BlockOps-HMAC " + mac(token, "request", bare, nonce, signedPort))
            .header("X-BlockOps-Nonce", nonce)
            .POST(HttpRequest.BodyPublishers.noBody());
    }

    private static HttpResponse<Void> join(LocalJoinBridge bridge, String path, String token, String port, String origin)
            throws Exception {
        var builder = signed(bridge.port(), path, token, nonce(), port).header("X-BlockOps-Port", port);
        if (origin != null) builder.header("Origin", origin);
        return HTTP.send(builder.build(), HttpResponse.BodyHandlers.discarding());
    }
    private static int request(LocalJoinBridge bridge, String path, String token, String port, String origin) throws Exception {
        return join(bridge, path, token, port, origin).statusCode();
    }
    static HttpResponse<String> world(int bridgePort, String token, String nonce) throws Exception {
        return HTTP.send(signed(bridgePort, "/v1/world", token, nonce, "").build(), HttpResponse.BodyHandlers.ofString());
    }
    private static HttpResponse<String> world(LocalJoinBridge bridge, String token) throws Exception {
        return world(bridge.port(), token, nonce());
    }
    private static HttpResponse<java.io.InputStream> watch(LocalJoinBridge bridge, String nonce) throws Exception {
        return HTTP.send(signed(bridge.port(), "/v1/watch-world", bridge.token(), nonce, "").build(),
            HttpResponse.BodyHandlers.ofInputStream());
    }
    static void check(boolean ok, String message) {
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
            String nonce = nonce();
            var unsigned = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + bridge.port() + "/v1/join"))
                .header("Authorization", "Bearer " + bridge.token()).header("X-BlockOps-Nonce", nonce)
                .header("X-BlockOps-Port", "25565").POST(HttpRequest.BodyPublishers.noBody()).build();
            check(HTTP.send(unsigned, HttpResponse.BodyHandlers.discarding()).statusCode() == 403, "bearer capability accepted");
            var noNonce = HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + bridge.port() + "/v1/join"))
                .header("Authorization", "BlockOps-HMAC " + mac(bridge.token(), "request", "/v1/join", "", "25565"))
                .header("X-BlockOps-Port", "25565").POST(HttpRequest.BodyPublishers.noBody()).build();
            check(HTTP.send(noNonce, HttpResponse.BodyHandlers.discarding()).statusCode() == 400, "missing nonce accepted");
            var otherPort = signed(bridge.port(), "/v1/join", bridge.token(), nonce, "25566")
                .header("X-BlockOps-Port", "25565").build();
            check(HTTP.send(otherPort, HttpResponse.BodyHandlers.discarding()).statusCode() == 403, "port not bound by signature");
            check(received.get() == 0, "rejected request reached callback");
            var accepted = HTTP.send(signed(bridge.port(), "/v1/join", bridge.token(), nonce, "25565")
                .header("X-BlockOps-Port", "25565").build(), HttpResponse.BodyHandlers.discarding());
            check(accepted.statusCode() == 202, "valid request rejected");
            check(accepted.headers().firstValue("X-BlockOps-Proof").orElse("")
                .equals(mac(bridge.token(), "response", "/v1/join", nonce, "")), "join response not proven");
            check(received.get() == 25565, "valid port not delivered");
            check(request(bridge, "/v1/join", bridge.token(), "12345", null) == 403, "replay accepted");
            check(received.get() == 25565, "replay reached callback");
        }
        try (var bridge = new LocalJoinBridge(port -> { received.set(port); return true; }, Duration.ofSeconds(100), clock::get)) {
            clock.addAndGet(Duration.ofSeconds(100).toNanos());
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "expired capability accepted");
        }
        try (var bridge = new LocalJoinBridge(port -> false, Duration.ofSeconds(30))) {
            var refused = join(bridge, "/v1/join", bridge.token(), "25565", null);
            check(refused.statusCode() == 409, "unavailable game accepted request");
            check(refused.headers().firstValue("X-BlockOps-Proof").isEmpty(), "refusal carried a success proof");
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "failed one-use request was replayed");
        }
        var worldCalls = new AtomicInteger();
        try (var bridge = new LocalJoinBridge(port -> false, () -> { worldCalls.incrementAndGet(); return 43210; }, Duration.ofSeconds(30))) {
            check(world(bridge, "0".repeat(64)).statusCode() == 403, "wrong token exposed the world port");
            check(worldCalls.get() == 0, "unauthorized world request reached callback");
            String nonce = nonce();
            var response = world(bridge.port(), bridge.token(), nonce);
            check(response.statusCode() == 200 && response.body().equals("43210"), "world port not returned");
            check(response.headers().firstValue("X-BlockOps-Proof").orElse("")
                .equals(mac(bridge.token(), "response", "/v1/world", nonce, "43210")), "world port not proven");
            check(world(bridge, bridge.token()).statusCode() == 403, "world request replay accepted");
            check(request(bridge, "/v1/join", bridge.token(), "25565", null) == 403, "world token reused to join");
        }
        try (var bridge = new LocalJoinBridge(port -> false, () -> 0, Duration.ofSeconds(30))) {
            check(world(bridge, bridge.token()).statusCode() == 409, "closed world reported as open");
        }
        var watchedPort = new AtomicInteger(43210);
        try (var bridge = new LocalJoinBridge(port -> false, watchedPort::get, Duration.ofSeconds(30))) {
            String nonce = nonce();
            var response = watch(bridge, nonce);
            check(response.statusCode() == 200, "watch rejected");
            check(response.headers().firstValue("X-BlockOps-Proof").orElse("")
                .equals(mac(bridge.token(), "response", "/v1/watch-world", nonce, "43210")), "watched port not proven");
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
        var identity = new WorldIdentity<Object>();
        Object firstWorld = new Object();
        Object secondWorld = new Object();
        var currentWorld = new AtomicReference<>(firstWorld);
        try (var bridge = new LocalJoinBridge(port -> false,
                () -> identity.accepts(currentWorld.get()) ? 43210 : 0, Duration.ofSeconds(30))) {
            var response = watch(bridge, nonce());
            try (var reader = new BufferedReader(new InputStreamReader(response.body(), StandardCharsets.US_ASCII))) {
                check("W 43210".equals(reader.readLine()), "missing initial world port");
                currentWorld.set(secondWorld); // A new world using the same port must not retain access.
                var result = java.util.concurrent.CompletableFuture.supplyAsync(() -> {
                    try { String line; while ("A".equals(line = reader.readLine())) {} return line; }
                    catch (Exception failure) { throw new RuntimeException(failure); }
                });
                check("C".equals(result.get(5, TimeUnit.SECONDS)), "different world with same port was not closed");
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
