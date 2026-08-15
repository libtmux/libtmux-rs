use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "invalid_rename")]
struct InvalidRename {
    #[filterable(rename = "")]
    empty: String,
    #[filterable(rename = "Uppercase")]
    uppercase: String,
    #[filterable(rename = "lowerUpper")]
    later_uppercase: String,
    #[filterable(rename = "lower-hyphen")]
    hyphen: String,
    #[filterable(rename = "lower space")]
    space: String,
    #[filterable(rename = "loweré")]
    non_ascii: String,
    #[filterable(rename = "1leading_digit")]
    digit: String,
    #[filterable(rename = "_leading_underscore")]
    underscore: String,
}

fn main() {}
