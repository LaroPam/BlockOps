import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const desktopDir = fileURLToPath(new URL('..', import.meta.url));
const vite = fileURLToPath(new URL('../../../node_modules/vite/bin/vite.js', import.meta.url));

test('creation sends the selected game mode and PvP settings to Tauri', { timeout: 60_000 }, async () => {
  const probe = createServer();
  await new Promise((resolve, reject) => { probe.once('error', reject); probe.listen(0, '127.0.0.1', resolve); });
  const port = probe.address().port;
  await new Promise((resolve, reject) => probe.close(error => error ? reject(error) : resolve()));
  const baseUrl = `http://127.0.0.1:${port}`;
  const server = spawn(process.execPath, [vite, '--host', '127.0.0.1', '--port', String(port), '--strictPort'], { cwd: desktopDir, stdio: 'ignore' });
  let browser;
  try {
    for (let attempt = 0; ; attempt++) {
      if (server.exitCode !== null) throw new Error('Vite exited before startup');
      try { if ((await fetch(baseUrl)).ok) break; } catch { /* Wait for Vite. */ }
      if (attempt >= 100) throw new Error('Vite did not start');
      await delay(100);
    }
    browser = await chromium.launch({ executablePath: process.env.BLOCKOPS_CHROMIUM_PATH || undefined, args: ['--no-sandbox'] });
    for (const choice of [{ mode: 'survival', pvp: true }, { mode: 'creative', pvp: false }]) {
      const page = await browser.newPage();
      await page.addInitScript(() => {
        window.__calls = [];
        window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
          if (command === 'create_server') window.__calls.push(args);
          if (command === 'servers') return [];
          return null;
        }};
      });
      await page.goto(baseUrl);
      await page.getByRole('button', { name: /Создать сервер/ }).first().click();
      const form = page.locator('form.create-card');
      const submit = form.getByRole('button', { name: 'Создать сервер', exact: true });
      assert.equal(await submit.isDisabled(), true);
      await form.locator('summary').click();
      await form.locator('#game-mode').selectOption(choice.mode);
      if (!choice.pvp) await form.getByRole('checkbox', { name: /PvP/ }).uncheck();
      await form.getByRole('checkbox', { name: /принимаю Minecraft EULA/ }).check();
      await submit.click();
      await page.waitForFunction(() => window.__calls.length === 1);
      const [request] = await page.evaluate(() => window.__calls);
      assert.equal(request.settings.game_mode, choice.mode);
      assert.equal(request.settings.pvp, choice.pvp);
      assert.equal(request.acceptEula, true);
      assert.equal(request.version, '1.21.1');
      await page.close();
    }
  } finally {
    if (browser) await browser.close();
    server.kill();
  }
});
