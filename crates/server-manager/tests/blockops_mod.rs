use blockops_server_manager::blockops_mod::{
    install, remove, status, Catalog, InstallError, Target,
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zip::{write::SimpleFileOptions, ZipWriter};

fn jar(path: &Path, files: &[(&str, serde_json::Value)]) {
    let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
    for (name, content) in files {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(content.to_string().as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

/// A BlockOps build for one loader and version, as the Gradle builds produce it.
fn ours(path: &Path, loader: &str, minecraft: &str, version: &str) {
    let mut files = vec![(
        "blockops.json",
        json!({"id": "blockops", "loader": loader, "minecraft": minecraft, "version": version}),
    )];
    if loader == "fabric" {
        files.push((
            "fabric.mod.json",
            json!({"id": "blockops", "version": version}),
        ));
    }
    jar(path, &files);
}

/// A Fabric jar of another mod, or one of BlockOps from before `blockops.json` existed.
fn fabric_mod(path: &Path, id: &str, version: &str) {
    jar(
        path,
        &[("fabric.mod.json", json!({"id": id, "version": version}))],
    );
}

fn catalog(dir: &Path, builds: &[(&str, &str, &str)]) -> Catalog {
    fs::create_dir_all(dir).unwrap();
    for (loader, minecraft, version) in builds {
        ours(
            &dir.join(format!("{loader}-{minecraft}.jar")),
            loader,
            minecraft,
            version,
        );
    }
    Catalog::load(dir)
}

fn version(root: &Path, id: &str, json: serde_json::Value) {
    let dir = root.join("versions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{id}.json")), json.to_string()).unwrap();
}

/// A launcher folder with Vanilla releases and Fabric, Forge and NeoForge builds of them.
fn launcher(profiles: serde_json::Value) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("launcher_profiles.json"),
        json!({ "profiles": profiles }).to_string(),
    )
    .unwrap();
    for release in ["1.21.1", "1.20.4", "1.20.1"] {
        version(root, release, json!({"id": release, "libraries": []}));
        let fabric = format!("fabric-{release}");
        version(
            root,
            &fabric,
            json!({"id": fabric, "inheritsFrom": release,
                   "libraries": [{"name": "net.fabricmc:fabric-loader:0.16.10"}]}),
        );
    }
    version(
        root,
        "forge-1.20.1",
        json!({"id": "forge-1.20.1", "inheritsFrom": "1.20.1",
               "libraries": [{"name": "net.minecraftforge:forge:1.20.1-47.4.28"}]}),
    );
    version(
        root,
        "neoforge-1.21.1",
        json!({"id": "neoforge-1.21.1", "inheritsFrom": "1.21.1",
               "libraries": [{"name": "net.neoforged:neoforge:21.1.265"}]}),
    );
    temp
}

fn names(mods: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(mods)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn target(loader: &str, minecraft: &str) -> Target {
    Target {
        loader: loader.into(),
        minecraft: minecraft.into(),
    }
}

#[test]
fn installs_updates_and_removes_only_the_blockops_mod() {
    let root = launcher(json!({
        "fabric": {"name": "Fabric", "lastVersionId": "fabric-1.21.1", "gameDir": "instances/fabric"},
        "latest": {"name": "Latest release", "lastVersionId": "latest-release", "gameDir": "instances/fabric"}
    }));
    let game: PathBuf = root.path().join("instances/fabric");
    let mods = game.join("mods");
    fs::create_dir_all(&mods).unwrap();
    fabric_mod(&mods.join("sodium.jar"), "sodium", "0.6.0");
    // Installed by an earlier BlockOps, before jars named their target.
    fabric_mod(&mods.join("renamed.jar"), "blockops", "0.0.9");
    fs::write(mods.join("notes.txt"), "keep").unwrap();
    let source = tempfile::tempdir().unwrap();
    let first = catalog(&source.path().join("1"), &[("fabric", "1.21.1", "0.1.0")]);
    let bundled = first.get(&target("fabric", "1.21.1"));

    let before = status(&game, bundled).unwrap();
    assert_eq!(before.installed, ["0.0.9"]);
    assert!(!before.current);

    let after = install(root.path(), &game, &first).unwrap();
    assert_eq!(after.bundled.as_deref(), Some("0.1.0"));
    assert_eq!(after.installed, ["0.1.0"]);
    assert!(after.current);
    assert_eq!(
        names(&mods),
        [
            "blockops-fabric-1.21.1-0.1.0.jar",
            "notes.txt",
            "sodium.jar"
        ],
        "other files changed or staging left behind"
    );
    // Installing again changes nothing.
    assert!(install(root.path(), &game, &first).unwrap().current);

    let newer = catalog(&source.path().join("2"), &[("fabric", "1.21.1", "0.2.0")]);
    assert!(
        !status(&game, newer.get(&target("fabric", "1.21.1")))
            .unwrap()
            .current
    );
    assert!(install(root.path(), &game, &newer).unwrap().current);
    assert_eq!(
        names(&mods),
        [
            "blockops-fabric-1.21.1-0.2.0.jar",
            "notes.txt",
            "sodium.jar"
        ]
    );

    let removed = remove(&game, newer.get(&target("fabric", "1.21.1"))).unwrap();
    assert!(removed.installed.is_empty() && !removed.current);
    assert_eq!(names(&mods), ["notes.txt", "sodium.jar"]);
}

#[test]
fn each_build_gets_the_jar_made_for_it() {
    let root = launcher(json!({
        "f121": {"lastVersionId": "fabric-1.21.1", "gameDir": "f121"},
        "f120": {"lastVersionId": "fabric-1.20.1", "gameDir": "f120"},
        "forge": {"lastVersionId": "forge-1.20.1", "gameDir": "forge"},
        "neo": {"lastVersionId": "neoforge-1.21.1", "gameDir": "neo"}
    }));
    let source = tempfile::tempdir().unwrap();
    let all = catalog(
        source.path(),
        &[
            ("fabric", "1.21.1", "0.1.0"),
            ("fabric", "1.20.1", "0.1.0"),
            ("forge", "1.20.1", "0.1.0"),
            ("neoforge", "1.21.1", "0.1.0"),
        ],
    );
    assert_eq!(all.targets().len(), 4);
    for (folder, expected) in [
        ("f121", "blockops-fabric-1.21.1-0.1.0.jar"),
        ("f120", "blockops-fabric-1.20.1-0.1.0.jar"),
        ("forge", "blockops-forge-1.20.1-0.1.0.jar"),
        ("neo", "blockops-neoforge-1.21.1-0.1.0.jar"),
    ] {
        let game = root.path().join(folder);
        fs::create_dir_all(&game).unwrap();
        assert!(
            install(root.path(), &game, &all).unwrap().current,
            "{folder}"
        );
        assert_eq!(names(&game.join("mods")), [expected]);
    }
}

#[test]
fn creates_the_mods_folder_and_reports_a_build_without_the_mod() {
    let root = launcher(json!({
        "fabric": {"lastVersionId": "fabric-1.21.1"}
    }));
    let game = root.path().canonicalize().unwrap();
    let missing = status(&game, None).unwrap();
    assert_eq!(missing.bundled, None);
    assert!(missing.installed.is_empty() && !missing.current);
    let source = tempfile::tempdir().unwrap();
    let empty = Catalog::load(source.path());
    assert!(matches!(
        install(root.path(), &game, &empty),
        Err(InstallError::Unsupported)
    ));
    let carried = catalog(source.path(), &[("fabric", "1.21.1", "0.1.0")]);
    assert!(install(root.path(), &game, &carried).unwrap().current);
    assert!(game.join("mods/blockops-fabric-1.21.1-0.1.0.jar").is_file());
}

#[test]
fn refuses_folders_where_the_mod_could_break_or_never_load() {
    let source = tempfile::tempdir().unwrap();
    let carried = catalog(
        source.path(),
        &[("fabric", "1.21.1", "0.1.0"), ("forge", "1.20.1", "0.1.0")],
    );
    // Another version, or another loader, on the same folder would no longer start.
    for (other, name) in [
        ("fabric-1.20.4", "Старый Fabric"),
        ("forge-1.20.1", "Forge"),
    ] {
        let shared = launcher(json!({
            "new": {"name": "Fabric 1.21.1", "lastVersionId": "fabric-1.21.1"},
            "old": {"name": name, "lastVersionId": other}
        }));
        match install(shared.path(), shared.path(), &carried) {
            Err(InstallError::SharedWith(found)) => assert_eq!(found, name),
            result => panic!("unexpected result: {result:?}"),
        }
        assert!(
            !shared.path().join("mods").exists(),
            "folder changed on refusal"
        );
    }
    // Unrecognised versions cannot be vouched for either.
    let unknown = launcher(json!({
        "new": {"lastVersionId": "fabric-1.21.1"},
        "lost": {"name": "Потерянная", "lastVersionId": "deleted-build"}
    }));
    assert!(matches!(
        install(unknown.path(), unknown.path(), &carried),
        Err(InstallError::SharedWith(name)) if name == "Потерянная"
    ));
    // Vanilla only, or a build no jar is made for: the mod would never load.
    for last in ["1.21.1", "fabric-1.20.4", "neoforge-1.21.1"] {
        let other = launcher(json!({"only": {"lastVersionId": last}}));
        assert!(matches!(
            install(other.path(), other.path(), &carried),
            Err(InstallError::Unsupported)
        ));
    }
    // A supported profile with its own folder does not make a different folder eligible.
    let separate = launcher(json!({
        "fabric": {"lastVersionId": "fabric-1.21.1", "gameDir": "instances/fabric"}
    }));
    assert!(matches!(
        install(separate.path(), separate.path(), &carried),
        Err(InstallError::Unsupported)
    ));
}

#[test]
fn never_overwrites_a_foreign_file_under_the_mods_name() {
    let root = launcher(json!({
        "fabric": {"lastVersionId": "fabric-1.21.1"}
    }));
    let mods = root.path().join("mods");
    fs::create_dir_all(&mods).unwrap();
    let name = "blockops-fabric-1.21.1-0.1.0.jar";
    // Another mod renamed to the BlockOps file name, an unreadable file, and an old copy.
    fabric_mod(&mods.join(name), "other", "1.0.0");
    fabric_mod(&mods.join("old-blockops.jar"), "blockops", "0.0.9");
    let source = tempfile::tempdir().unwrap();
    let carried = catalog(source.path(), &[("fabric", "1.21.1", "0.1.0")]);
    for _ in 0..2 {
        let foreign = fs::read(mods.join(name)).unwrap();
        assert!(matches!(
            install(root.path(), root.path(), &carried),
            Err(InstallError::NameTaken(taken)) if taken == name
        ));
        assert_eq!(fs::read(mods.join(name)).unwrap(), foreign);
        assert_eq!(
            names(&mods),
            [name, "old-blockops.jar"],
            "folder changed on refusal"
        );
        fs::write(mods.join(name), b"not a zip").unwrap();
    }
}

#[test]
fn only_jars_naming_a_known_target_are_carried() {
    let source = tempfile::tempdir().unwrap();
    let dir = source.path();
    ours(&dir.join("good.jar"), "neoforge", "1.21.1", "0.1.0");
    ours(&dir.join("same-target.jar"), "neoforge", "1.21.1", "0.1.0");
    ours(&dir.join("quilt.jar"), "quilt", "1.21.1", "0.1.0");
    ours(&dir.join("unsafe.jar"), "fabric", "../../evil", "0.1.0");
    // Without blockops.json a jar does not say which build it is for.
    fabric_mod(&dir.join("legacy.jar"), "blockops", "0.0.9");
    fabric_mod(&dir.join("other.jar"), "sodium", "0.6.0");
    fs::write(dir.join("plain.jar"), b"not a zip").unwrap();
    fs::write(dir.join("README.md"), "notes").unwrap();
    let loaded = Catalog::load(dir);
    assert_eq!(loaded.targets(), [target("neoforge", "1.21.1")]);
}
