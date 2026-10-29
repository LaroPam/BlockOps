# Итерация 3: одноразовые коды и участники

`crates/coordination` содержит HTTP API, клиентскую библиотеку и CLI. Desktop создаёт
контролируемый туннель, затем выдаёт отдельный код для каждого друга. Для подключения
гость указывает тот же coordinator URL и вводит код. Ручной JSON-путь остаётся только
совместимым PoC в старом CLI `blockops-tunnel`; его bearer-приглашения не приобретают
одноразовость автоматически.

## Контракт доступа

- Код: 12 символов Crockford alphabet, 60 случайных бит, вид `XXXX-XXXX-XXXX`,
  обычно до 10 минут (API допускает 1–900 секунд, не дольше жизни host session).
  При погашении регистр, дефисы и пробелы не важны, а O, I и L читаются как 0, 1 и 1:
  этих букв в алфавите нет, поэтому ошибка ввода не меняет смысл кода.
- Координатор хранит SHA-256 кода, invitation capability и хеш отдельного 256-битного
  management token. Создание не требует аккаунта: это сервис для контролируемого пилота.
- Клиент гостя создаёт libp2p keypair, передаёт PeerId при погашении и использует
  **тот же ключ** в Noise handshake. PeerId из HTTP сам по себе не считается доказательством
  владения ключом: его проверяет хост через аутентифицированный libp2p stream.
- Погашение атомарное; из двух конкурентных запросов проходит один. Хост опрашивает claim,
  привязывает локальный grant к PeerId и подтверждает его. Только после подтверждения
  coordinator возвращает гостю capability. Иного локального порта/файлового API нет.
- Скопированная capability с другой identity отклоняется **до подключения к target**.
  Один grant разрешает несколько Minecraft TCP streams одному участнику.
- Отзыв необратим: блокирует новые streams и закрывает активные потоки этого участника.
  Другие grants сохраняются. Desktop связывает grants с generation Java-процесса.
- Хост опрашивает coordinator каждые 500 мс. Ошибка control-plane закрывает доступ;
  HTTP timeout запроса — 3 секунды. После outage нужно выдать новый код. Автоматическое
  восстановление доступа сознательно не реализовано: это ограничение доступности,
  а не обещание бесшовного переподключения Minecraft.

Координатор является **доверенным** компонентом: он видит capability/PeerId и может
влиять на выдачу доступа. Это не схема с недоверенным посредником. Передача управления
защищена стандартным HTTPS; игровой поток — Noise. Собственной криптографии нет.
Код передавайте приватно; его знание позволяет первому предъявителю получить доступ.
Подтверждённые имена/аккаунты друзей пока отсутствуют.

## Локальная проверка

```sh
cargo build --workspace --locked
cargo test --workspace --locked
python3 scripts/coordination_smoke.py
python3 scripts/https_pilot_smoke.py
```

Smoke запускает **четыре отдельных процесса**: coordinator, relay, host, guest. Проверяет
бинарный echo через relay, затем отказ повторного погашения. Для настоящего уже запущенного
Minecraft, с ранее принятыми условиями:

```sh
python3 scripts/coordination_smoke.py --minecraft-port 25565
```

HTTPS smoke дополнительно проверяет TLS с временным тестовым CA и отказ при недоверенном
сертификате, не изменяя системное доверие. Он использует локальный TLS terminator, не Caddy.

Minecraft smoke проверяет Minecraft 1.21.1 status/ping; вход аккаунта не проверяет и EULA не принимает.

Ручной запуск в разных терминалах:

```sh
target/debug/blockops-coordination serve --bind 127.0.0.1:8080
target/debug/blockops-relay --listen /ip4/127.0.0.1/tcp/4001
# Используйте реальный PeerId, напечатанный relay:
target/debug/blockops-coordination host --coordinator http://127.0.0.1:8080 \
  --target 127.0.0.1:25565 --relay /ip4/127.0.0.1/tcp/4001/p2p/PEER_ID
# Сохраните напечатанный секретный код в приватный файл вне Git:
target/debug/blockops-coordination join --coordinator http://127.0.0.1:8080 \
  --code-file /private/code.txt --bind 127.0.0.1:25566
```

Код читается из файла, чтобы не попадать в argv/history. CLI host выдаёт один код;
desktop поддерживает до 8 кодов и отдельную кнопку отзыва каждого участника. Ctrl+C
останавливает сессию. Ключи участников пока не сохраняются: после перезапуска гостя нужен
новый код. Потеря HTTP-ответа после погашения тоже требует нового кода; это fail-closed
семантика, а не идемпотентная доставка приглашения.

## API

| Метод | Путь | Назначение |
|---|---|---|
| GET | /health | Процесс отвечает; не проверяет relay/host |
| POST | /v1/invites | Создать код; JSON invite + code_ttl |
| POST | /v1/redeem | Погасить; JSON code + peer, ожидание ack до 10 с |
| GET | /v1/invites/{id} | Claim; management bearer required |
| POST | /v1/invites/{id}/activate | Host ack; management bearer required |
| DELETE | /v1/invites/{id} | Отзыв; management bearer required |

Invalid/used/expired code возвращает одинаковый 404. Временный отказ rate limit — 429.
Ожидание недоступного хоста — 504; код уже погашен и повторно не используется.
Тела запросов ограничены 8 КиБ; 2048 записей; 4096 buckets IP; 30 запросов создания/
погашения в минуту на IP; 128 одновременно обрабатываемых запросов. Хост ограничивает
32 grants и 32 streams. Не считайте эти лимиты production-защитой от ботнета.

## Публичный controlled pilot

Нужны VPS, DNS, HTTPS-сертификат и отдельно реально доступный libp2p relay. Coordinator
**не предоставляет relay и не гарантирует NAT traversal**. Клиент запрещает remote HTTP,
credentials/query/path в origin URL и HTTP redirects, чтобы не переносить секреты на
другой origin. HTTP допускается только для literal loopback локального теста.

Готовые шаблоны systemd и Caddy находятся в [deploy/pilot](../deploy/pilot/README.md).

Соберите `cargo build --release --locked -p blockops-coordination`. Установите бинарник
на VPS от администратора и запустите выделенным непривилегированным пользователем:

```ini
[Unit]
Description=BlockOps coordination pilot
After=network-online.target
[Service]
User=blockops
ExecStart=/opt/blockops/blockops-coordination serve --bind 127.0.0.1:8080
Restart=on-failure
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
MemoryMax=256M
TasksMax=128
LimitNOFILE=1024
[Install]
WantedBy=multi-user.target
```

Перед ним настройте HTTPS reverse proxy. Пример Caddyfile (замените домен на свой):

```caddy
connect.example.org {
    reverse_proxy 127.0.0.1:8080
}
```

Открытие 80/443 и relay TCP-порта — явное действие администратора. Backend 8080 не
публикуется. Не логируйте тела запросов/ответов и Authorization; коды не передаются в URL.
Поставьте edge connection limits и общий request budget. X-Forwarded-For сейчас **не
доверяется**: за reverse proxy лимит 30/min общий для его адреса. Per-user quotas,
аккаунты, распределённая БД, HA и биллинг не реализованы. Не запускайте несколько
реплик: хранилище в памяти. Перезапуск теряет записи и отзывает доступ у подключённых
хостов при следующем poll; игра прервётся. Relay имеет свои отдельные лимиты/расходы.

Проверяйте публичное развёртывание реальными host/guest с двух внешних сетей, включая
повторный код и отзыв активного соединения. `/health` недостаточно. В этой итерации
публичная инфраструктура не развёртывалась и CGNAT-проверки не выполнялись.

При использовании актуального клиента loopback-порт гостя резервируется до погашения
кода, поэтому занятый порт не расходует приглашение. Неподтверждённый claim занимает
запись не дольше 10 секунд; истёкший код повторно не активируется.
