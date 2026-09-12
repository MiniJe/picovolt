#![cfg(feature = "full-text")]
use picovolt::search::{SearchError, SearchIndex};
use picovolt::{Database, Value};

#[test]
fn ranking_unicode_updates_and_bounds() {
    let mut index = SearchIndex::new();
    index.upsert(2, "SQL SQL database transactions").unwrap();
    index
        .upsert(
            1,
            "SQL database transactions and a much longer description of the application",
        )
        .unwrap();
    index.upsert(3, "Bună ziua, ROMÂNIA!").unwrap();
    let hits = index.search("sql database", 10).unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![2, 1]);
    assert!(hits.iter().all(|h| h.score.is_finite() && h.score > 0.0));
    assert_eq!(index.search("românia", 1).unwrap()[0].id, 3);
    assert_eq!(index.search("SQL sql", 10), index.search("sql", 10));
    assert!(index.search("sql absent", 10).unwrap().is_empty());
    assert_eq!(index.upsert(2, &"x".repeat(65537)), Err(SearchError::Limit));
    assert_eq!(index.search("sql database", 1).unwrap()[0].id, 2);
    index.upsert(2, "new topic").unwrap();
    assert_eq!(index.search("sql", 10).unwrap().len(), 1);
    assert!(index.remove(1));
    assert!(!index.remove(1));
    assert!(index.search("sql", 10).unwrap().is_empty());
    assert_eq!(index.search("sql", 101), Err(SearchError::Limit));
    assert!(index.search("---", 10).unwrap().is_empty());
}

#[test]
fn database_snapshot_build_and_reload() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = Database::open_dev(temp.path()).unwrap();
    db.query("CREATE TABLE articles (id, title, body)").unwrap();
    for (id, title, body) in [
        (1, "Transactions", "Atomic batch writes"),
        (2, "Recovery", "Restore database backups"),
    ] {
        db.query_with(
            "INSERT INTO articles VALUES (?, ?, ?)",
            &[
                Value::Int(id),
                Value::Text(title.into()),
                Value::Text(body.into()),
            ],
        )
        .unwrap();
    }
    let rows = db.query("SELECT id,title,body FROM articles").unwrap();
    let before = SearchIndex::from_rows(&rows, "id", &["title", "body"]).unwrap();
    assert_eq!(before.search("restore backups", 10).unwrap()[0].id, 2);
    db.query("DELETE FROM articles WHERE id=2").unwrap();
    drop(db);
    let mut db = Database::open_dev(temp.path()).unwrap();
    let after = SearchIndex::from_rows(
        &db.query("SELECT id,title,body FROM articles").unwrap(),
        "id",
        &["title", "body"],
    )
    .unwrap();
    assert!(after.search("restore", 10).unwrap().is_empty());
    assert_eq!(before.search("restore", 10).unwrap().len(), 1);
}

#[test]
fn ties_are_stable_and_bad_rows_fail() {
    let mut index = SearchIndex::new();
    for id in [8, 3, 5] {
        index.upsert(id, "identical text").unwrap();
    }
    assert_eq!(
        index
            .search("text", 10)
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect::<Vec<_>>(),
        vec![3, 5, 8]
    );
    let mut db = Database::open_memory();
    db.query("CREATE TABLE duplicate (id, body)").unwrap();
    db.query("INSERT INTO duplicate VALUES (1, 'a'), (1, 'b')")
        .unwrap();
    assert!(matches!(
        SearchIndex::from_rows(
            &db.query("SELECT id,body FROM duplicate").unwrap(),
            "id",
            &["body"]
        ),
        Err(SearchError::InvalidRows)
    ));
}
