//! Translate an explicit environment HTTPS proxy into supported JVM properties.
//! Never place proxy credentials in command-line arguments or logs.
use anyhow::{ensure, Context, Result};
use std::path::Path;

pub fn proxy_args(proxy: &str) -> Result<Vec<String>> {
    let url = reqwest::Url::parse(proxy)
        .map_err(|_| anyhow::anyhow!("Invalid HTTPS proxy configuration"))?;
    ensure!(
        url.scheme() == "http" && url.username().is_empty() && url.password().is_none(),
        "Java currently supports a credential-free HTTP CONNECT proxy only"
    );
    let host = url.host_str().context("HTTPS proxy has no hostname")?;
    let port = url
        .port_or_known_default()
        .context("HTTPS proxy has no port")?;
    Ok(vec![
        format!("-Dhttps.proxyHost={host}"),
        format!("-Dhttps.proxyPort={port}"),
        "-Dhttp.nonProxyHosts=localhost|127.*|[::1]".into(),
    ])
}
pub fn configure(command: &mut tokio::process::Command) -> Result<()> {
    for name in [
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "CLASSPATH",
    ] {
        command.env_remove(name);
    }
    if let Some(proxy) = std::env::var("HTTPS_PROXY")
        .ok()
        .or_else(|| std::env::var("https_proxy").ok())
        .filter(|p| !p.is_empty())
    {
        command.args(proxy_args(&proxy)?);
        // Debian maintains this truststore from the system CA set, including authorized
        // enterprise/proxy roots. Keep certificate verification enabled.
        let system_store = Path::new("/etc/ssl/certs/java/cacerts");
        if cfg!(target_os = "linux") && system_store.is_file() {
            command.arg(format!(
                "-Djavax.net.ssl.trustStore={}",
                system_store.display()
            ));
        }
    }
    Ok(())
}
