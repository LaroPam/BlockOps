use blockops_server_manager::{
    minecraft_selection::{MinecraftSelection, ReportedLoader},
    Manager,
};
use std::fs;

#[test]
fn user_choice_survives_reopening_without_touching_the_game_folder() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("моя сборка");
    fs::create_dir_all(game.join("mods")).unwrap();
    fs::write(game.join("mods/existing.jar"), b"existing").unwrap();
    let root = temp.path().join("blockops");
    let selection = MinecraftSelection {
        game_dir: game.to_string_lossy().into_owned(),
        loader: ReportedLoader::Forge,
    };
    let manager = Manager::open(&root).unwrap();
    assert!(manager.minecraft_selection().unwrap().is_none());
    assert_eq!(
        manager.save_minecraft_selection(selection.clone()).unwrap(),
        selection
    );
    let changed = MinecraftSelection {
        game_dir: selection.game_dir.clone(),
        loader: ReportedLoader::Neoforge,
    };
    manager.save_minecraft_selection(changed.clone()).unwrap();
    drop(manager);
    assert_eq!(
        Manager::open(&root).unwrap().minecraft_selection().unwrap(),
        Some(changed)
    );
    assert_eq!(
        fs::read(game.join("mods/existing.jar")).unwrap(),
        b"existing"
    );
    assert_eq!(fs::read_dir(game.join("mods")).unwrap().count(), 1);
}

#[test]
fn invalid_path_does_not_replace_existing_choice() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    fs::create_dir(&game).unwrap();
    let manager = Manager::open(temp.path().join("app")).unwrap();
    let valid = MinecraftSelection {
        game_dir: game.to_string_lossy().into_owned(),
        loader: ReportedLoader::Unknown,
    };
    manager.save_minecraft_selection(valid.clone()).unwrap();
    for path in [
        "relative/folder".to_string(),
        temp.path().join("missing").to_string_lossy().into_owned(),
    ] {
        assert!(manager
            .save_minecraft_selection(MinecraftSelection {
                game_dir: path,
                loader: ReportedLoader::None
            })
            .is_err());
        assert_eq!(manager.minecraft_selection().unwrap(), Some(valid.clone()));
    }
    assert!(serde_json::from_str::<MinecraftSelection>(
        r#"{"game_dir":"x","loader":"unexpected"}"#
    )
    .is_err());
}
