use syn::{Ident, LitStr, Path, Type};

#[derive(Default)]
pub(super) struct Errors {
    error: Option<syn::Error>,
}

impl Errors {
    pub(super) fn push(&mut self, error: syn::Error) {
        if let Some(existing) = &mut self.error {
            existing.combine(error);
        } else {
            self.error = Some(error);
        }
    }

    pub(super) fn finish(self) -> syn::Result<()> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
pub(super) struct ContainerOptions {
    pub(super) target: Option<LitStr>,
    pub(super) target_seen: bool,
    pub(super) fields: Option<Ident>,
    pub(super) fields_seen: bool,
    pub(super) crate_path: Option<Path>,
    pub(super) crate_seen: bool,
}

pub(super) enum FieldKind {
    Text {
        optional: bool,
    },
    Bool {
        optional: bool,
    },
    SignedInteger {
        ty: Type,
        optional: bool,
        kind: Ident,
    },
    UnsignedInteger {
        ty: Type,
        optional: bool,
        kind: Ident,
    },
    Enum {
        ty: Type,
        optional: bool,
    },
    Many {
        related: Type,
    },
    One {
        related: Type,
    },
}

pub(super) struct FieldSpec {
    pub(super) ident: Ident,
    pub(super) wire_name: LitStr,
    pub(super) kind: FieldKind,
}
