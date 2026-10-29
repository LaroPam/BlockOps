# Bundled BlockOps mod

The installer ships the Fabric mod from this folder; the application installs it into a
Minecraft profile only after the player confirms. CI builds `mods/fabric-1.21.1` and copies
`blockops-fabric-client-*.jar` here before `tauri build`. The jar is a build output and is not
committed. Without it, the application reports that this build carries no mod.

For a local build with the mod, run `./gradlew build` in `mods/fabric-1.21.1` and copy
`build/libs/blockops-fabric-client-0.1.0.jar` into this folder.
