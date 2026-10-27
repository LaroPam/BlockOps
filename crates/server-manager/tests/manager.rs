use blockops_server_manager::{artifacts, properties_match, Difficulty, Manager, Settings};
use std::io::Write;
fn settings() -> Settings {
    Settings {
        name: "World".into(),
        memory_mb: 2048,
        port: 25565,
        max_players: 10,
        difficulty: Difficulty::Normal,
    }
}
#[tokio::test]
async fn eula_is_required_before_network_or_install() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path()).unwrap();
    let error = manager.install(settings(), false).await.err().unwrap();
    assert!(error.to_string().contains("EULA"));
    assert!(manager.list().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(root.path().join("servers"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn settings_and_properties_enforce_safe_launch() {
    let safe = settings().properties().unwrap();
    assert!(safe.contains("online-mode=true\n"));
    assert!(safe.contains("server-ip=127.0.0.1\n"));
    assert!(properties_match(&format!("# generated\nview-distance=10\n{safe}"), &safe).unwrap());
    assert!(!properties_match(
        &safe.replace("online-mode=true", "online-mode=false"),
        &safe
    )
    .unwrap());
    assert!(!properties_match(
        &safe.replace("server-ip=127.0.0.1", "server-ip=0.0.0.0"),
        &safe
    )
    .unwrap());
    assert!(properties_match(&format!("{safe}online-mode=false\n"), &safe).is_err());
    assert!(properties_match(&format!("{safe}online\\u002dmode=false\n"), &safe).is_err());
    // Java Properties also accepts ':' and whitespace as key separators.  An
    // attacker must not be able to append an alternate spelling that our
    // validator treats as an unrelated key but Java uses to overwrite safety.
    for override_line in [
        "online-mode:false=ignored",
        "server-ip:0.0.0.0=ignored",
        "online-mode false=ignored",
        "server-ip 0.0.0.0=ignored",
    ] {
        assert!(properties_match(&format!("{safe}{override_line}\n"), &safe).is_err());
    }
    let mut bad = settings();
    bad.memory_mb = 10;
    assert!(bad.validate().is_err());
    bad = settings();
    bad.name = "name\nonline-mode=false".into();
    assert!(bad.validate().is_err());
}
#[test]
fn checksum_rejects_corruption() {
    artifacts::verify(b"abc", "a9993e364706816aba3e25717850c26c9cd0d89d", false).unwrap();
    assert!(artifacts::verify(b"abd", "a9993e364706816aba3e25717850c26c9cd0d89d", false).is_err());
    artifacts::verify(
        b"abc",
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        true,
    )
    .unwrap();
}
#[test]
fn only_official_https_sources() {
    assert!(artifacts::official_url(
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
    )
    .is_ok());
    for url in [
        "http://piston-data.mojang.com/server.jar",
        "https://evil.example/server.jar",
        "https://piston-data.mojang.com.evil.example/x",
        "https://user@github.com/x",
        "https://github.com:444/x",
    ] {
        assert!(artifacts::official_url(url).is_err());
    }
}
fn zip_file(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap().into_inner()
}
#[test]
fn zip_extracts_runtime_and_blocks_traversal() {
    let root = tempfile::tempdir().unwrap();
    artifacts::extract(&zip_file("jre/bin/java.exe", b"runtime"), root.path(), true).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("jre/bin/java.exe")).unwrap(),
        b"runtime"
    );
    for name in ["../escape", "/absolute", "jre/../../escape"] {
        assert!(artifacts::extract(&zip_file(name, b"bad"), root.path(), true).is_err());
    }
    // Duplicate entries cannot silently replace a previously extracted file.
    assert!(artifacts::extract(&zip_file("jre/bin/java.exe", b"bad"), root.path(), true).is_err());
}
#[test]
fn database_survives_reopen_and_rejects_path_ids() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path()).unwrap();
    let server = blockops_server_manager::Server {
        id: uuid::Uuid::new_v4().to_string(),
        settings: settings(),
        version: artifacts::VERSION.into(),
        jar_sha1: "00".repeat(20),
        eula_accepted_at: 1,
    };
    let db = rusqlite::Connection::open(root.path().join("blockops.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO servers(id,metadata) VALUES (?1,?2)",
        rusqlite::params![server.id, serde_json::to_string(&server).unwrap()],
    )
    .unwrap();
    drop(manager);
    let manager = Manager::open(root.path()).unwrap();
    assert_eq!(manager.list().unwrap().len(), 1);
    assert_eq!(manager.get(&server.id).unwrap().settings.name, "World");
    assert!(manager.get("../../anything").is_err());
}
#[cfg(unix)]
mod supervisor {
    use blockops_server_manager::process;
    use std::time::Duration;
    fn launch(script: &str, timeout: u64) -> (tempfile::TempDir, process::Process) {
        let root = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(root.path().join("lock")).unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", script]);
        let process = process::spawn(command, file, Duration::from_secs(timeout)).unwrap();
        (root, process)
    }
    async fn phase(
        receiver: &mut tokio::sync::watch::Receiver<process::ProcessStatus>,
        expected: &str,
    ) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if receiver.borrow().phase == expected {
                    return;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn readiness_logs_and_graceful_stop() {
        let (_root, process) = launch(
            "printf 'Done (0.1s)! For help, type help\n'; read command; test \"$command\" = stop",
            2,
        );
        let mut status = process.status.clone();
        phase(&mut status, "running").await;
        process.stop().await;
        assert_eq!(status.borrow().phase, "stopped");
        assert_eq!(status.borrow().exit_code, Some(0));
        assert!(status.borrow().logs.iter().any(|l| l.contains("Done (")));
    }
    #[tokio::test]
    async fn crash_records_exit_and_stderr() {
        let (_root, process) = launch("printf 'bind failed\n' >&2; exit 3", 2);
        let mut status = process.status.clone();
        phase(&mut status, "failed").await;
        process.stop().await;
        assert_eq!(status.borrow().exit_code, Some(3));
        assert!(status
            .borrow()
            .logs
            .iter()
            .any(|l| l.contains("bind failed")));
    }
    #[tokio::test]
    async fn unresponsive_process_is_terminated() {
        let (_root, process) = launch(
            "printf 'Done (0.1s)! For help, type help\n'; while read command; do :; done",
            1,
        );
        let mut status = process.status.clone();
        phase(&mut status, "running").await;
        process.stop().await;
        assert_eq!(status.borrow().phase, "failed");
        assert!(status
            .borrow()
            .error
            .as_ref()
            .unwrap()
            .contains("timed out"));
    }
    #[tokio::test]
    async fn output_retention_is_bounded() {
        let (_root,process)=launch("i=0; while [ $i -lt 250 ]; do echo line; i=$((i+1)); done; printf 'Done (0.1s)! For help, type help\n'; read command",2);
        let mut status = process.status.clone();
        phase(&mut status, "running").await;
        process.stop().await;
        assert_eq!(status.borrow().logs.len(), 200);
    }
}

#[tokio::test]
async fn installation_is_exclusive_across_managers() {
    use fs2::FileExt;
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path()).unwrap();
    let file = std::fs::File::create(root.path().join("install.lock")).unwrap();
    file.try_lock_exclusive().unwrap();
    let error = manager.install(settings(), true).await.err().unwrap();
    assert!(format!("{error:#}").contains("already owns"));
    assert!(manager.list().unwrap().is_empty());
}
#[cfg(unix)]
#[test]
fn tar_links_must_stay_inside_staging() {
    fn archive(target: &str) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut file = tar::Header::new_gnu();
        file.set_size(3);
        file.set_mode(0o755);
        file.set_cksum();
        tar.append_data(&mut file, "jre/bin/java", &b"abc"[..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        tar.append_link(&mut link, "jre/bin/link", target).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }
    let root = tempfile::tempdir().unwrap();
    artifacts::extract(&archive("java"), root.path(), false).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("jre/bin/link")).unwrap(),
        b"abc"
    );
    let other = tempfile::tempdir().unwrap();
    assert!(artifacts::extract(&archive("/etc/passwd"), other.path(), false).is_err());
    let other = tempfile::tempdir().unwrap();
    assert!(artifacts::extract(&archive("../../../../etc/passwd"), other.path(), false).is_err());
}

#[test]
fn java_proxy_preserves_tls_and_does_not_expose_credentials() {
    let args = blockops_server_manager::java::proxy_args("http://proxy.example:3128").unwrap();
    assert!(args.contains(&"-Dhttps.proxyHost=proxy.example".into()));
    assert!(args.contains(&"-Dhttps.proxyPort=3128".into()));
    let error = blockops_server_manager::java::proxy_args("http://user:secret@proxy.example:3128")
        .unwrap_err()
        .to_string();
    assert!(!error.contains("secret"));
    assert!(blockops_server_manager::java::proxy_args("socks5://proxy.example:1080").is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn authentication_network_failure_is_visible() {
    let root = tempfile::tempdir().unwrap();
    let file = std::fs::File::create(root.path().join("lock")).unwrap();
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c","printf 'Failed to request yggdrasil public key\nDone (0.1s)! For help, type help\n'; read command"]);
    let process =
        blockops_server_manager::process::spawn(command, file, std::time::Duration::from_secs(2))
            .unwrap();
    let mut status = process.status.clone();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if status.borrow().phase == "running" {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(status
        .borrow()
        .error
        .as_ref()
        .unwrap()
        .contains("authentication keys"));
    process.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn console_delivers_one_command_and_rejects_injection_and_stop() {
    use blockops_server_manager::process;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let file = std::fs::File::create(root.path().join("lock")).unwrap();
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c", "printf 'Done (0.1s)! For help, type help\\n'; while read input; do [ \"$input\" = stop ] && exit 0; printf 'received:%s\\n' \"$input\"; done"]);
    let process = process::spawn(command, file, Duration::from_secs(2)).unwrap();
    let mut status = process.status.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if status.borrow().phase == "running" {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let console = process.console();
    assert!(console.send("say hi\nstop").await.is_err());
    assert!(console.send("/stop").await.is_err());
    assert!(console.send("minecraft:stop").await.is_err());
    assert!(console.send(&"x".repeat(1025)).await.is_err());
    console.send("/say hello").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if status
                .borrow()
                .logs
                .iter()
                .any(|line| line == "received:say hello")
            {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    process.stop().await;
    assert!(console.send("say after exit").await.is_err());
}

#[tokio::test]
async fn version_selection_rejects_unapproved_and_unsupported_releases() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path()).unwrap();
    assert!(manager
        .install_version(settings(), false, "1.21.4")
        .await
        .is_err());
    assert!(manager
        .install_version(settings(), true, "1.20.1")
        .await
        .is_err());
    assert!(manager
        .install_version(settings(), true, "../1.21.4")
        .await
        .is_err());
    assert!(manager.list().unwrap().is_empty());
    assert!(artifacts::supported_version("1.21.1"));
    assert!(artifacts::supported_version("1.21.4"));
}
