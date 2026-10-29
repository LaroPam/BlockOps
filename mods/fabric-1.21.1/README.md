# BlockOps Client: Fabric 1.21.1 prototype

This is a client-only Fabric mod for Minecraft Java **1.21.1**, Java 21 and Fabric Loader
0.16.10 or newer. It needs nothing else: the three Fabric API modules it uses
(`fabric-api-base`, `fabric-command-api-v2`, `fabric-lifecycle-events-v1`, Apache-2.0, each with
its license) are nested in the jar, and a separately installed, newer Fabric API takes
precedence. No Minecraft binaries are included in this repository or distributed inside the mod.

On Windows the mod advertises a one-use local bridge through a private descriptor, so the
BlockOps desktop on the same computer finds it without a copied code (see below). The mod also
registers the local client command `/blockops` and a pairing button in the title screen. Either
opens a short-lived local bridge and lets the user copy a one-use secret as a fallback.
When the host has opened a single-player world to LAN, `/blockops` allows the desktop to
request its current port from Minecraft without parsing LAN broadcasts. The desktop can
keep a one-use authenticated watch open to stop its associated tunnel after world closure.
After the desktop has established a guest tunnel, the user can paste this secret into BlockOps
and request that Minecraft start connecting. This experimental path is not yet verified in a
real Minecraft client.

## Installation by the desktop

The Windows installer of BlockOps carries this jar. In **Найти мою сборку Minecraft** the
desktop offers to install, update or remove it for an official Minecraft Launcher profile, and
writes only after the player confirms. It re-reads the launcher profiles itself and requires
that a complete Fabric 1.21.1 profile runs from the chosen game folder and that every other
profile running from that folder is Fabric 1.21.1 or Vanilla: the mod's `minecraft = 1.21.1`
dependency would stop a Fabric profile of another version from starting. It recognises its own
jars by the `blockops` id in `fabric.mod.json`, writes under a hidden temporary name, removes
older copies and renames the new file into `mods`; other files are never touched. A file held
open by a running game is reported instead of half-replaced. Other launchers are not supported
yet; their users can copy the jar into `mods` manually.

## Build

Run `./gradlew build` (Windows: `gradlew.bat build`) from this directory with JDK 21.
The checked-in Gradle wrapper verifies the Gradle distribution SHA-256. The build fetches
Minecraft, mappings and Fabric dependencies from their official repositories. Its output is
`build/libs/blockops-fabric-client-0.1.0.jar`. GitHub Actions builds and archives this JAR.
Building it does not establish that it works in a real Minecraft client.

## Automatic discovery (Windows)

While the game runs, the mod keeps one unused bridge advertised in
`%LOCALAPPDATA%\BlockOps\discovery\minecraft-<pid>.json`. The file is created with an
explicit owner and a protected DACL that allows only the current Windows user, before any
secret is written, then renamed into place. It holds the bridge port, a 256-bit one-use
secret valid for under five minutes and the game state (`lan`, `menu` or `busy`), observed
on the client thread. The state is only a hint for when the desktop should ask: the bridge
checks the real state on every request, and a state change rewrites the descriptor without
spending the bridge. A used or nearly expired bridge is replaced within a
second; the file is removed when the game stops, and files older than ten minutes are removed
at the next start. The writer calls WinAPI through the JNA library that Minecraft already
ships, so nothing extra is bundled.

The desktop reads only owner-only files, skips expired ones and those of exited processes, and
then authenticates as described below. Other Windows users cannot read the secret; software
running as the same user or as an administrator is inside the trust boundary. Automatic
discovery is not available on other operating systems, where the manual code remains.

## Experimental world port

In an open single-player world, use **Найти открытый мир** in BlockOps. As a fallback, enter
`/blockops`, copy the local code and paste it under **Ввести код мода вручную**. This returns a snapshot only if the integrated
server reports the world open to LAN. It still requires a manual one-use pairing code and
monitors the world while its authenticated stream remains open. A closed world, changed port
or lost stream causes the associated tunnel to stop.
The mod and the selected Minecraft instance must run on the host's computer. Detection is
not instantaneous: stop sharing immediately after leaving the world, since a different
local process could reuse the port before the monitor notices.

## Experimental local pairing and join

1. Install the prototype manually only in an isolated, compatible Fabric 1.21.1 test instance.
   Start Minecraft, leave any active world, and open **BlockOps** on the title screen.
2. After a guest tunnel is established, **Войти в мир** in BlockOps finds the mod automatically
   on Windows. As a fallback, choose **Скопировать код** in the mod and paste the secret under
   **Ввести код мода вручную**. Clipboard history or synchronization may retain the code.
   Never share it. A new code is needed after use or five minutes.
3. The desktop asks the mod to connect to the current loopback guest tunnel. The mod
   refuses while a world is open or Minecraft is outside its title, pairing or multiplayer screens. A
   confirmation means only that Minecraft started an attempt; the game reports its outcome.

The mod binds only `127.0.0.1`, rejects browser Origin headers and closes the listener after
five minutes. The secret never crosses the socket: each request carries a fresh 256-bit nonce
and `Authorization: BlockOps-HMAC <hex>`, an HMAC-SHA256 keyed by the secret over the protocol
version, path, nonce and requested port; a request is accepted once. A successful response
carries `X-BlockOps-Proof`, an HMAC over the path, nonce and returned port, which the desktop
verifies before acting on it. A process that later reuses the port therefore neither learns
the secret nor passes as the mod. The desktop derives the target from the current guest tunnel
rather than accepting a destination from the UI. The manual code is not written to files,
process arguments, game chat or application logs; the automatic descriptor is written only to
the owner-only file above. The clipboard is explicitly user-controlled. Minecraft's Java
runtime must provide the `jdk.httpserver` module.

`./gradlew build` runs the Java bridge and publisher checks, including Rust-to-Java tests that
pass a short-lived secret through a private test-process stdin pipe. On Windows it also writes
real descriptors and requires the Rust reader to accept them, to reject a file with inherited
permissions and to discover the bridge end to end; set `BLOCKOPS_BRIDGE_TEST_CLIENT` and
`BLOCKOPS_DISCOVERY_TEST_READER` to the built `local_bridge_smoke` and `read_descriptor`
examples. Real Minecraft, mixin runtime, launcher compatibility (including file-system
virtualization by packaged launchers) and gameplay have not yet been tested. Mod installation,
Forge, NeoForge and other Minecraft versions remain future work.
