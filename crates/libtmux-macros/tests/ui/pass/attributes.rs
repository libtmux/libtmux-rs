#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::{Filterable as _, TextField};

#[derive(Filterable)]
#[filterable(target = "artifact", fields = "DocumentHandles")]
struct Document {
    #[filterable(rename = "kind")]
    r#type: String,
    r#match: String,
    #[filterable(rename = "title_2")]
    title: String,
    #[filterable(skip)]
    cached_width: usize,
}

#[derive(Filterable)]
#[filterable(target = "empty_document")]
struct EmptyDocument {
    #[filterable(skip)]
    opaque: Vec<u8>,
}

fn main() {
    let document = Document {
        r#type: String::from("guide"),
        r#match: String::from("exact"),
        title: String::from("querying"),
        cached_width: 80,
    };
    let fields = Document::filter_fields();
    let _: TextField<Document> = fields.r#type;
    assert!(fields.r#type.eq("guide").matches(&document));
    assert!(fields.r#match.eq("exact").matches(&document));
    let _ = fields.title.eq("querying");
    let renamed_debug = format!("{:?}", fields.r#type);
    assert!(renamed_debug.contains("artifact"));
    assert!(renamed_debug.contains("kind"));
    assert!(!renamed_debug.contains("r#type"));
    let default_debug = format!("{:?}", fields.r#match);
    assert!(default_debug.contains("match"));
    assert!(!default_debug.contains("r#match"));
    assert!(format!("{:?}", fields.title).contains("title_2"));

    let empty = EmptyDocument::filter_fields();
    let copied = empty;
    assert_eq!(empty, copied);
    let _ = format!("{empty:?}");
}
