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

The next step is an authenticated local connection between the desktop application and the
mod, followed by a join command that runs on the Minecraft client thread. Its security,
launcher compatibility and behaviour on existing worlds must be tested before installation
is offered to a user. Forge, NeoForge and other Minecraft versions need separate adapters
and compatibility checks.
