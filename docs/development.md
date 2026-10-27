# Local development and verification

## Prerequisites

Rust via official rustup (the repository pins 1.99.1), Node.js 22 LTS/npm, a native C/C++
toolchain. Python 3 is optional for the process-level smoke script. Cargo/npm lockfiles
must be committed. The Tauri crate has a separate lockfile to keep network-only builds
independent of GTK/WebKit system packages.

Windows is the sole desktop target for this stage; Linux commands below support headless development and do not imply a Linux desktop release.

Windows desktop: install Visual Studio Build Tools with **Desktop development with C++**,
Windows SDK and Microsoft Edge WebView2 Runtime. Use the MSVC Rust target. See the official
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

Debian/Ubuntu native desktop additionally needs:

```sh
sudo apt-get install build-essential pkg-config libglib2.0-dev libgtk-3-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

A graphical session is required to launch the desktop. Network tests are headless.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/smoke.py
npm ci
npm run build
npm run desktop
```

For a Windows installer: `npm run tauri -w apps/desktop -- build`. This targets NSIS;
code signing and release distribution are not configured. Do not distribute this PoC as a
finished hosting product. The CI Windows job is configured to build an unsigned NSIS installer and smoke-test installation and removal; it has not yet been run.

## Manual three-process tunnel

Build once using `cargo build --workspace --locked`. On Windows use `.exe` suffixes.
Start a real Minecraft Java server separately on `127.0.0.1:25565`, with accepted EULA
and `online-mode=true`, or use any local TCP echo service for transport checks.

Terminal 1:

```sh
target/debug/blockops-relay --listen /ip4/127.0.0.1/tcp/4001
```

Copy the printed multiaddr including `/p2p/PEER_ID`. Terminal 2:

```sh
target/debug/blockops-tunnel host --target 127.0.0.1:25565 \
  --relay /ip4/127.0.0.1/tcp/4001/p2p/PEER_ID --ttl 3600
```

Save the single JSON line privately to an `invite.json` **outside the repository**. This
line is the secret capability. Don't paste it into issues or public logs. Terminal 3:

```sh
target/debug/blockops-tunnel join --invite-file /private/path/invite.json --bind 127.0.0.1:25566
```

Point Minecraft at `127.0.0.1:25566`. The guest announces listener readiness, not successful
remote authentication; authentication occurs for each TCP connection. Errors go to stderr
and appear in desktop status. Ctrl+C stops a session and closes its TCP connections.
Omit `--relay` on the host for same-machine direct mode. Public relay deployment is separate.
For two machines, both must reach the public relay; send the JSON securely to the guest.

The guest transport status reports the latest established connection to the host, not a
per-stream routing guarantee. Existing streams remain on their original connection if
DCUtR later opens a direct connection. This PoC starts with a relay path and enables DCUtR;
a production direct-first discovery/retry policy has not been implemented or verified.

## Test boundaries

Automated checks use echo targets, not a downloaded Minecraft JAR. They verify binary
payloads, half-close, concurrent streams, wrong secret rejection before target connection,
server-side expiry even if a guest edits the JSON, loopback-only bindings, unreachable target
errors and cancellation. The process smoke test verifies the public CLI path too.

No public VPS was deployed in this iteration. NAT/CGNAT hole-punching, relay outages,
long-running Minecraft traffic, Windows firewall/Defender, installer and desktop GUI
interaction require subsequent validation. Returning to a ready listener does not imply
that an interrupted Minecraft TCP connection can be resumed.

## Cloud instance

Installed Rust is in `/workspace/.cargo` and `/workspace/.rustup`. Before Cargo commands:

```sh
export CARGO_HOME=/workspace/.cargo
export RUSTUP_HOME=/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
export npm_config_cache=/workspace/.npm
cd /workspace/BlockOps
```

Use the existing checkout; cloud tasks are already isolated. Do not create a Git worktree
unless explicitly requested. No service must be running for `cargo test`: tests own their
listeners and relay subprocesses and clean them up.
