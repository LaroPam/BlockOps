# BlockOps Client: Fabric 1.21.1 prototype

This is a client-only Fabric mod for Minecraft Java **1.21.1**, Java 21 and Fabric Loader
0.16.10 or newer. It requires Fabric API. No Minecraft binaries are included in this
repository or distributed inside the mod.

The mod registers the local client command `/blockops` and a pairing button in the title
screen. Either opens a short-lived local bridge and lets the user copy a one-use secret.
When the host has opened a single-player world to LAN, `/blockops` allows the desktop to
request its current port from Minecraft without parsing LAN broadcasts.
After the desktop has established a guest tunnel, the user can paste this secret into BlockOps
and request that Minecraft start connecting. This experimental path is not yet verified in a
real Minecraft client and must not be installed automatically.

## Build

Run `./gradlew build` (Windows: `gradlew.bat build`) from this directory with JDK 21.
The checked-in Gradle wrapper verifies the Gradle distribution SHA-256. The build fetches
Minecraft, mappings and Fabric dependencies from their official repositories. Its output is
`build/libs/blockops-fabric-client-0.1.0.jar`. GitHub Actions builds and archives this JAR.
Building it does not establish that it works in a real Minecraft client.

## Experimental world port

In an open single-player world, enter `/blockops`, copy the local code, then use
**Определить порт через мод** in BlockOps. This returns a snapshot only if the integrated
server reports the world open to LAN. It still requires a manual one-use pairing code and
does not monitor world closure. When the world closes, stop the host tunnel manually.
The mod and the selected Minecraft instance must run on the host's computer.

## Experimental local pairing and join

1. Install the prototype manually only in an isolated, compatible Fabric 1.21.1 test instance.
   Start Minecraft, leave any active world, and open **BlockOps** on the title screen.
2. Choose **Скопировать код** and paste the secret into BlockOps on the same computer, after
   a guest tunnel is established. Clipboard history or synchronization may retain the code.
   Never share it. A new code is needed after use or five minutes.
3. **Войти в мир** asks the mod to connect to the current loopback guest tunnel. The mod
   refuses while a world is open or Minecraft is outside its title, pairing or multiplayer screens. A
   confirmation means only that Minecraft started an attempt; the game reports its outcome.

The mod binds only `127.0.0.1`, requires a 256-bit one-use bearer secret, rejects browser
Origin headers and closes the listener after five minutes. The desktop derives the target
from the current guest tunnel rather than accepting a destination from the UI. The code is
not written to files, process arguments, game chat or application logs by this integration.
Same-user local processes remain within the trust boundary, and the clipboard is explicitly
user-controlled. Minecraft's Java runtime must provide the `jdk.httpserver` module.

`./gradlew build` runs the Java bridge checks, including a Rust-to-Java test that passes a
short-lived secret through a private test-process stdin pipe. Real Minecraft, mixin runtime,
launcher compatibility and gameplay have not yet been tested. Automatic pairing and world-closure monitoring,
mod installation, Forge, NeoForge and other Minecraft versions remain future work.
