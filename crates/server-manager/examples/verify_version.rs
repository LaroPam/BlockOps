//! Downloads and verifies official server metadata and JAR without accepting EULA or running it.
use anyhow::{Context, Result};
use blockops_server_manager::artifacts;
#[tokio::main]
async fn main() -> Result<()> {
    let version = std::env::args().nth(1).context("Missing version")?;
    let artifact = artifacts::vanilla_version(&version).await?;
    let dir = tempfile::tempdir()?;
    let jar = dir.path().join("server.jar");
    artifacts::download_server(&artifact, &jar).await?;
    artifacts::verify_file(&jar, &artifact.sha1)?;
    let java = artifacts::find_release(&version)
        .context("Unsupported Minecraft version")?
        .java;
    println!(
        "PASS: official Minecraft {version} metadata requires Java {java}; JAR size and SHA-1 verified"
    );
    Ok(())
}
