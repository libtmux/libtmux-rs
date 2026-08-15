use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "malformed_fields", fields = "not::an::identifier")]
struct MalformedFieldsIdentifier {
    name: String,
}

fn main() {}
