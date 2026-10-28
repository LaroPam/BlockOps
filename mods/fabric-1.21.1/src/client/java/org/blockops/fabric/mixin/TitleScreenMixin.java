package org.blockops.fabric.mixin;

import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.screen.TitleScreen;
import net.minecraft.client.gui.widget.ButtonWidget;
import net.minecraft.text.Text;
import org.blockops.fabric.PairingScreen;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(TitleScreen.class)
public abstract class TitleScreenMixin extends Screen {
    protected TitleScreenMixin(Text title) { super(title); }

    @Inject(method = "init", at = @At("TAIL"))
    private void blockops$addPairingButton(CallbackInfo callback) {
        addDrawableChild(ButtonWidget.builder(Text.literal("BlockOps"), button -> {
            if (client != null) client.setScreen(new PairingScreen((Screen)(Object)this));
        }).dimensions(width - 92, 8, 84, 20).build());
    }
}
