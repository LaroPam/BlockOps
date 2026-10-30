package org.blockops.forge;

import java.io.IOException;
import net.minecraft.client.gui.GuiGraphics;
import net.minecraft.client.gui.components.Button;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.network.chat.Component;

/** Code is shown only via an explicit clipboard action; never sent to chat or logs. */
public final class PairingScreen extends Screen {
    private final Screen parent;
    private String code;
    private boolean copied;
    private boolean failed;

    public PairingScreen(Screen parent) { super(Component.literal("BlockOps")); this.parent = parent; }

    @Override protected void init() {
        if (code == null && !failed) {
            try { code = BlockOpsForgeClient.startPairing(); }
            catch (IOException | RuntimeException | LinkageError failure) { failed = true; }
        }
        if (code != null) {
            addRenderableWidget(Button.builder(Component.literal("Скопировать код"), button -> {
                if (minecraft != null) { minecraft.keyboardHandler.setClipboard(code); copied = true; }
            }).bounds(width / 2 - 100, height / 2 + 15, 200, 20).build());
        }
        addRenderableWidget(Button.builder(Component.literal("Назад"), button -> onClose())
            .bounds(width / 2 - 100, height / 2 + 42, 200, 20).build());
    }

    Screen parentScreen() { return parent; }

    @Override public boolean isPauseScreen() { return false; }

    @Override public void onClose() { if (minecraft != null) minecraft.setScreen(parent); }

    @Override public void render(GuiGraphics graphics, int mouseX, int mouseY, float delta) {
        // Before 1.20.2 a screen draws its own background.
        renderBackground(graphics);
        super.render(graphics, mouseX, mouseY, delta);
        graphics.drawCenteredString(font, title, width / 2, height / 2 - 72, 0xFFFFFF);
        graphics.drawCenteredString(font, Component.literal("BlockOps на этом компьютере обычно находит Minecraft сам."), width / 2, height / 2 - 52, 0xC0C0C0);
        graphics.drawCenteredString(font, Component.literal(failed ? "Не удалось открыть локальное сопряжение." : "Если не нашёл, вставьте этот код в BlockOps."), width / 2, height / 2 - 34, 0xFFFFFF);
        graphics.drawCenteredString(font, Component.literal(copied ? "Код скопирован. Не передавайте его другим." : "Код действует 5 минут и используется один раз."), width / 2, height / 2 - 17, 0xC0C0C0);
    }
}
