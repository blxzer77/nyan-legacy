//! Prints the catalog built from the cache in `<agent dir>` as JSON, one object
//! per provider mapping model id to `[api, base URL, context, max output,
//! reasoning, image input, input price, output price]`.
//!
//! `rust/tools/compare_catalog.py` compares this with the TypeScript model data.

use std::path::Path;

use nyan_coding_agent_models::cache;
use nyan_coding_agent_models::catalog::Catalog;
use serde_json::{Map, Value, json};

fn main() {
    let dir = std::env::args().nth(1).expect("usage: dump <agent dir>");
    let cached = cache::read(Path::new(&dir)).expect("no models.dev cache in that directory");
    let catalog = Catalog::from_models_dev(&cached.providers);
    let mut out = Map::new();
    for model in catalog.models() {
        let entry = out
            .entry(model.provider.clone())
            .or_insert_with(|| json!({}));
        entry[&model.id] = json!([
            model.api.name(),
            model.base_url,
            model.context_window,
            model.max_tokens,
            model.reasoning,
            model.image_input,
            model.cost.input,
            model.cost.output
        ]);
    }
    println!("{}", Value::Object(out));
}
