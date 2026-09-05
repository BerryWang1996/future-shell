mod common;
use fs_connmgr::{Db, SettingsRepo};

#[tokio::test]
async fn get_missing_returns_none() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = SettingsRepo::new(db.pool());
    assert_eq!(repo.get("theme").await.unwrap(), None);
}

#[tokio::test]
async fn set_then_overwrite_roundtrip() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = SettingsRepo::new(db.pool());
    repo.set("theme", "dark").await.unwrap();
    assert_eq!(repo.get("theme").await.unwrap().as_deref(), Some("dark"));
    repo.set("theme", "light").await.unwrap();
    assert_eq!(repo.get("theme").await.unwrap().as_deref(), Some("light"));
}

#[tokio::test]
async fn list_orders_by_key() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = SettingsRepo::new(db.pool());
    repo.set("b", "2").await.unwrap();
    repo.set("a", "1").await.unwrap();
    assert_eq!(
        repo.list().await.unwrap(),
        vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string())
        ]
    );
}

fn tmp() -> std::path::PathBuf {
    common::tmpdir("settings")
}
