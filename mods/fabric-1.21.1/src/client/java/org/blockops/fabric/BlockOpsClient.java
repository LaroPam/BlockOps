package org.blockops.fabric;

import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandRegistrationCallback;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandManager;
import net.minecraft.text.Text;

/** Client-only entry point; connection to the desktop app is a future step. */
public final class BlockOpsClient implements ClientModInitializer {
    @Override
    public void onInitializeClient() {
        ClientCommandRegistrationCallback.EVENT.register((dispatcher, registryAccess) ->
            dispatcher.register(ClientCommandManager.literal("blockops")
                .executes(context -> {
                    context.getSource().sendFeedback(Text.literal(
                        "BlockOps: клиентский мод загружен. Подключение к приложению пока не реализовано."));
                    return 1;
                })));
    }
}
