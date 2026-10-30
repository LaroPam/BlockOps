# Bundled BlockOps mods

The installer ships the mod jars from this folder, one per loader and Minecraft version; the
application installs the matching one into a Minecraft profile only after the player confirms.
CI builds every folder under `mods` and places the jars here before `tauri build`. The jars are
build outputs and are not committed. Without them, the application reports that this build
carries no mod.

For a local build with the mods, run `./gradlew -p <folder> build` in `mods` for each folder
and copy `<folder>/build/libs/blockops-<folder>-0.1.0.jar` into this folder.
