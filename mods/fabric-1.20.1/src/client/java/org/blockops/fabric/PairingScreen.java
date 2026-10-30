package org.blockops.fabric;

import java.io.IOException;
import net.minecraft.client.gui.DrawContext;
import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.widget.ButtonWidget;
import net.minecraft.text.Text;

/** Code is shown only via an explicit clipboard action; never sent to chat or logs. */
public final class PairingScreen extends Screen {
    private final Screen parent;
    private String code;
    private boolean copied;
    private boolean failed;

    public PairingScreen(Screen parent) { super(Text.literal("BlockOps")); this.parent = parent; }

    @Override protected void init() {
        if (code == null && !failed) {
            try { code = BlockOpsClient.startPairing(); }
            catch (IOException | RuntimeException | LinkageError failure) { failed = true; }
        }
        if (code != null) {
            addDrawableChild(ButtonWidget.builder(Text.literal("Скопировать код"), button -> {
                if (client != null) { client.keyboard.setClipboard(code); copied = true; }
            }).dimensions(width / 2 - 100, height / 2 + 15, 200, 20).build());
        }
        addDrawableChild(ButtonWidget.builder(Text.literal("Назад"), button -> close())
            .dimensions(width / 2 - 100, height / 2 + 42, 200, 20).build());
    }

    Screen parentScreen() { return parent; }

    @Override public boolean shouldPause() { return false; }

    @Override public void close() { if (client != null) client.setScreen(parent); }

    @Override public void render(DrawContext context, int mouseX, int mouseY, float delta) {
        // Before 1.20.2 a screen draws its own background.
        renderBackground(context);
        super.render(context, mouseX, mouseY, delta);
        context.drawCenteredTextWithShadow(textRenderer, title, width / 2, height / 2 - 72, 0xFFFFFF);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal("BlockOps на этом компьютере обычно находит Minecraft сам."), width / 2, height / 2 - 52, 0xC0C0C0);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal(failed ? "Не удалось открыть локальное сопряжение." : "Если не нашёл, вставьте этот код в BlockOps."), width / 2, height / 2 - 34, 0xFFFFFF);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal(copied ? "Код скопирован. Не передавайте его другим." : "Код действует 5 минут и используется один раз."), width / 2, height / 2 - 17, 0xC0C0C0);
    }
}
