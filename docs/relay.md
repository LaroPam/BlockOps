# Experimental public relay

Use a Linux VPS with a public IPv4 address and an explicit inbound **TCP 4001** firewall
rule. No UDP rule is required by this TCP-only PoC. Opening this port is a deliberate
operator action; BlockOps does not alter firewalls or routers. Guests and hosts only need
outbound access to this port. DNS multiaddrs are not enabled: use literal IP multiaddrs.

Build from the repository with the pinned Rust toolchain:

```sh
cargo build --release --locked -p blockops-relay
```

Create a dedicated unprivileged `blockops` service account, install the binary as
`/opt/blockops/blockops-relay`, and provision a writable `/var/lib/blockops` owned by that
account (mode 0700). Example systemd unit, installed by the administrator:

```ini
[Unit]
Description=BlockOps experimental circuit relay
After=network-online.target
Wants=network-online.target

[Service]
User=blockops
Group=blockops
ExecStart=/opt/blockops/blockops-relay --listen /ip4/0.0.0.0/tcp/4001 --external /ip4/PUBLIC_IP/tcp/4001 --identity-file /var/lib/blockops/identity.key --max-circuit-bytes 67108864 --max-circuit-seconds 3600
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/blockops
UMask=0077
MemoryMax=512M
TasksMax=128

[Install]
WantedBy=multi-user.target
```

Replace `PUBLIC_IP` with the VPS address. Enable with `systemctl enable --now blockops-relay`
and inspect `journalctl -u blockops-relay`. Use the peer ID printed at startup with the
**public** address, never the wildcard or private interface address:
`/ip4/PUBLIC_IP/tcp/4001/p2p/PEER_ID`. Check readiness by running the host and guest commands
from the development guide on external machines and transferring real data; an open port
alone does not verify relay reservation or authenticated traffic.

The identity file is generated with exclusive creation and Unix mode 0600. Back it up
securely; never commit it. Without `--identity-file`, each restart changes PeerId and
invalidates relay addresses. Even with persistent identity, process restart terminates
circuits; restart host/join sessions to obtain a fresh reservation/invite.

## Quotas and billing

Defaults: 32 reservations, 2 per peer; 64 circuits, 8 per peer; each circuit has a 64 MiB
aggregate byte budget and one-hour duration. These are hard disconnect limits, not per-user
billing quotas. A circuit may carry multiple game TCP streams; traffic includes encrypted
protocol overhead. Minecraft world transfers can exhaust 64 MiB quickly. Increase limits
only with a traffic budget. Monitor provider ingress/egress accounting and enforce a total
server egress budget externally. The relay cannot inspect game contents.

The library retains reservation/circuit rate limiting. However, arbitrary peers can reserve
circuits: account admission, payment/quota enforcement, global bandwidth control, abuse
reporting and operational monitoring are not implemented. Deploy only a controlled pilot,
restrict source IPs during tests when possible, and do not announce this as a free public
relay service. Capability authorization is enforced at the host, not at the relay.

Noise authenticates peer identities and encrypts the end-to-end stream. The relay learns
peer IDs, IPs, timing and traffic volumes, but not the tunneled payload or invitation token.
Do not place an HTTP reverse proxy in front of this raw libp2p TCP listener.
