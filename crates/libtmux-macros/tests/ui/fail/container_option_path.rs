use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "container_path", serde::rename = "other")]
struct ContainerOptionPath {
    name: String,
}

fn main() {}
