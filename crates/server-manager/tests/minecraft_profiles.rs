use blockops_server_manager::minecraft_profiles::discover;
use serde_json::json;
use std::{fs, path::Path};

fn version(root: &Path, id: &str, json: serde_json::Value) {
    let dir = root.join("versions").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_vec(&json).unwrap(),
    )
    .unwrap();
}

#[test]
fn discovers_loader_by_metadata_not_profile_name_and_preserves_game_folder() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("launcher_profiles.json"), serde_json::to_vec(&json!({"profiles":{
        "profile1":{"name":"Моя сборка","lastVersionId":"renamed-build", "gameDir":"instances/custom"},
        "profile2":{"name":"Fabric в названии", "lastVersionId":"1.21.1"},
        "profile3":{"name":"Потерянная", "lastVersionId":"missing"}
    }, "authenticationDatabase":{"secret":"must never be returned"}})).unwrap()).unwrap();
    version(root, "1.21.1", json!({"id":"1.21.1", "libraries":[]}));
    version(
        root,
        "renamed-build",
        json!({"id":"renamed-build", "inheritsFrom":"1.21.1", "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.9"}]}),
    );
    let found = discover(root).unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].name, "Моя сборка");
    assert_eq!(found[0].loader, "fabric");
    assert_eq!(found[0].minecraft_version.as_deref(), Some("1.21.1"));
    assert!(
        found[0].game_dir.ends_with("instances/custom")
            || found[0].game_dir.ends_with("instances\\custom")
    );
    assert!(found[0].metadata_complete);
    assert_eq!(found[1].loader, "vanilla");
    assert_eq!(found[2].loader, "unknown");
    assert!(!found[2].metadata_complete);
    assert!(!serde_json::to_string(&found)
        .unwrap()
        .contains("must never be returned"));
}

#[test]
fn detects_neo_and_forge_and_does_not_guess_custom_or_conflicting_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("launcher_profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
            "a":{"lastVersionId":"forge-x"}, "b":{"lastVersionId":"neo-x"},
            "c":{"lastVersionId":"custom"}, "d":{"lastVersionId":"mixed"}
        }}))
        .unwrap(),
    )
    .unwrap();
    version(root, "1.21.1", json!({"id":"1.21.1"}));
    version(
        root,
        "forge-x",
        json!({"id":"forge-x", "inheritsFrom":"1.21.1", "libraries":[{"name":"net.minecraftforge:forge:1"}]}),
    );
    version(
        root,
        "neo-x",
        json!({"id":"neo-x", "inheritsFrom":"1.21.1", "libraries":[{"name":"net.neoforged:neoforge:1"}]}),
    );
    version(
        root,
        "custom",
        json!({"id":"custom", "inheritsFrom":"1.21.1", "libraries":[{"name":"org.example:other-loader:1"}]}),
    );
    version(
        root,
        "mixed",
        json!({"id":"mixed", "inheritsFrom":"1.21.1", "libraries":[{"name":"net.fabricmc:fabric-loader:1"}, {"name":"net.minecraftforge:forge:1"}]}),
    );
    let found = discover(root).unwrap();
    assert_eq!(
        found.iter().map(|p| p.loader).collect::<Vec<_>>(),
        ["forge", "neoforge", "unknown", "ambiguous"]
    );
}

#[test]
fn incomplete_cyclic_and_unsafe_versions_are_not_interpreted_as_vanilla() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("launcher_profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
            "a":{"lastVersionId":"cycle"}, "b":{"lastVersionId":"../../secret"},
            "c":{"lastVersionId":"badid"}, "d":{"lastVersionId":"partial"}
        }}))
        .unwrap(),
    )
    .unwrap();
    version(root, "cycle", json!({"id":"cycle", "inheritsFrom":"cycle"}));
    version(root, "badid", json!({"id":"other"}));
    version(
        root,
        "partial",
        json!({"id":"partial", "inheritsFrom":"missing", "libraries":[{"name":"net.fabricmc:fabric-loader:1"}]}),
    );
    let found = discover(root).unwrap();
    assert!(found.iter().all(|p| !p.metadata_complete));
    assert_eq!(
        found.iter().map(|p| p.loader).collect::<Vec<_>>(),
        ["unknown", "unknown", "unknown", "fabric"]
    );
}
