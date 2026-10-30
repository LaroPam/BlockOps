# BlockOps

Open-source desktop foundation for hosting Minecraft Java and connecting friends through
an encrypted application-level TCP tunnel. **Iteration 3: Vanilla manager, identity-bound one-time codes and working relay tunnel.** Windows is the only desktop target for the current release. Linux is used for headless development and CI; a Linux or macOS desktop release is not planned for this stage.

- [Playing without a server of our own](docs/direct.md)
- [One-time codes, participant revocation and coordinator deployment](docs/coordination.md)
- [Vanilla server installation, lifecycle and current limitations](docs/servers.md)
- [Architecture, feasibility, trust boundaries and roadmap](docs/architecture.md)
- [Connecting an existing Minecraft world](docs/lan-world.md)
- [Discovering Minecraft profiles](docs/minecraft-profiles.md)
- [Experimental client mod for Fabric, Forge and NeoForge](mods/README.md)
- [Local verification and Windows desktop](docs/development.md)
- [Windows preview installation and test guide](docs/windows-test.md)
- [Public relay deployment and traffic limits](docs/relay.md)

## Quick verification

Install the Rust toolchain pinned in `rust-toolchain.toml` and Node.js 22 LTS.

```sh
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/smoke.py
python3 scripts/coordination_smoke.py
python3 scripts/https_pilot_smoke.py
npm ci
npm run build
```

The integration suite creates isolated libp2p clients and a separate relay process and
checks real TCP data, authorization, expiry, isolation and shutdown. The Python smoke
check additionally starts **two CLI client processes** and a relay process.

`npm run desktop` starts Tauri once OS prerequisites are installed. A browser-only
`npm run dev` can show the interface but explicitly disables networking actions.

## Included

- Vanilla 1.21.1 and 1.21.4 manager: official verified JAR/Java downloads, explicit EULA acceptance,
  SQLite registry, isolated worlds, safe settings, process supervisor and local command console.
- Serverless mode by default: players meet through the public libp2p (IPFS) network, prove a
  one-use 80-bit code with SPAKE2 bound to their peer ids, and play over a hole-punched direct
  connection; public relays carry only the meeting.
- Optional coordination service: single-use codes, guest identity binding, individual revocation,
  bounded request rates and fail-closed access on coordinator outage.
- Offline world backups with checksum verification and recoverable restoration; the previous world is retained.
- Edit managed settings for stopped servers with crash recovery, including the Minecraft whitelist.
- Shared Rust host/join API, CLI and Tauri command adapters.
- Noise-authenticated, multiplexed TCP streams; explicit loopback target; secret expiring
  invitations; fixed destination; bounded stream concurrency and authentication timeout.
- Standalone circuit relay v2, persistent optional identity, configurable traffic limits.
- UI to start/stop sessions, exchange a secret invitation, obtain a local Minecraft address,
  and inspect actual connection counts, observed guest transport and last errors.
- Worlds opened to LAN are found automatically in any Minecraft Java version, from the game's own
  LAN announcement (accepted only from this computer), with name and version from a status ping;
  the tunnel stops once the announcements stop.
- Guests use a fixed local address, and one click saves it as a server in Minecraft's own list
  (servers.dat, edited without touching other entries), so joining takes two clicks in any version.
- Experimental Fabric 1.21.1 companion: on Windows the desktop notices an opened LAN world by itself,
  one click prepares the connection and a copyable code, and the guest's Minecraft joins once it
  reaches the main menu. It works through an owner-only descriptor and an HMAC-authenticated one-use
  local bridge; a manually copied code remains as a fallback. The installer carries the mod, which
  needs only Fabric Loader, and the desktop installs it into a Fabric 1.21.1 launcher profile after
  the player confirms.

## Not included yet

Other Minecraft versions/loaders, automatic backups, accounts,
reliable NAT test matrix, automatic reconnect and signed installers. Nothing disables `online-mode` or opens router ports. Internet play
works directly when hole punching succeeds; otherwise it needs an explicitly configured, reachable relay. The desktop uses per-member identity-bound codes. Legacy blockops-tunnel CLI JSON invites
remain bearer secrets and are intended only for PoC compatibility.

The network core and an actual installed Vanilla server were tested in this cloud Linux instance.
Minecraft status and ping/pong passed through the relay; authenticated player login is not
verified because Vanilla authlib bypasses the required cloud HTTPS proxy and direct DNS fails. Desktop native compilation is
blocked in the Linux development container by missing GLib/GTK/WebKit development libraries.
Windows CI builds an unsigned installer and tests silent installation, native window startup and removal; interactive
Windows GUI use and authenticated Minecraft player login still need manual validation.

## Stack decisions

Tauri 2, React/TypeScript, Rust/Tokio/libp2p. Small scoped CSS keeps the initial shell simple;
SQLite now persists server metadata; Tailwind/shadcn adoption remains deferred. `libp2p-stream` currently publishes an alpha API; versions are locked and its
usage is confined to the tunnel crate. Assess API stability before a production release.
