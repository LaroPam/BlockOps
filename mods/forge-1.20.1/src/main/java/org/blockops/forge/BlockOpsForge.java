package org.blockops.forge;

import net.minecraftforge.api.distmarker.Dist;
import net.minecraftforge.fml.common.Mod;
import net.minecraftforge.fml.loading.FMLEnvironment;

/** Entry point; the companion only runs in the game client. */
@Mod("blockops")
public final class BlockOpsForge {
    public BlockOpsForge() {
        if (FMLEnvironment.dist == Dist.CLIENT) BlockOpsForgeClient.init();
    }
}
