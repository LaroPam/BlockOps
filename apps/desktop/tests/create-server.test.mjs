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
    for (const choice of [{ mode: 'survival', pvp: true }, { mode: 'creative', pvp: false, version: '1.12.2', java: 8 }]) {
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
      if (choice.version) {
        await form.locator('#server-version').selectOption(choice.version);
        await form.getByText(`Java ${choice.java} BlockOps скачает`).waitFor();
      }
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
      assert.equal(request.version, choice.version ?? '1.21.1');
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
    assert.equal(hostArgs.direct, true, 'without a configured service the host meets friends directly');
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
          return codes[0];
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
    // The first invitation is copied as a ready message; the button copies it again.
    await auto.getByText('Приглашение скопировано: вставьте его в чат с другом.').waitFor();
    await auto.getByRole('button', { name: 'Копировать приглашение' }).click();
    await auto.getByRole('button', { name: 'Скопировано' }).waitFor();
    const autoCalls = await auto.evaluate(() => window.__calls);
    assert.deepEqual(autoCalls.map(([command]) => command), ['detect_open_world', 'host', 'create_code']);
    assert.deepEqual(autoCalls[0][1], { pairingCode: null });
    assert.equal(autoCalls[1][1].target, '127.0.0.1:45678');
    assert.equal(autoCalls[1][1].worldWatchId, 3);
    assert.deepEqual(autoCalls[2][1], { coordinator: 'http://127.0.0.1:8080' });
    const copied = await auto.evaluate(() => window.__copied);
    assert.equal(copied.length, 2);
    assert.equal(copied[0], copied[1]);
    assert.match(copied[0], /^Заходи в мой мир Minecraft через BlockOps! Код приглашения: ABCD-EFGH-JKMN\. Он одноразовый и действует до /);
    await auto.close();

    // A whole pasted invitation connects at once; text without exactly one code pastes as usual.
    const invited = await browser.newPage();
    await invited.addInitScript(() => {
      window.__joins = [];
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'join_code') { window.__joins.push(args); throw '404 Not Found'; }
        if (command === 'servers') return [];
        return null;
      }};
    });
    await invited.goto(baseUrl);
    await invited.getByRole('button', { name: /Присоединиться/ }).click();
    // A serverless invitation carries a four-group code.
    await invited.getByLabel('Приглашение или код').evaluate(input => {
      const data = new DataTransfer();
      data.setData('text/plain', 'Заходи! Код приглашения: ABCD-EFGH-JKMN-PQRS. Он одноразовый.');
      input.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await invited.waitForFunction(() => window.__joins.length === 1);
    assert.equal((await invited.evaluate(() => window.__joins))[0].code, 'ABCD-EFGH-JKMN-PQRS');
    await invited.evaluate(() => { window.__joins = []; });
    await invited.reload();
    await invited.getByRole('button', { name: /Присоединиться/ }).click();
    const field = invited.getByLabel('Приглашение или код');
    const paste = text => field.evaluate((input, value) => {
      const data = new DataTransfer();
      data.setData('text/plain', value);
      input.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    }, text);
    await paste('Два кода: ABCD-EFGH-JKMN и ZZZZ-ZZZZ-ZZZZ');
    await paste('Без кода');
    assert.deepEqual(await invited.evaluate(() => window.__joins), []);
    await paste('Заходи в мой мир Minecraft через BlockOps! Код приглашения: abcd-efgh-jkmn. Он одноразовый и действует до 15:30.');
    await invited.getByText(/Код не найден, уже использован/).waitFor();
    assert.equal(await field.inputValue(), 'ABCD-EFGH-JKMN');
    assert.deepEqual(await invited.evaluate(() => window.__joins), [{ code: 'ABCD-EFGH-JKMN', coordinator: 'http://127.0.0.1:8080' }]);
    await invited.close();

    // Without the mod, in any version: the world's LAN announcement is enough.
    const vanilla = await browser.newPage();
    await vanilla.addInitScript(() => {
      window.__local = { running: 0, open_worlds: 0, in_menu: 0, lan_worlds: [] };
      window.__detections = [];
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'local_minecraft') return window.__local;
        if (command === 'detect_open_world') { window.__detections.push(args); return { id: 5, port: 51234, name: 'Steve - New World', version: '1.20.1' }; }
        if (command === 'world_watch_alive') return true;
        if (command === 'servers') return [];
        return null;
      }};
    });
    await vanilla.goto(baseUrl);
    await vanilla.getByText(/нажмите «Открыть для сети»: BlockOps найдёт его сам, в любой версии/).waitFor();
    await vanilla.evaluate(() => {
      window.__local = { running: 0, open_worlds: 1, in_menu: 0, lan_worlds: [{ port: 51234, name: 'Steve - New World', version: '1.20.1' }] };
    });
    await vanilla.getByText('Порт получен из Minecraft: «Steve - New World», Minecraft 1.20.1. BlockOps отслеживает, открыт ли мир.').waitFor();
    assert.equal(await vanilla.getByLabel('Порт мира Minecraft').inputValue(), '51234');
    assert.deepEqual(await vanilla.evaluate(() => window.__detections), [{ pairingCode: null }]);
    await vanilla.close();

    // Automatic guest flow: the game joins by itself once it reaches the main menu, once.
    const guest = await browser.newPage();
    await guest.addInitScript(() => {
      window.__local = { running: 1, open_worlds: 0, in_menu: 0 };
      window.__joins = [];
      window.__session = { mode: 'join', endpoint: '127.0.0.1:34567', active_connections: 0, last_error: null, transport: null };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'local_minecraft') return window.__local;
        if (command === 'status') return window.__session;
        if (command === 'join_in_minecraft') { window.__joins.push(args); return null; }
        if (command === 'add_server_entry') return { address: '127.0.0.1:25595', added: 2, present: 0, failed: 1 };
        if (command === 'servers') return [];
        return null;
      }};
    });
    await guest.goto(baseUrl);
    // Any version: the fixed address is saved as a server in Minecraft with one click.
    await guest.getByText('127.0.0.1:34567').waitFor();
    await guest.getByText(/Постоянный адрес 127.0.0.1:25595 занят другой программой/).waitFor();
    await guest.getByRole('button', { name: 'Добавить сервер в Minecraft' }).click();
    await guest.getByText(/появился сервер «BlockOps — мир друга»/).waitFor();
    await guest.getByText(/В 1 папке список серверов прочитать не удалось/).waitFor();
    await guest.getByText(/Откройте главное меню Minecraft: игра сама подключится/).waitFor();
    assert.equal(await guest.evaluate(() => window.__joins.length), 0, 'joined before Minecraft was ready');
    await guest.evaluate(() => { window.__local = { running: 1, open_worlds: 0, in_menu: 1 }; });
    // Minecraft waits in the menu until the route to the host is up.
    await guest.getByText('Устанавливаем соединение с другом…').waitFor();
    await delay(2000);
    assert.equal(await guest.evaluate(() => window.__joins.length), 0, 'joined before the route was ready');
    await guest.evaluate(() => { window.__session = { ...window.__session, transport: 'direct' }; });
    await guest.getByText('Соединение с другом установлено напрямую.').waitFor();
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
    assert.equal(await settings.getByLabel('Без своего сервера').isChecked(), true);
    assert.equal(await settings.getByLabel('Адрес сервиса координации').count(), 0, 'service fields shown in serverless mode');
    await settings.getByLabel('Через свой сервис координации').check();
    await settings.getByLabel('Адрес сервиса координации').fill('https://connect.example.org');
    await settings.getByLabel('Адрес relay для вашего сервера').fill('/ip4/192.0.2.1/tcp/4001/p2p/peer');
    await settings.reload();
    await settings.getByText('Дополнительные настройки подключения', { exact: true }).click();
    assert.equal(await settings.getByLabel('Через свой сервис координации').isChecked(), true);
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
      window.__modCalls = [];
      window.__mod = { bundled: '0.1.0', installed: ['0.0.9'], current: false };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'discover_minecraft_profiles') {
          // The readiness panel scans the standard folder by itself; record the manual search.
          if (args.folder !== null) window.__calls.push(args);
          return [
            { name: 'Моя сборка', game_dir: 'C:\\Games\\Minecraft', version_id: 'custom', minecraft_version: '1.21.1', loader: 'fabric', metadata_complete: true },
            { name: 'Старая сборка', game_dir: 'C:\\Games\\Old', version_id: 'old', minecraft_version: '1.20.4', loader: 'fabric', metadata_complete: true },
          ];
        }
        if (command === 'blockops_mod_targets') return [{ loader: 'fabric', minecraft: '1.21.1' }, { loader: 'fabric', minecraft: '1.20.1' }, { loader: 'forge', minecraft: '1.20.1' }, { loader: 'neoforge', minecraft: '1.21.1' }];
        if (command === 'blockops_mod_status') { window.__statusArgs = args; return window.__mod; }
        if (command === 'install_blockops_mod' || command === 'remove_blockops_mod') {
          window.__modCalls.push([command, args]);
          window.__mod = command === 'install_blockops_mod'
            ? { bundled: '0.1.0', installed: ['0.1.0'], current: true }
            : { bundled: '0.1.0', installed: [], current: false };
          return window.__mod;
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
    // The mod is offered only for Fabric 1.21.1, and installed only after confirmation.
    await profiles.getByText(/Мод BlockOps есть для: Fabric 1.21.1, Fabric 1.20.1, Forge 1.20.1, NeoForge 1.21.1/).waitFor();
    await profiles.getByText(/Установлена другая версия мода BlockOps: 0.0.9/).waitFor();
    await profiles.getByRole('button', { name: 'Обновить мод BlockOps' }).click();
    await profiles.getByText(/добавит мод BlockOps 0.1.0 вместо старой версии в папку mods/).waitFor();
    await profiles.getByRole('button', { name: 'Отмена' }).click();
    assert.equal(await profiles.evaluate(() => window.__modCalls.length), 0, 'installed without confirmation');
    await profiles.getByRole('button', { name: 'Обновить мод BlockOps' }).click();
    await profiles.getByRole('button', { name: 'Установить', exact: true }).click();
    await profiles.getByText('Мод BlockOps 0.1.0 установлен.').waitFor();
    await profiles.getByRole('button', { name: 'Удалить мод BlockOps' }).click();
    await profiles.getByText('Мод BlockOps не установлен.').waitFor();
    assert.deepEqual(await profiles.evaluate(() => window.__modCalls), [
      ['install_blockops_mod', { folder: 'C:\\MyLauncher', gameDir: 'C:\\Games\\Minecraft' }],
      ['remove_blockops_mod', { gameDir: 'C:\\Games\\Minecraft', loader: 'fabric', minecraft: '1.21.1' }],
    ]);
    const [scanArgs] = await profiles.evaluate(() => window.__calls);
    assert.equal(scanArgs.folder, 'C:\\MyLauncher');
    await profiles.getByRole('button', { name: 'Выбрать эту папку' }).first().click();
    assert.equal(await profiles.getByLabel('Папка игровой сборки').inputValue(), 'C:\\Games\\Minecraft');
    assert.equal(await profiles.getByRole('button', { name: 'Сохранить выбор' }).isDisabled(), true);
    await profiles.getByLabel('Какой загрузчик установлен?').selectOption('unknown');
    await profiles.getByRole('button', { name: 'Сохранить выбор' }).click();
    await profiles.getByText(/Совместимость ещё не проверена/).waitFor();
    const [, savedArgs] = await profiles.evaluate(() => window.__calls);
    assert.deepEqual(savedArgs.selection, { game_dir: 'C:\\Games\\Minecraft', loader: 'unknown' });
    await profiles.close();

    // Readiness at a glance: Minecraft, the world or the server entry, and the optional mod.
    const ready = await browser.newPage();
    await ready.addInitScript(() => {
      window.__local = { running: 0, open_worlds: 0, in_menu: 0, lan_worlds: [] };
      window.__entry = false;
      window.__readyCalls = [];
      window.__mod = { bundled: '0.1.0', installed: [], current: false };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'local_minecraft') return window.__local;
        if (command === 'discover_minecraft_profiles') return [
          { name: 'Fabric', game_dir: 'C:\\Games\\Fabric', version_id: 'fabric-loader-1.21.1', minecraft_version: '1.21.1', loader: 'fabric', metadata_complete: true },
          { name: 'Последняя версия', game_dir: 'C:\\Minecraft', version_id: '1.20.1', minecraft_version: '1.20.1', loader: 'vanilla', metadata_complete: true },
        ];
        if (command === 'blockops_mod_targets') return [{ loader: 'fabric', minecraft: '1.21.1' }];
        if (command === 'blockops_mod_status') return window.__mod;
        if (command === 'install_blockops_mod') {
          window.__readyCalls.push([command, args]);
          window.__mod = { bundled: '0.1.0', installed: ['0.1.0'], current: true };
          return window.__mod;
        }
        if (command === 'server_entry_present') return window.__entry;
        if (command === 'add_server_entry') { window.__readyCalls.push([command]); window.__entry = true; return { address: '127.0.0.1:25595', added: 1, present: 0, failed: 0 }; }
        if (command === 'detect_open_world') return { id: 3, port: 41234, name: 'Steve - World', version: '1.20.1' };
        if (command === 'world_watch_alive') return true;
        if (command === 'servers' || command === 'codes') return [];
        return null;
      }};
    });
    await ready.goto(baseUrl);
    const panel = ready.locator('.readiness');
    await panel.getByText('Сборок: 2 — Fabric 1.21.1, Vanilla 1.20.1').waitFor();
    await panel.getByText('Осталось открыть мир для сети.').waitFor();
    await panel.getByText('В игре: Esc → «Открыть для сети».').waitFor();
    // The world shows up by itself once it is opened to LAN.
    await ready.evaluate(() => { window.__local = { running: 0, open_worlds: 1, in_menu: 0, lan_worlds: [{ port: 41234, name: 'Steve - World', version: '1.20.1' }] }; });
    await panel.getByText('Найден «Steve - World», Minecraft 1.20.1.').waitFor();
    await panel.getByText('Всё готово к игре.').waitFor();
    // The mod is offered for the fitting build only, and installed only after confirmation.
    await panel.getByRole('button', { name: 'Установить в «Fabric» (Fabric 1.21.1)' }).click();
    await panel.getByText(/добавит мод 0.1.0 в папку mods сборки «Fabric»/).waitFor();
    assert.equal(await ready.evaluate(() => window.__readyCalls.length), 0, 'installed without confirmation');
    await panel.getByRole('button', { name: 'Установить', exact: true }).click();
    await panel.getByText('Установлен: Fabric 1.21.1.').waitFor();
    // A guest needs the server entry, added with one click.
    await ready.getByRole('button', { name: /Присоединиться/ }).click();
    await panel.getByText('Осталось добавить сервер в Minecraft.').waitFor();
    await panel.getByRole('button', { name: 'Добавить в Minecraft' }).click();
    await panel.getByText('«BlockOps — мир друга» есть в «Сетевой игре».').waitFor();
    await panel.getByText('Всё готово к игре.').waitFor();
    assert.deepEqual(await ready.evaluate(() => window.__readyCalls), [
      ['install_blockops_mod', { folder: null, gameDir: 'C:\\Games\\Fabric' }],
      ['add_server_entry'],
    ]);
    await ready.close();
  } finally {
    if (browser) await browser.close();
    server.kill();
  }
});
