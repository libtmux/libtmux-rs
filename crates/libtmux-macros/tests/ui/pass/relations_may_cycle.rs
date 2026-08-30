#![allow(dead_code)]

use libtmux_macros::Filterable;

// A tree. The related type is already known here, so the impl must not ask
// for what it is itself proving.
#[derive(Filterable)]
#[filterable(target = "category")]
struct Category {
    name: String,
    #[filterable(many)]
    subcategories: Vec<Category>,
}

// The same cycle across two types, which is how it usually arrives.
#[derive(Filterable)]
#[filterable(target = "author")]
struct Author {
    name: String,
    #[filterable(many)]
    books: Vec<Book>,
}

#[derive(Filterable)]
#[filterable(target = "book")]
struct Book {
    title: String,
    #[filterable(one)]
    author: Option<Author>,
}

fn main() {
    let leaf = Category {
        name: "leaf".to_owned(),
        subcategories: Vec::new(),
    };
    let root = Category {
        name: "root".to_owned(),
        subcategories: vec![leaf],
    };
    assert_eq!(root.subcategories.len(), 1);

    let book = Book {
        title: "one".to_owned(),
        author: Some(Author {
            name: "writer".to_owned(),
            books: Vec::new(),
        }),
    };
    assert!(book.author.is_some());
}
