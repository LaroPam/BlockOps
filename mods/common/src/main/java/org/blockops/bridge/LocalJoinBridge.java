package org.blockops.bridge;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.HexFormat;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.IntPredicate;
import java.util.function.IntSupplier;
import java.util.function.LongSupplier;
import java.util.regex.Pattern;
import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

/**
 * An explicitly started, short-lived one-use local HTTP capability.
 *
 * <p>The secret never travels over the socket. A request carries a fresh client nonce and
 * HMAC-SHA256 over its path, nonce and port; a successful response carries an HMAC over
 * its path, nonce and payload, so the desktop can tell this bridge from a process that
 * later reuses the port.
 */
public final class LocalJoinBridge implements AutoCloseable {
    private static final Pattern HEX_256 = Pattern.compile("[0-9a-f]{64}");
    private final HttpServer server;
    private final ThreadPoolExecutor worker;
    private final ScheduledExecutorService expiry;
    private ScheduledFuture<?> expiryTask;
    private final AtomicBoolean closed = new AtomicBoolean();
    private final byte[] key;
    private final long startedNanos;
    private final long lifetimeNanos;
    private final LongSupplier ticker;
    private final AtomicBoolean used = new AtomicBoolean();
    private final IntPredicate onJoin;
    private final IntSupplier onWorld;

    public LocalJoinBridge(IntPredicate onJoin, Duration lifetime) throws IOException {
        this(onJoin, () -> 0, lifetime, System::nanoTime);
    }

    public LocalJoinBridge(IntPredicate onJoin, IntSupplier onWorld, Duration lifetime) throws IOException {
        this(onJoin, onWorld, lifetime, System::nanoTime);
    }

    LocalJoinBridge(IntPredicate onJoin, Duration lifetime, LongSupplier ticker) throws IOException {
        this(onJoin, () -> 0, lifetime, ticker);
    }

    LocalJoinBridge(IntPredicate onJoin, IntSupplier onWorld, Duration lifetime, LongSupplier ticker) throws IOException {
        if (onJoin == null || onWorld == null || ticker == null || lifetime == null || lifetime.isNegative() || lifetime.isZero()
                || lifetime.compareTo(Duration.ofMinutes(5)) > 0) {
            throw new IllegalArgumentException("Invalid bridge lifetime or callback");
        }
        this.onJoin = onJoin;
        this.onWorld = onWorld;
        this.ticker = ticker;
        this.startedNanos = ticker.getAsLong();
        this.lifetimeNanos = lifetime.toNanos();
        this.key = new byte[32];
        new SecureRandom().nextBytes(key);
        this.server = HttpServer.create(new InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0), 8);
        this.worker = new ThreadPoolExecutor(1, 1, 0L, TimeUnit.MILLISECONDS,
            new ArrayBlockingQueue<>(16), runnable -> {
                Thread thread = new Thread(runnable, "blockops-local-bridge");
                thread.setDaemon(true);
                return thread;
            }, new ThreadPoolExecutor.AbortPolicy());
        this.expiry = Executors.newSingleThreadScheduledExecutor(runnable -> {
            Thread thread = new Thread(runnable, "blockops-bridge-expiry");
            thread.setDaemon(true);
            return thread;
        });
        server.setExecutor(worker);
        server.createContext("/v1/join", this::handle);
        server.createContext("/v1/world", this::handle);
        server.createContext("/v1/watch-world", this::handle);
        server.start();
        expiryTask = expiry.schedule(this::close, lifetime.toNanos(), TimeUnit.NANOSECONDS);
    }

    public int port() { return server.getAddress().getPort(); }

    /** Secret: the caller must deliver this through a trusted local pairing mechanism. */
    public String token() { return HexFormat.of().formatHex(key); }

    /** Whether the listener has stopped; a watch keeps it open after the bridge is spent. */
    public boolean isClosed() { return closed.get(); }

    /** Whether this bridge can no longer accept a request. */
    public boolean isSpent() {
        return closed.get() || used.get() || ticker.getAsLong() - startedNanos >= lifetimeNanos;
    }

    /** Remaining time before expiry; zero once spent. */
    public Duration remaining() {
        if (closed.get() || used.get()) return Duration.ZERO;
        return Duration.ofNanos(Math.max(0, lifetimeNanos - (ticker.getAsLong() - startedNanos)));
    }

    private void handle(HttpExchange exchange) throws IOException {
        boolean watching = false;
        try {
            String path = exchange.getRequestURI().getRawPath();
            if (!"POST".equals(exchange.getRequestMethod())
                    || !("/v1/join".equals(path) || "/v1/world".equals(path) || "/v1/watch-world".equals(path))
                    || exchange.getRequestURI().getRawQuery() != null
                    || exchange.getRequestHeaders().containsKey("Origin")
                    || exchange.getRequestHeaders().containsKey("Transfer-Encoding")
                    || !"0".equals(exchange.getRequestHeaders().getFirst("Content-Length"))) {
                respond(exchange, 400);
                return;
            }
            String nonce = single(exchange, "X-BlockOps-Nonce");
            if (!HEX_256.matcher(nonce).matches()) { respond(exchange, 400); return; }
            String portValue = "/v1/join".equals(path) ? single(exchange, "X-BlockOps-Port") : "";
            String supplied = single(exchange, "Authorization");
            supplied = supplied.startsWith("BlockOps-HMAC ") ? supplied.substring(14) : "";
            byte[] candidate = HEX_256.matcher(supplied).matches() ? HexFormat.of().parseHex(supplied) : new byte[0];
            if (!MessageDigest.isEqual(mac("request", path, nonce, portValue), candidate)
                    || ticker.getAsLong() - startedNanos >= lifetimeNanos || used.get()) {
                respond(exchange, 403);
                return;
            }
            if ("/v1/watch-world".equals(path)) {
                if (!used.compareAndSet(false, true)) { respond(exchange, 403); return; }
                int port = onWorld.getAsInt();
                if (port < 1 || port > 65535 || !extendForWatch()) { respond(exchange, 409); return; }
                watching = true;
                exchange.getResponseHeaders().set("X-BlockOps-Proof", proof(path, nonce, Integer.toString(port)));
                exchange.getResponseHeaders().set("Cache-Control", "no-store");
                exchange.getResponseHeaders().set("Content-Type", "text/plain; charset=us-ascii");
                exchange.sendResponseHeaders(200, 0); // chunked stream
                var out = exchange.getResponseBody();
                out.write(("W " + port + "\n").getBytes(StandardCharsets.US_ASCII));
                out.flush();
                while (!closed.get()) {
                    try { Thread.sleep(1000); }
                    catch (InterruptedException interrupted) { Thread.currentThread().interrupt(); return; }
                    if (onWorld.getAsInt() != port) {
                        out.write("C\n".getBytes(StandardCharsets.US_ASCII));
                        out.flush();
                        return;
                    }
                    out.write("A\n".getBytes(StandardCharsets.US_ASCII));
                    out.flush();
                }
                return;
            }
            if ("/v1/world".equals(path)) {
                if (!used.compareAndSet(false, true)) { respond(exchange, 403); return; }
                int port = onWorld.getAsInt();
                if (port < 1 || port > 65535) { respond(exchange, 409); return; }
                byte[] body = Integer.toString(port).getBytes(StandardCharsets.US_ASCII);
                exchange.getResponseHeaders().set("X-BlockOps-Proof", proof(path, nonce, Integer.toString(port)));
                exchange.getResponseHeaders().set("Cache-Control", "no-store");
                exchange.getResponseHeaders().set("Content-Type", "text/plain; charset=us-ascii");
                exchange.sendResponseHeaders(200, body.length);
                exchange.getResponseBody().write(body);
                return;
            }
            if (!portValue.matches("[0-9]{1,5}")) { respond(exchange, 400); return; }
            int port = Integer.parseInt(portValue);
            if (port == 0 || port > 65535) { respond(exchange, 400); return; }
            if (!used.compareAndSet(false, true)) { respond(exchange, 403); return; }
            // Acceptance only means the caller started the attempt, not that Minecraft joined.
            if (!onJoin.test(port)) { respond(exchange, 409); return; }
            exchange.getResponseHeaders().set("X-BlockOps-Proof", proof(path, nonce, ""));
            respond(exchange, 202);
        } finally {
            exchange.close();
            if (watching) close();
        }
    }

    private synchronized boolean extendForWatch() {
        if (closed.get() || !expiryTask.cancel(false)) return false;
        expiryTask = expiry.schedule(this::close, 12, TimeUnit.HOURS);
        return true;
    }

    private static String single(HttpExchange exchange, String name) {
        var values = exchange.getRequestHeaders().get(name);
        return values != null && values.size() == 1 ? values.get(0) : "";
    }

    private String proof(String path, String nonce, String payload) {
        return HexFormat.of().formatHex(mac("response", path, nonce, payload));
    }

    private byte[] mac(String direction, String path, String nonce, String value) {
        try {
            Mac mac = Mac.getInstance("HmacSHA256");
            mac.init(new SecretKeySpec(key, "HmacSHA256"));
            return mac.doFinal(("blockops-bridge-" + direction + "-v1\n" + path + "\n" + nonce + "\n" + value)
                .getBytes(StandardCharsets.US_ASCII));
        } catch (GeneralSecurityException failure) {
            throw new IllegalStateException("HmacSHA256 unavailable", failure);
        }
    }

    private static void respond(HttpExchange exchange, int code) throws IOException {
        exchange.getResponseHeaders().set("Cache-Control", "no-store");
        exchange.sendResponseHeaders(code, -1);
    }

    @Override public void close() {
        if (!closed.compareAndSet(false, true)) return;
        server.stop(0);
        worker.shutdownNow();
        expiry.shutdownNow();
    }
}
