import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const desktopDir = fileURLToPath(new URL('..', import.meta.url));
const vite = fileURLToPath(new URL('../../../node_modules/vite/bin/vite.js', import.meta.url));

test('server settings, detected LAN port and Minecraft profile discovery' , { timeout: 60_000 }, async () => {
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
      await page.getByText('Отдельные серверы BlockOps', { exact: true }).click();
      await page.getByRole('button', { name: '＋ Создать новый сервер' }).click();
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
    // The main host flow must use only the Minecraft port and bind the tunnel
    // target to this computer; creating a server or accepting an EULA is not involved.
    const lan = await browser.newPage();
    await lan.addInitScript(() => {
      window.__calls = [];
      window.__worldAlive = true;
      let session = null;
      window.__detections = [];
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'detect_open_world') {
          window.__detections.push(args);
          if (args.pairingCode === null && window.__detections.length === 1) throw 'Minecraft с модом BlockOps не найден на этом компьютере.';
          return { id: 7, port: 54321 };
        }
        if (command === 'world_watch_alive') return window.__worldAlive;
        if (command === 'create_code') { window.__codeRequests = (window.__codeRequests || 0) + 1; return null; }
        if (command === 'host') {
          window.__calls.push(args);
          session = { mode: 'host', endpoint: null, active_connections: 0, last_error: null, transport: null };
          return session;
        }
        if (command === 'status') return session;
        if (command === 'servers' || command === 'codes') return [];
        return null;
      }};
    });
    await lan.goto(baseUrl);
    const portInput = lan.getByLabel('Порт мира Minecraft');
    assert.equal(await portInput.inputValue(), '');
    // Automatic discovery first: a failure explains itself and leaves the manual path open.
    const find = lan.getByRole('button', { name: 'Найти открытый мир', exact: true });
    await find.click();
    await lan.getByText(/Minecraft с модом BlockOps не найден/).waitFor();
    assert.equal(await portInput.inputValue(), '');
    await lan.locator('.world-detection').getByText('Ввести код мода вручную', { exact: true }).click();
    await lan.getByLabel('Код мода на этом компьютере').fill('12345:' + 'a'.repeat(64));
    await lan.getByRole('button', { name: 'Определить порт', exact: true }).click();
    await lan.getByText(/Порт получен из Minecraft/).waitFor();
    assert.equal(await portInput.inputValue(), '54321');
    assert.equal(await lan.getByLabel('Код мода на этом компьютере').inputValue(), '');
    await find.click();
    await lan.waitForFunction(() => window.__detections.length === 3);
    await lan.getByText(/Порт получен из Minecraft/).waitFor();
    const detectionArgs = await lan.evaluate(() => window.__detections);
    assert.deepEqual(detectionArgs, [{ pairingCode: null }, { pairingCode: '12345:' + 'a'.repeat(64) }, { pairingCode: null }]);
    await lan.getByRole('button', { name: 'Пригласить друга' }).click();
    await lan.waitForFunction(() => window.__calls.length === 1);
    const [hostArgs] = await lan.evaluate(() => window.__calls);
    assert.equal(hostArgs.target, '127.0.0.1:54321');
    assert.equal(hostArgs.relay, null);
    assert.equal(hostArgs.worldWatchId, 7);
    await lan.getByRole('button', { name: 'Создать одноразовый код' }).waitFor();
    // One click prepares the connection and the first friend's code.
    await lan.waitForFunction(() => window.__codeRequests === 1);
    await lan.evaluate(() => { window.__worldAlive = false; });
    await lan.getByText(/Мир закрыт или связь с модом потеряна/).waitFor();
    assert.equal(await portInput.inputValue(), '');
    await lan.close();
    const joinPage = await browser.newPage();
    await joinPage.addInitScript(() => {
      window.__calls = [];
      const session = { mode: 'join', endpoint: '127.0.0.1:34567', active_connections: 0, last_error: null, transport: null };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'status') return session;
        if (command === 'join_in_minecraft') { window.__calls.push(args); return null; }
        if (command === 'servers') return [];
        return null;
      }};
    });
    await joinPage.goto(baseUrl);
    await joinPage.getByRole('button', { name: 'Войти в мир', exact: true }).click();
    await joinPage.getByText(/Minecraft начал попытку подключения/).waitFor();
    await joinPage.locator('.minecraft-join').getByText('Ввести код мода вручную', { exact: true }).click();
    const pairing = joinPage.getByLabel('Код сопряжения с Minecraft');
    await pairing.fill('12345:' + 'a'.repeat(64));
    await joinPage.getByRole('button', { name: 'Войти по коду', exact: true }).click();
    await joinPage.waitForFunction(() => window.__calls.length === 2);
    const joinArgs = await joinPage.evaluate(() => window.__calls);
    assert.deepEqual(joinArgs, [{ pairingCode: null }, { pairingCode: '12345:' + 'a'.repeat(64) }]);
    assert.equal(await pairing.inputValue(), '');
    await joinPage.close();
    // Automatic host flow: the world is noticed once opened, one click yields a copyable code.
    const auto = await browser.newPage();
    await auto.addInitScript(() => {
      window.__local = { running: 1, open_worlds: 0, in_menu: 0 };
      window.__calls = [];
      window.__copied = [];
      Object.defineProperty(navigator, 'clipboard', { value: { writeText: async text => { window.__copied.push(text); } } });
      let session = null;
      let codes = [];
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'local_minecraft') return window.__local;
        if (command === 'detect_open_world') { window.__calls.push([command, args]); return { id: 3, port: 45678 }; }
        if (command === 'world_watch_alive') return true;
        if (command === 'host') {
          window.__calls.push([command, args]);
          session = { mode: 'host', endpoint: null, active_connections: 0, last_error: null, transport: null };
          return session;
        }
        if (command === 'create_code') {
          window.__calls.push([command, args]);
          codes = [{ id: 'a', code: 'ABCD-EFGH-JKMN', expires_at: 2000000000, phase: 'waiting', peer: null, error: null }];
          return null;
        }
        if (command === 'codes') return codes;
        if (command === 'status') return session;
        if (command === 'servers') return [];
        return null;
      }};
    });
    await auto.goto(baseUrl);
    await auto.getByText(/Minecraft запущен. Откройте мир для сети/).waitFor();
    assert.equal(await auto.evaluate(() => window.__calls.length), 0, 'detection started before the world opened');
    await auto.evaluate(() => { window.__local = { running: 1, open_worlds: 1, in_menu: 0 }; });
    await auto.getByText(/Порт получен из Minecraft/).waitFor();
    assert.equal(await auto.getByLabel('Порт мира Minecraft').inputValue(), '45678');
    await auto.getByRole('button', { name: 'Пригласить друга' }).click();
    await auto.getByRole('button', { name: 'Копировать код' }).click();
    await auto.getByRole('button', { name: 'Скопировано' }).waitFor();
    const autoCalls = await auto.evaluate(() => window.__calls);
    assert.deepEqual(autoCalls.map(([command]) => command), ['detect_open_world', 'host', 'create_code']);
    assert.deepEqual(autoCalls[0][1], { pairingCode: null });
    assert.equal(autoCalls[1][1].target, '127.0.0.1:45678');
    assert.equal(autoCalls[1][1].worldWatchId, 3);
    assert.deepEqual(autoCalls[2][1], { coordinator: 'http://127.0.0.1:8080' });
    assert.deepEqual(await auto.evaluate(() => window.__copied), ['ABCD-EFGH-JKMN']);
    await auto.close();

    // Automatic guest flow: the game joins by itself once it reaches the main menu, once.
    const guest = await browser.newPage();
    await guest.addInitScript(() => {
      window.__local = { running: 1, open_worlds: 0, in_menu: 0 };
      window.__joins = [];
      const session = { mode: 'join', endpoint: '127.0.0.1:34567', active_connections: 0, last_error: null, transport: null };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'local_minecraft') return window.__local;
        if (command === 'status') return session;
        if (command === 'join_in_minecraft') { window.__joins.push(args); return null; }
        if (command === 'servers') return [];
        return null;
      }};
    });
    await guest.goto(baseUrl);
    await guest.getByText(/Откройте главное меню Minecraft: игра сама подключится/).waitFor();
    assert.equal(await guest.evaluate(() => window.__joins.length), 0, 'joined before Minecraft was ready');
    await guest.evaluate(() => { window.__local = { running: 1, open_worlds: 0, in_menu: 1 }; });
    await guest.getByText(/Minecraft начал попытку подключения/).waitFor();
    await delay(3500); // Several more polls must not repeat the request.
    assert.deepEqual(await guest.evaluate(() => window.__joins), [{ pairingCode: null }]);
    await guest.close();

    // Service addresses survive a restart, can be reset, and failures read as advice.
    const settings = await browser.newPage();
    await settings.addInitScript(() => {
      let session = null;
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'host') {
          session = { mode: 'host', endpoint: null, active_connections: 0, last_error: null, transport: null };
          return session;
        }
        if (command === 'create_code') throw `error sending request for url (${args.coordinator}/v1/invites)`;
        if (command === 'status') return session;
        if (command === 'servers' || command === 'codes') return [];
        return null;
      }};
    });
    await settings.goto(baseUrl);
    await settings.getByText('Дополнительные настройки подключения', { exact: true }).click();
    await settings.getByLabel('Адрес сервиса координации').fill('https://connect.example.org');
    await settings.getByLabel('Адрес relay для вашего сервера').fill('/ip4/192.0.2.1/tcp/4001/p2p/peer');
    await settings.reload();
    await settings.getByText('Дополнительные настройки подключения', { exact: true }).click();
    assert.equal(await settings.getByLabel('Адрес сервиса координации').inputValue(), 'https://connect.example.org');
    assert.equal(await settings.getByLabel('Адрес relay для вашего сервера').inputValue(), '/ip4/192.0.2.1/tcp/4001/p2p/peer');
    await settings.getByLabel('Порт мира Minecraft').fill('25565');
    await settings.getByRole('button', { name: 'Пригласить друга' }).click();
    await settings.getByText(/код для друга не создан: Не удалось связаться с сервисом координации/).waitFor();
    await settings.getByRole('button', { name: 'Отключить соединение' }).waitFor();
    await settings.evaluate(() => window.__TAURI_INTERNALS__.invoke = async command => command === 'servers' || command === 'codes' ? [] : null);
    await settings.getByRole('button', { name: 'Отключить соединение' }).click();
    await settings.getByRole('button', { name: 'Вернуть локальные настройки' }).click();
    assert.equal(await settings.getByLabel('Адрес сервиса координации').inputValue(), 'http://127.0.0.1:8080');
    assert.equal(await settings.getByLabel('Адрес relay для вашего сервера').inputValue(), '');
    await settings.close();

    const profiles = await browser.newPage();
    await profiles.addInitScript(() => {
      window.__calls = [];
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'discover_minecraft_profiles') {
          window.__calls.push(args);
          return [{ name: 'Моя сборка', game_dir: 'C:\\Games\\Minecraft', version_id: 'custom', minecraft_version: '1.21.1', loader: 'fabric', metadata_complete: true }];
        }
        if (command === 'save_minecraft_selection') { window.__calls.push(args); return args.selection; }
        if (command === 'status' || command === 'get_minecraft_selection') return null;
        if (command === 'servers') return [];
        return null;
      }};
    });
    await profiles.goto(baseUrl);
    await profiles.getByText('Найти мою сборку Minecraft', { exact: true }).click();
    await profiles.getByLabel('Папка Minecraft').fill('C:\\MyLauncher');
    await profiles.getByRole('button', { name: 'Найти сборки', exact: true }).click();
    await profiles.getByText('Моя сборка', { exact: true }).waitFor();
    assert.match(await profiles.locator('.profile-list').innerText(), /Загрузчик: Fabric/);
    const [scanArgs] = await profiles.evaluate(() => window.__calls);
    assert.equal(scanArgs.folder, 'C:\\MyLauncher');
    await profiles.getByRole('button', { name: 'Выбрать эту папку' }).click();
    assert.equal(await profiles.getByLabel('Папка игровой сборки').inputValue(), 'C:\\Games\\Minecraft');
    assert.equal(await profiles.getByRole('button', { name: 'Сохранить выбор' }).isDisabled(), true);
    await profiles.getByLabel('Какой загрузчик установлен?').selectOption('unknown');
    await profiles.getByRole('button', { name: 'Сохранить выбор' }).click();
    await profiles.getByText(/Совместимость ещё не проверена/).waitFor();
    const [, savedArgs] = await profiles.evaluate(() => window.__calls);
    assert.deepEqual(savedArgs.selection, { game_dir: 'C:\\Games\\Minecraft', loader: 'unknown' });
    await profiles.close();
  } finally {
    if (browser) await browser.close();
    server.kill();
  }
});
