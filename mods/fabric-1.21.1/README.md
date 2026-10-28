# BlockOps Client: Fabric 1.21.1 prototype

This is a client-only Fabric mod for Minecraft Java **1.21.1**, Java 21 and Fabric Loader
0.16.10 or newer. It requires Fabric API. No Minecraft binaries are included in this
repository or distributed inside the mod.

The mod registers the local client command `/blockops` and reports that the mod is loaded.
It does not connect to BlockOps or join a world yet. The command can only be used when the
Minecraft client is connected to a world. There is no background service or network listener.
Do not install this prototype automatically; it does not implement the one-click flow.

## Build

Run `./gradlew build` (Windows: `gradlew.bat build`) from this directory with JDK 21.
The checked-in Gradle wrapper verifies the Gradle distribution SHA-256. The build fetches
Minecraft, mappings and Fabric dependencies from their official repositories. Its output is
`build/libs/blockops-fabric-client-0.1.0.jar`. GitHub Actions builds and archives this JAR.
Building it does not establish that it works in a real Minecraft client.

The repository also contains an experimental, separately tested one-use loopback bridge and a Rust client. The mod does not start the bridge and the desktop does not pair with it yet. A safe pairing mechanism and a join command that runs on the Minecraft client thread are the next steps. Its security,
launcher compatibility and behaviour on existing worlds must be tested before installation
is offered to a user. Forge, NeoForge and other Minecraft versions need separate adapters
and compatibility checks.

## Experimental local bridge

`LocalJoinBridge` is not started by the mod. It binds only `127.0.0.1`, accepts a single
HTTP POST authenticated with a random 256-bit bearer capability, expires within five
minutes using monotonic time, rejects browser Origin headers and validates a numeric
Minecraft endpoint port. The Rust client refuses non-loopback Minecraft addresses and
does not use HTTP proxies or redirects. An accepted request means only that the callback
received a port; no Minecraft connection is reported.

The secret has no delivery mechanism yet and must never be put in logs or command-line
arguments. Before integration, design authenticated pairing, restrict the requested port
to an active BlockOps guest tunnel, and test the actual Minecraft client thread and
shutdown. Same-user local processes are within the local trust boundary; the bridge alone
does not identify a Windows user. Minecraft's chosen Java runtime must also provide the
`jdk.httpserver` module.

`./gradlew build` runs the Java bridge checks. In CI, an additional Rust test client is
compiled and sends the secret to the Java bridge through a private test-process stdin pipe,
verifying the Rust-to-Java request. This pipe is solely a test harness, not a production
pairing mechanism.
