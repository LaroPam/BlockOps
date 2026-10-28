package org.blockops.bridge;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.HexFormat;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.IntPredicate;
import java.util.function.LongSupplier;

/** An explicitly started, short-lived one-use local HTTP capability. */
public final class LocalJoinBridge implements AutoCloseable {
    private final HttpServer server;
    private final ThreadPoolExecutor worker;
    private final ScheduledExecutorService expiry;
    private final AtomicBoolean closed = new AtomicBoolean();
    private final byte[] token;
    private final long startedNanos;
    private final long lifetimeNanos;
    private final LongSupplier ticker;
    private final AtomicBoolean used = new AtomicBoolean();
    private final IntPredicate onJoin;

    public LocalJoinBridge(IntPredicate onJoin, Duration lifetime) throws IOException {
        this(onJoin, lifetime, System::nanoTime);
    }

    LocalJoinBridge(IntPredicate onJoin, Duration lifetime, LongSupplier ticker) throws IOException {
        if (onJoin == null || ticker == null || lifetime == null || lifetime.isNegative() || lifetime.isZero()
                || lifetime.compareTo(Duration.ofMinutes(5)) > 0) {
            throw new IllegalArgumentException("Invalid bridge lifetime or callback");
        }
        this.onJoin = onJoin;
        this.ticker = ticker;
        this.startedNanos = ticker.getAsLong();
        this.lifetimeNanos = lifetime.toNanos();
        byte[] random = new byte[32];
        new SecureRandom().nextBytes(random);
        this.token = HexFormat.of().formatHex(random).getBytes(StandardCharsets.US_ASCII);
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
        server.start();
        expiry.schedule(this::close, lifetime.toNanos(), TimeUnit.NANOSECONDS);
    }

    public int port() { return server.getAddress().getPort(); }

    /** Secret: the caller must deliver this through a trusted local pairing mechanism. */
    public String token() { return new String(token, StandardCharsets.US_ASCII); }

    private void handle(HttpExchange exchange) throws IOException {
        try {
            if (!"POST".equals(exchange.getRequestMethod())
                    || !"/v1/join".equals(exchange.getRequestURI().getRawPath())
                    || exchange.getRequestURI().getRawQuery() != null
                    || exchange.getRequestHeaders().containsKey("Origin")
                    || exchange.getRequestHeaders().containsKey("Transfer-Encoding")
                    || !"0".equals(exchange.getRequestHeaders().getFirst("Content-Length"))) {
                respond(exchange, 400);
                return;
            }
            var auth = exchange.getRequestHeaders().get("Authorization");
            String supplied = auth != null && auth.size() == 1 && auth.get(0).startsWith("Bearer ")
                ? auth.get(0).substring(7) : "";
            byte[] candidate = supplied.getBytes(StandardCharsets.US_ASCII);
            if (candidate.length != token.length || !MessageDigest.isEqual(token, candidate)
                    || ticker.getAsLong() - startedNanos >= lifetimeNanos || used.get()) {
                respond(exchange, 403);
                return;
            }
            var values = exchange.getRequestHeaders().get("X-BlockOps-Port");
            String value = values != null && values.size() == 1 ? values.get(0) : "";
            if (!value.matches("[0-9]{1,5}")) { respond(exchange, 400); return; }
            int port = Integer.parseInt(value);
            if (port == 0 || port > 65535) { respond(exchange, 400); return; }
            if (!used.compareAndSet(false, true)) { respond(exchange, 403); return; }
            // Acceptance only means the caller started the attempt, not that Minecraft joined.
            respond(exchange, onJoin.test(port) ? 202 : 409);
        } finally {
            exchange.close();
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
