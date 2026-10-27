# Controlled pilot assets

These templates are starting points for an administrator; copying them does not deploy a
service or open firewall ports. See [coordination](../../docs/coordination.md) and
[relay](../../docs/relay.md) for protocol details, limits and threat model.

1. Build Linux binaries on a trusted system with `cargo build --release --locked -p
   blockops-coordination -p blockops-relay`, and install them as
   `/opt/blockops/blockops-coordination` and `/opt/blockops/blockops-relay`.
2. Create a dedicated unprivileged `blockops` user and group. Create `/var/lib/blockops`
   owned by that user, mode 0700. Store and back up its relay identity securely.
3. Copy the systemd service files to `/etc/systemd/system/`. Replace
   `REPLACE_WITH_PUBLIC_IPV4` with the actual publicly routable IPv4 address; make sure
   it matches the network address and TCP port the clients can reach.
4. Replace `connect.example.org` in `Caddyfile` with your actual domain, install the file
   through your existing Caddy deployment, and validate it with `caddy validate --config
   /etc/caddy/Caddyfile`. Obtain a trusted HTTPS certificate for that domain.
5. Open TCP 80/443 for Caddy and TCP 4001 for relay on the VPS firewall, after the operator
   has chosen to expose these services. Keep the coordinator's 8080 bound to loopback.
   Enable the two units with systemd and verify their logs without recording secrets.
6. Construct the relay multiaddr from the public IP, port and relay's printed PeerId;
   give users the HTTPS origin and this multiaddr. Test both with actual host and guest
   on different networks, including code reuse and revocation. `GET /health` only checks
   process liveness, not HTTPS configuration, relay reservation or game traffic.

Coordinator state is volatile, and all connected grants are revoked after an outage.
This pilot has no accounts, global traffic billing, high availability, per-user quotas,
DDoS protection or automatic DNS/NAT discovery. Configure edge limits and monitor VPS
costs. Behind the reverse proxy the current coordinator IP rate limit is shared by all
clients. Do not publish as an unrestricted free public service.
