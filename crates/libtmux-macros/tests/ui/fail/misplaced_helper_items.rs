use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(
    target = "misplaced_items",
    rename = "container_name",
    skip,
    enum,
    many,
    one
)]
struct MisplacedHelperItems {
    #[filterable(fields = "FieldHandles")]
    name: String,
    #[filterable(target = "field_target")]
    label: String,
    #[filterable(crate = "renamed_libtmux")]
    context: String,
}

fn main() {}
