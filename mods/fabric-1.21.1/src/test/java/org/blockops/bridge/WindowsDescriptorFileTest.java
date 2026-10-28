package org.blockops.bridge;

import static org.blockops.bridge.LocalJoinBridgeTest.check;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.concurrent.TimeUnit;

/** Windows only: the mod's writer against the desktop's owner-only reader and discovery. */
public final class WindowsDescriptorFileTest {
    private static byte[] descriptor(long pid, int port, String token) {
        return ("{\"version\":1,\"pid\":" + pid + ",\"port\":" + port + ",\"token\":\"" + token
            + "\",\"expires_at\":" + (System.currentTimeMillis() / 1000 + 120) + "}").getBytes(StandardCharsets.US_ASCII);
    }

    /** Runs a Rust test program; a secret goes only through stdin and is redacted from failures. */
    private static String run(String secret, String... command) throws Exception {
        Process process = new ProcessBuilder(command).redirectErrorStream(true).start();
        try {
            try (var stdin = process.getOutputStream()) {
                stdin.write((secret + "\n").getBytes(StandardCharsets.US_ASCII));
            }
            check(process.waitFor(15, TimeUnit.SECONDS), "Rust test program timed out");
            String output = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8).trim();
            if (!secret.isEmpty()) output = output.replace(secret, "[redacted]");
            return process.exitValue() == 0 ? output : "FAILED: " + output;
        } finally {
            if (process.isAlive()) process.destroyForcibly();
        }
    }

    public static void main(String[] args) throws Exception {
        if (!System.getProperty("os.name", "").startsWith("Windows")) {
            System.out.println("Windows descriptor tests skipped: not Windows.");
            return;
        }
        String reader = System.getenv("BLOCKOPS_DISCOVERY_TEST_READER");
        String client = System.getenv("BLOCKOPS_BRIDGE_TEST_CLIENT");
        check(reader != null && !reader.isBlank() && client != null && !client.isBlank(),
            "BLOCKOPS_DISCOVERY_TEST_READER and BLOCKOPS_BRIDGE_TEST_CLIENT are required on Windows");
        Path directory = Files.createTempDirectory("blockops-discovery");
        try {
            long pid = ProcessHandle.current().pid();
            var store = new WindowsDescriptorFile(directory, pid);
            Path target = directory.resolve("minecraft-" + pid + ".json");
            String first = LocalJoinBridgeTest.nonce();
            store.publish(descriptor(pid, 41001, first));
            String result = run(first, reader, target.toString());
            check(result.equals(pid + " 41001"), "private descriptor rejected: " + result);
            String second = LocalJoinBridgeTest.nonce();
            store.publish(descriptor(pid, 41002, second));
            result = run(second, reader, target.toString());
            check(result.equals(pid + " 41002"), "replaced descriptor rejected: " + result);
            check(!Files.exists(directory.resolve("minecraft-" + pid + ".tmp")), "staging file left behind");

            // Ordinary inherited permissions are not private enough to carry a capability.
            Path plain = directory.resolve("minecraft-1.json");
            Files.write(plain, descriptor(1, 41003, first));
            check(run(first, reader, plain.toString()).startsWith("FAILED"), "inherited permissions accepted");
            Files.delete(plain);
            store.remove();
            check(!Files.exists(target), "descriptor not removed");

            // The whole path: publisher -> private file -> Rust discovery -> proven world port.
            try (var publisher = new DiscoveryPublisher(
                    () -> new LocalJoinBridge(port -> false, () -> 43210, Duration.ofMinutes(5)),
                    new WindowsDescriptorFile(directory, pid))) {
                publisher.refresh();
                result = run("", client, "discover", directory.toString(), "43210");
                check(result.isEmpty(), "Rust discovery failed: " + result);
            }
            check(!Files.exists(target), "publisher left its descriptor behind");
        } finally {
            try (var files = Files.list(directory)) { for (var file : files.toList()) Files.deleteIfExists(file); }
            Files.deleteIfExists(directory);
        }
        System.out.println("Windows descriptor tests passed.");
    }
}
