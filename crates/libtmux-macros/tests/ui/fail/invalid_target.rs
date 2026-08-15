use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "")]
struct EmptyTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "Uppercase")]
struct UppercaseTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "lowerUpper")]
struct LaterUppercaseTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "lower-hyphen")]
struct HyphenTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "lower space")]
struct SpaceTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "loweré")]
struct NonAsciiTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "1leading_digit")]
struct DigitTarget {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "_leading_underscore")]
struct UnderscoreTarget {
    name: String,
}

fn main() {}
