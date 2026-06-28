//! Generates the dataset for the "Rewind" browser demo: a sample package-registry
//! that grows month by month, baked into a single `.pvdb` file plus a tiny
//! `versions.json` that maps each month to the MVCC transaction id at its end.
//!
//! The browser loads `rewind.pvdb` with `Db.fromBytes(...)` and runs
//! `SELECT ... BEFORE <tx>` as a time slider moves, so scrubbing the slider shows
//! the registry as it was at any past month, all client-side from one static file.
//!
//! Run: `cargo run --example make_rewind_dataset [out_dir]`
//! (out_dir defaults to `../picovolt-rewind`, i.e. a sibling of the repo).
//!
//! The data is synthetic and deterministic (seeded), and is labelled as sample
//! data in the demo UI.

use std::collections::HashSet;
use std::path::PathBuf;

use picovolt::{Database, Value};

/// A tiny deterministic LCG so the dataset is identical on every run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            (self.next() >> 17) % n
        }
    }
}

const CATEGORIES: &[&str] = &[
    "cli", "web", "async", "parsing", "serde", "database", "game", "crypto", "math", "gui",
    "embedded", "testing",
];
const PREFIX: &[&str] = &[
    "fast", "micro", "tokio", "serde", "async", "tiny", "hyper", "open", "quick", "rust", "zero",
    "blaze", "nano", "crab", "swift",
];
const STEM: &[&str] = &[
    "log", "json", "http", "sql", "time", "net", "fs", "cli", "parse", "crypt", "sync", "vec",
    "map", "graph", "wasm", "render", "query", "cache", "queue", "stream",
];
const SUFFIX: &[&str] = &["", "-rs", "-core", "-lite", "x", "2", "-kit", "-util", "er"];

fn pick<'a>(rng: &mut Lcg, items: &[&'a str]) -> &'a str {
    items[rng.below(items.len() as u64) as usize]
}

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../picovolt-rewind".into())
        .into();
    std::fs::create_dir_all(&out).expect("create output dir");

    let mut db = Database::open_memory();
    db.query("CREATE TABLE crates (name, category, downloads, version)")
        .unwrap();

    let mut rng = Lcg(0xC0FF_EE12_3456_789A);
    let months = 96usize; // 2017-01 .. 2024-12
    let start_year = 2017;
    // Per-month volume (override with argv[2]). The default builds a multi-MB file
    // so the streaming reader has something real to stream.
    let per_month: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1400);
    let mut used: HashSet<String> = HashSet::new();
    let mut versions: Vec<(String, u64)> = Vec::with_capacity(months);

    for m in 0..months {
        let year = start_year + m / 12;
        let mon = (m % 12) + 1;
        let label = format!("{year}-{mon:02}");

        // The registry adds more crates per month as it grows.
        let count = per_month + (m as u64) * 12 + rng.below(per_month / 3 + 1);
        for _ in 0..count {
            let name = {
                let base = format!(
                    "{}{}{}",
                    pick(&mut rng, PREFIX),
                    pick(&mut rng, STEM),
                    pick(&mut rng, SUFFIX)
                );
                // Disambiguate collisions with a numeric suffix (like real crates:
                // `log`, `log2`, ...). This always terminates, unlike retrying.
                let mut n = base.clone();
                let mut k = 2u32;
                while used.contains(&n) {
                    n = format!("{base}{k}");
                    k += 1;
                }
                used.insert(n.clone());
                n
            };
            let category = pick(&mut rng, CATEGORIES).to_string();
            // Older crates have had longer to accumulate downloads; a rare few are
            // mega-hits, so "top by downloads" is dominated by long-lived crates.
            let age_years = (months - m) as u64 / 12;
            let base = (rng.below(5_000) + 50) * (1 + age_years);
            let hit = if rng.below(40) == 0 {
                rng.below(8_000_000) + 500_000
            } else {
                0
            };
            let downloads = (base + hit) as i64;
            let version = format!(
                "{}.{}.{}",
                if rng.below(3) == 0 { 1 } else { 0 },
                rng.below(12),
                rng.below(20)
            );
            db.query_with(
                "INSERT INTO crates VALUES (?, ?, ?, ?)",
                &[
                    Value::Text(name),
                    Value::Text(category),
                    Value::Int(downloads),
                    Value::Text(version),
                ],
            )
            .unwrap();
        }
        // The transaction id after this month's inserts is the time-travel bound
        // for "the registry as of <label>".
        versions.push((label, db.current_tx()));
    }

    db.bake(out.join("rewind.pvdb")).expect("bake");

    let mut json = String::from("[\n");
    for (i, (label, tx)) in versions.iter().enumerate() {
        json.push_str(&format!(
            "  {{\"i\":{i},\"label\":\"{label}\",\"tx\":{tx}}}"
        ));
        json.push_str(if i + 1 < versions.len() { ",\n" } else { "\n" });
    }
    json.push_str("]\n");
    std::fs::write(out.join("versions.json"), json).expect("write versions.json");

    let size = std::fs::metadata(out.join("rewind.pvdb")).unwrap().len();
    println!(
        "wrote {} crates across {} months -> {} ({} KB) + versions.json",
        used.len(),
        months,
        out.join("rewind.pvdb").display(),
        size / 1024
    );
}
