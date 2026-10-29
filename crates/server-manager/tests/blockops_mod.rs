use blockops_server_manager::blockops_mod::{install, remove, status, BundledMod, InstallError};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zip::{write::SimpleFileOptions, ZipWriter};

fn jar(path: &Path, id: &str, version: &str) {
    let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
    zip.start_file("fabric.mod.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(
        serde_json::to_vec(&json!({"schemaVersion":1,"id":id,"version":version,"depends":{}}))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    zip.finish().unwrap();
}

fn version(root: &Path, id: &str, json: serde_json::Value) {
    let dir = root.join("versions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{id}.json")), json.to_string()).unwrap();
}

/// A launcher folder with Vanilla 1.21.1/1.20.4 and Fabric builds of both.
fn launcher(profiles: serde_json::Value) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("launcher_profiles.json"),
        json!({ "profiles": profiles }).to_string(),
    )
    .unwrap();
    for release in ["1.21.1", "1.20.4"] {
        version(root, release, json!({"id": release, "libraries": []}));
        let fabric = format!("fabric-loader-0.16.10-{release}");
        version(
            root,
            &fabric,
            json!({"id": fabric, "inheritsFrom": release,
                   "libraries": [{"name": "net.fabricmc:fabric-loader:0.16.10"}]}),
        );
    }
    temp
}

fn bundled(dir: &Path, version: &str) -> BundledMod {
    let path = dir.join(format!("bundled-{version}.jar"));
    jar(&path, "blockops", version);
    BundledMod::load(&path).unwrap()
}

fn names(mods: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(mods)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn installs_updates_and_removes_only_the_blockops_mod() {
    let root = launcher(json!({
        "fabric": {"name": "Fabric", "lastVersionId": "fabric-loader-0.16.10-1.21.1", "gameDir": "instances/fabric"},
        "latest": {"name": "Latest release", "lastVersionId": "latest-release", "gameDir": "instances/fabric"}
    }));
    let game: PathBuf = root.path().join("instances/fabric");
    let mods = game.join("mods");
    fs::create_dir_all(&mods).unwrap();
    jar(&mods.join("sodium.jar"), "sodium", "0.6.0");
    jar(&mods.join("renamed.jar"), "blockops", "0.0.9");
    fs::write(mods.join("notes.txt"), "keep").unwrap();
    let source = tempfile::tempdir().unwrap();
    let mod_file = bundled(source.path(), "0.1.0");

    let before = status(&game, Some(&mod_file)).unwrap();
    assert_eq!(before.installed, ["0.0.9"]);
    assert!(!before.current);

    let after = install(root.path(), &game, &mod_file).unwrap();
    assert_eq!(after.bundled.as_deref(), Some("0.1.0"));
    assert_eq!(after.installed, ["0.1.0"]);
    assert!(after.current);
    assert_eq!(
        names(&mods),
        [
            "blockops-fabric-client-0.1.0.jar",
            "notes.txt",
            "sodium.jar"
        ],
        "other files changed or staging left behind"
    );
    // Installing again changes nothing.
    assert!(install(root.path(), &game, &mod_file).unwrap().current);

    let newer = bundled(source.path(), "0.2.0");
    assert!(!status(&game, Some(&newer)).unwrap().current);
    assert!(install(root.path(), &game, &newer).unwrap().current);
    assert_eq!(
        names(&mods),
        [
            "blockops-fabric-client-0.2.0.jar",
            "notes.txt",
            "sodium.jar"
        ]
    );

    let removed = remove(&game, Some(&newer)).unwrap();
    assert!(removed.installed.is_empty() && !removed.current);
    assert_eq!(names(&mods), ["notes.txt", "sodium.jar"]);
}

#[test]
fn creates_the_mods_folder_and_reports_a_build_without_the_mod() {
    let root = launcher(json!({
        "fabric": {"lastVersionId": "fabric-loader-0.16.10-1.21.1"}
    }));
    let game = root.path().canonicalize().unwrap();
    let missing = status(&game, None).unwrap();
    assert_eq!(missing.bundled, None);
    assert!(missing.installed.is_empty() && !missing.current);
    let source = tempfile::tempdir().unwrap();
    assert!(
        install(root.path(), &game, &bundled(source.path(), "0.1.0"))
            .unwrap()
            .current
    );
    assert!(game.join("mods/blockops-fabric-client-0.1.0.jar").is_file());
}

#[test]
fn refuses_folders_where_the_mod_could_break_or_never_load() {
    let source = tempfile::tempdir().unwrap();
    let mod_file = bundled(source.path(), "0.1.0");
    // A Fabric 1.20.4 profile on the same folder would no longer start.
    let shared = launcher(json!({
        "new": {"name": "Fabric 1.21.1", "lastVersionId": "fabric-loader-0.16.10-1.21.1"},
        "old": {"name": "Старый Fabric", "lastVersionId": "fabric-loader-0.16.10-1.20.4"}
    }));
    match install(shared.path(), shared.path(), &mod_file) {
        Err(InstallError::SharedWith(name)) => assert_eq!(name, "Старый Fabric"),
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(
        !shared.path().join("mods").exists(),
        "folder changed on refusal"
    );
    // Unrecognised versions cannot be vouched for either.
    let unknown = launcher(json!({
        "new": {"lastVersionId": "fabric-loader-0.16.10-1.21.1"},
        "lost": {"name": "Потерянная", "lastVersionId": "deleted-build"}
    }));
    assert!(matches!(
        install(unknown.path(), unknown.path(), &mod_file),
        Err(InstallError::SharedWith(name)) if name == "Потерянная"
    ));
    // Vanilla only, or Fabric for another version: the mod would never load.
    for last in ["1.21.1", "fabric-loader-0.16.10-1.20.4"] {
        let other = launcher(json!({"only": {"lastVersionId": last}}));
        assert!(matches!(
            install(other.path(), other.path(), &mod_file),
            Err(InstallError::NotFabric)
        ));
    }
    // A Fabric profile with its own folder does not make a different folder eligible.
    let separate = launcher(json!({
        "fabric": {"lastVersionId": "fabric-loader-0.16.10-1.21.1", "gameDir": "instances/fabric"}
    }));
    assert!(matches!(
        install(separate.path(), separate.path(), &mod_file),
        Err(InstallError::NotFabric)
    ));
}

#[test]
fn only_the_blockops_mod_is_accepted_as_bundled() {
    let source = tempfile::tempdir().unwrap();
    let other = source.path().join("other.jar");
    jar(&other, "sodium", "0.6.0");
    assert!(BundledMod::load(&other).is_err());
    let unsafe_version = source.path().join("version.jar");
    jar(&unsafe_version, "blockops", "../../evil");
    assert!(BundledMod::load(&unsafe_version).is_err());
    fs::write(source.path().join("plain.jar"), b"not a zip").unwrap();
    assert!(BundledMod::load(&source.path().join("plain.jar")).is_err());
}
