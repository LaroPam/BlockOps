use anyhow::{ensure, Result};
use clap::Parser;
use futures::StreamExt;
use libp2p::{
    identify, noise, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, SwarmBuilder,
};
use std::{io::Write, path::PathBuf, time::Duration};
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/ip4/127.0.0.1/tcp/4001")]
    listen: Multiaddr,
    /// Routable address, e.g. /ip4/203.0.113.10/tcp/4001. Required on a public VPS.
    #[arg(long)]
    external: Option<Multiaddr>,
    /// Private identity file, created exclusively when missing. Keep outside the checkout.
    #[arg(long)]
    identity_file: Option<PathBuf>,
    #[arg(long, default_value_t = 67108864)]
    max_circuit_bytes: u64,
    #[arg(long, default_value_t = 3600)]
    max_circuit_seconds: u64,
}
#[derive(NetworkBehaviour)]
struct Behaviour {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.max_circuit_seconds > 0 && args.max_circuit_seconds <= u32::MAX as u64,
        "Invalid duration"
    );
    ensure!(args.max_circuit_bytes > 0, "Byte quota must be positive");
    let key = match args.identity_file {
        Some(path) => match std::fs::read(&path) {
            Ok(bytes) => libp2p::identity::Keypair::from_protobuf_encoding(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = libp2p::identity::Keypair::generate_ed25519();
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(path)?;
                file.write_all(&key.to_protobuf_encoding()?)?;
                file.sync_all()?;
                key
            }
            Err(e) => return Err(e.into()),
        },
        None => libp2p::identity::Keypair::generate_ed25519(),
    };
    let config = relay::Config {
        max_reservations: 32,
        max_reservations_per_peer: 2,
        max_circuits: 64,
        max_circuits_per_peer: 8,
        max_circuit_duration: Duration::from_secs(args.max_circuit_seconds),
        max_circuit_bytes: args.max_circuit_bytes,
        ..Default::default()
    };
    let mut swarm = SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| Behaviour {
            relay: relay::Behaviour::new(key.public().to_peer_id(), config),
            identify: identify::Behaviour::new(identify::Config::new(
                "/blockops/1".into(),
                key.public(),
            )),
        })?
        .build();
    if let Some(address) = args.external {
        swarm.add_external_address(address);
    }
    swarm.listen_on(args.listen)?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    // Loopback is advertised for local tests only; public deployment passes --external.
                    swarm.add_external_address(address.clone());
                    println!("{address}/p2p/{}", swarm.local_peer_id());
                }
                SwarmEvent::ListenerError { error, .. } => eprintln!("Listener error: {error}"),
                _ => {}
            }
        }
    }
    Ok(())
}
