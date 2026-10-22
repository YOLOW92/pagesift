//! Small stdin protocol for the locked comparison harness, not a crawler CLI.
use pagesift_store::{Config, Store, SyncPolicy};
use std::io::{self, BufRead, Write};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new(std::env::args().nth(1).ok_or("missing store directory")?);
    config.sync = SyncPolicy::Always;
    config.background = false;
    let store = Store::open(config)?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 4 {
            return Err("expected action, source, item, revision".into());
        }
        let items = store.items();
        let (source, item, revision) = (
            fields[1].as_bytes(),
            fields[2].as_bytes(),
            fields[3].as_bytes(),
        );
        let answer = match fields[0] {
            "check" => items.matches(source, item, revision)?,
            "commit" => {
                items.commit(source, item, revision)?;
                true
            }
            "forget" => items.forget(source, item)?,
            _ => return Err("unknown action".into()),
        };
        println!("{answer}");
        io::stdout().flush()?;
    }
    Ok(())
}
