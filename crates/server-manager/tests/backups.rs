use blockops_server_manager::{artifacts, Difficulty, Manager, Server, Settings};
use fs2::FileExt;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

struct Fixture {
    root: tempfile::TempDir,
    manager: Manager,
    id: String,
    dir: PathBuf,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path()).unwrap();
    let id = Uuid::new_v4().to_string();
    let dir = root.path().join("servers").join(&id);
    fs::create_dir_all(dir.join("world/playerdata")).unwrap();
    fs::create_dir(dir.join("world/empty")).unwrap();
    fs::write(dir.join("world/level.dat"), b"original level").unwrap();
    fs::write(dir.join("world/playerdata/user.dat"), b"original player").unwrap();
    let server = Server {
        id: id.clone(),
        settings: Settings {
            name: "Test".into(),
            memory_mb: 2048,
            port: 25565,
            max_players: 10,
            difficulty: Difficulty::Normal,
        },
        version: artifacts::VERSION.into(),
        jar_sha1: "00".repeat(20),
        eula_accepted_at: 1,
    };
    let db = rusqlite::Connection::open(root.path().join("blockops.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO servers(id,metadata) VALUES (?1,?2)",
        rusqlite::params![id, serde_json::to_string(&server).unwrap()],
    )
    .unwrap();
    Fixture {
        root,
        manager,
        id,
        dir,
    }
}
fn rewrite(path: &Path, mut edit: impl FnMut(&str, &mut Vec<u8>)) {
    let mut zip = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut all = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().to_owned();
        let dir = f.is_dir();
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        edit(&name, &mut data);
        all.push((name, dir, data));
    }
    drop(zip);
    let mut out = ZipWriter::new(fs::File::create(path).unwrap());
    for (name, dir, data) in all {
        if dir {
            out.add_directory(name, SimpleFileOptions::default())
                .unwrap();
        } else {
            out.start_file(name, SimpleFileOptions::default()).unwrap();
            out.write_all(&data).unwrap();
        }
    }
    out.finish().unwrap();
}
#[test]
fn round_trip_preserves_previous_world_and_empty_directories() {
    let f = fixture();
    let first = f.manager.create_backup(&f.id).unwrap();
    assert_eq!(f.manager.list_backups(&f.id).unwrap().len(), 1);
    assert!(first.size_bytes > 0);
    assert_eq!(first.minecraft_version, "1.21.1");
    fs::write(f.dir.join("world/level.dat"), b"new level").unwrap();
    fs::write(f.dir.join("world/playerdata/user.dat"), b"new player").unwrap();
    f.manager.restore_backup(&f.id, &first.id).unwrap();
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
    assert_eq!(
        fs::read(f.dir.join("world/playerdata/user.dat")).unwrap(),
        b"original player"
    );
    assert!(f.dir.join("world/empty").is_dir());
    let previous: Vec<_> = fs::read_dir(&f.dir)
        .unwrap()
        .map(|i| i.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("world-before-restore-")
        })
        .collect();
    assert_eq!(previous.len(), 1);
    assert_eq!(
        fs::read(previous[0].join("level.dat")).unwrap(),
        b"new level"
    );
    assert!(!f.dir.join("restore.pending.json").exists());
}
#[test]
fn file_corruption_never_modifies_existing_world() {
    let f = fixture();
    let backup = f.manager.create_backup(&f.id).unwrap();
    let path = f
        .root
        .path()
        .join("backups")
        .join(&f.id)
        .join(format!("{}.zip", backup.id));
    rewrite(&path, |name, data| {
        if name == "world/level.dat" {
            *data = b"forged level!".to_vec()
        }
    });
    fs::write(f.dir.join("world/level.dat"), b"current").unwrap();
    assert!(f.manager.restore_backup(&f.id, &backup.id).is_err());
    assert_eq!(fs::read(f.dir.join("world/level.dat")).unwrap(), b"current");
    assert!(!f.dir.join("restore.pending.json").exists());
    assert!(!fs::read_dir(&f.dir).unwrap().any(|i| i
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("restore-staging-")));
}
#[test]
fn unsafe_manifest_and_wrong_server_are_rejected() {
    let f = fixture();
    let backup = f.manager.create_backup(&f.id).unwrap();
    let path = f
        .root
        .path()
        .join("backups")
        .join(&f.id)
        .join(format!("{}.zip", backup.id));
    rewrite(&path, |name, data| {
        if name == "manifest.json" {
            let mut m: serde_json::Value = serde_json::from_slice(data).unwrap();
            m["files"][0]["path"] = "../escape".into();
            *data = serde_json::to_vec(&m).unwrap();
        }
    });
    assert!(f.manager.restore_backup(&f.id, &backup.id).is_err());
    assert!(!f.root.path().join("escape").exists());
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
    assert!(f.manager.restore_backup(&f.id, "../../evil").is_err());
}
#[test]
fn active_server_lock_blocks_backup_and_restore() {
    let f = fixture();
    let backup = f.manager.create_backup(&f.id).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(f.dir.join("process.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    assert!(f.manager.create_backup(&f.id).is_err());
    assert!(f.manager.restore_backup(&f.id, &backup.id).is_err());
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
}
#[test]
fn interrupted_swap_rolls_back_without_destroying_old_world() {
    let f = fixture();
    let op = Uuid::new_v4().to_string();
    let stage = f.dir.join(format!("restore-staging-{op}"));
    let old = f.dir.join(format!("world-before-restore-{op}"));
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("level.dat"), b"new").unwrap();
    fs::write(
        f.dir.join("restore.pending.json"),
        serde_json::json!({"id":op}).to_string(),
    )
    .unwrap();
    fs::rename(f.dir.join("world"), &old).unwrap();
    assert!(f.manager.create_backup(&f.id).is_err());
    f.manager.recover_restore(&f.id).unwrap();
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
    assert!(!stage.exists());
    assert!(!f.dir.join("restore.pending.json").exists());
}
#[test]
fn finished_swap_keeps_both_worlds_during_recovery() {
    let f = fixture();
    let op = Uuid::new_v4().to_string();
    let old = f.dir.join(format!("world-before-restore-{op}"));
    fs::write(
        f.dir.join("restore.pending.json"),
        serde_json::json!({"id":op}).to_string(),
    )
    .unwrap();
    fs::rename(f.dir.join("world"), &old).unwrap();
    fs::create_dir(f.dir.join("world")).unwrap();
    fs::write(f.dir.join("world/level.dat"), b"restored").unwrap();
    f.manager.recover_restore(&f.id).unwrap();
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"restored"
    );
    assert_eq!(fs::read(old.join("level.dat")).unwrap(), b"original level");
}
#[tokio::test]
async fn malformed_marker_blocks_start_and_recovery() {
    let f = fixture();
    fs::write(
        f.dir.join("restore.pending.json"),
        br#"{"id":"../../escape"}"#,
    )
    .unwrap();
    assert!(f.manager.start(&f.id).await.is_err());
    assert!(f.manager.recover_restore(&f.id).is_err());
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
}
#[cfg(unix)]
#[test]
fn symlinks_in_world_and_backup_directory_are_rejected() {
    use std::os::unix::fs::symlink;
    let f = fixture();
    symlink("/etc/passwd", f.dir.join("world/link")).unwrap();
    assert!(f.manager.create_backup(&f.id).is_err());
    fs::remove_file(f.dir.join("world/link")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), f.root.path().join("backups")).unwrap();
    assert!(f.manager.create_backup(&f.id).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn backup_from_another_server_cannot_be_restored() {
    let a = fixture();
    let b = fixture();
    let backup = a.manager.create_backup(&a.id).unwrap();
    b.manager.list_backups(&b.id).unwrap();
    let file = format!("{}.zip", backup.id);
    fs::copy(
        a.root.path().join("backups").join(&a.id).join(&file),
        b.root.path().join("backups").join(&b.id).join(file),
    )
    .unwrap();
    assert!(b.manager.restore_backup(&b.id, &backup.id).is_err());
    assert_eq!(
        fs::read(b.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn dangling_restore_marker_blocks_start_and_backup() {
    use std::os::unix::fs::symlink;
    let f = fixture();
    symlink(
        "/nonexistent-blockops-test",
        f.dir.join("restore.pending.json"),
    )
    .unwrap();
    assert!(f.manager.start(&f.id).await.is_err());
    assert!(f.manager.create_backup(&f.id).is_err());
    assert!(f.manager.recover_restore(&f.id).is_err());
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
}

#[test]
fn backup_from_another_minecraft_version_is_not_restored() {
    let f = fixture();
    let backup = f.manager.create_backup(&f.id).unwrap();
    let path = f
        .root
        .path()
        .join("backups")
        .join(&f.id)
        .join(format!("{}.zip", backup.id));
    rewrite(&path, |name, data| {
        if name == "manifest.json" {
            let mut manifest: serde_json::Value = serde_json::from_slice(data).unwrap();
            manifest["minecraft_version"] = serde_json::json!("1.21.4");
            *data = serde_json::to_vec(&manifest).unwrap();
        }
    });
    let error = f.manager.restore_backup(&f.id, &backup.id).unwrap_err();
    assert!(error.to_string().contains("version"));
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
}

#[test]
fn newer_supported_version_backups_round_trip() {
    let f = fixture();
    let mut server = f.manager.get(&f.id).unwrap();
    server.version = "1.21.4".into();
    let db = rusqlite::Connection::open(f.root.path().join("blockops.sqlite3")).unwrap();
    db.execute(
        "UPDATE servers SET metadata=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_string(&server).unwrap(), f.id],
    )
    .unwrap();
    let backup = f.manager.create_backup(&f.id).unwrap();
    assert_eq!(backup.minecraft_version, "1.21.4");
    fs::write(f.dir.join("world/level.dat"), b"changed").unwrap();
    f.manager.restore_backup(&f.id, &backup.id).unwrap();
    assert_eq!(
        fs::read(f.dir.join("world/level.dat")).unwrap(),
        b"original level"
    );
}
