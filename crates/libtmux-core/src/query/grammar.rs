//! Portable filter wire vocabulary shared by decoding and presentation.

#[cfg(feature = "serde")]
pub(super) const VERSION: u8 = 1;
#[cfg(feature = "serde")]
pub(super) const MAX_EXPRESSION_DEPTH: usize = 64;
#[cfg(feature = "serde")]
pub(super) const MAX_EXPRESSION_NODES: usize = 4_096;
#[cfg(feature = "serde")]
pub(super) const MAX_SET_VALUES: usize = 4_096;

#[derive(Clone, Copy, Eq, PartialEq)]
#[cfg_attr(
    not(feature = "serde"),
    allow(
        dead_code,
        reason = "ordering reaches text only from the wire, which serde gates"
    )
)]
pub(super) enum TextOperator {
    Eq,
    EqIgnoreCase,
    Contains,
    ContainsIgnoreCase,
    StartsWith,
    StartsWithIgnoreCase,
    EndsWith,
    EndsWithIgnoreCase,
    In,
    NotIn,
    Regex,
    RegexIgnoreCase,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl TextOperator {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::EqIgnoreCase => "eq_ignore_case",
            Self::Contains => "contains",
            Self::ContainsIgnoreCase => "contains_ignore_case",
            Self::StartsWith => "starts_with",
            Self::StartsWithIgnoreCase => "starts_with_ignore_case",
            Self::EndsWith => "ends_with",
            Self::EndsWithIgnoreCase => "ends_with_ignore_case",
            Self::In => "in",
            Self::NotIn => "not_in",
            Self::Regex => "regex",
            Self::RegexIgnoreCase => "regex_ignore_case",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Gt => "gt",
            Self::Gte => "gte",
        }
    }

    pub(super) const fn is_ordering(self) -> bool {
        matches!(self, Self::Lt | Self::Lte | Self::Gt | Self::Gte)
    }

    pub(super) const fn is_regex(self) -> bool {
        matches!(self, Self::Regex | Self::RegexIgnoreCase)
    }

    #[cfg(feature = "serde")]
    pub(super) fn from_label(label: &str) -> Option<Self> {
        [
            Self::Eq,
            Self::EqIgnoreCase,
            Self::Contains,
            Self::ContainsIgnoreCase,
            Self::StartsWith,
            Self::StartsWithIgnoreCase,
            Self::EndsWith,
            Self::EndsWithIgnoreCase,
            Self::In,
            Self::NotIn,
            Self::Regex,
            Self::RegexIgnoreCase,
            Self::Lt,
            Self::Lte,
            Self::Gt,
            Self::Gte,
        ]
        .into_iter()
        .find(|operator| operator.label() == label)
    }

    #[cfg(feature = "serde")]
    pub(super) const fn set_operator(self) -> Option<SetOperator> {
        match self {
            Self::Eq => Some(SetOperator::Eq),
            Self::In => Some(SetOperator::In),
            Self::NotIn => Some(SetOperator::NotIn),
            Self::Lt => Some(SetOperator::Lt),
            Self::Lte => Some(SetOperator::Lte),
            Self::Gt => Some(SetOperator::Gt),
            Self::Gte => Some(SetOperator::Gte),
            Self::EqIgnoreCase
            | Self::Contains
            | Self::ContainsIgnoreCase
            | Self::StartsWith
            | Self::StartsWithIgnoreCase
            | Self::EndsWith
            | Self::EndsWithIgnoreCase
            | Self::Regex
            | Self::RegexIgnoreCase => None,
        }
    }

    #[cfg(feature = "serde")]
    pub(super) const fn from_set(operator: SetOperator) -> Option<Self> {
        match operator {
            SetOperator::Eq => Some(Self::Eq),
            SetOperator::In => Some(Self::In),
            SetOperator::NotIn => Some(Self::NotIn),
            SetOperator::Lt | SetOperator::Lte | SetOperator::Gt | SetOperator::Gte => None,
        }
    }

    #[cfg(feature = "schema")]
    pub(super) const SCALAR_SCHEMA: [Self; 10] = [
        Self::Eq,
        Self::EqIgnoreCase,
        Self::Contains,
        Self::ContainsIgnoreCase,
        Self::StartsWith,
        Self::StartsWithIgnoreCase,
        Self::EndsWith,
        Self::EndsWithIgnoreCase,
        Self::Regex,
        Self::RegexIgnoreCase,
    ];
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SetOperator {
    Eq,
    In,
    NotIn,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl SetOperator {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::In => "in",
            Self::NotIn => "not_in",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Gt => "gt",
            Self::Gte => "gte",
        }
    }

    pub(super) const fn is_ordering(self) -> bool {
        matches!(self, Self::Lt | Self::Lte | Self::Gt | Self::Gte)
    }

    #[cfg(feature = "serde")]
    pub(super) const fn takes_a_set(self) -> bool {
        matches!(self, Self::In | Self::NotIn)
    }

    #[cfg(feature = "schema")]
    pub(super) const EQ_SCHEMA: [Self; 1] = [Self::Eq];
    #[cfg(feature = "schema")]
    pub(super) const COMPARISON_SCHEMA: [Self; 5] =
        [Self::Eq, Self::Lt, Self::Lte, Self::Gt, Self::Gte];
    #[cfg(feature = "schema")]
    pub(super) const MEMBERSHIP_SCHEMA: [Self; 2] = [Self::In, Self::NotIn];
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum RelationQuantifier {
    Any,
    All,
    None,
    Is,
}

impl RelationQuantifier {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
            Self::None => "none",
            Self::Is => "is",
        }
    }

    #[cfg(feature = "serde")]
    pub(super) fn from_label(label: &str) -> Option<Self> {
        [Self::Any, Self::All, Self::None, Self::Is]
            .into_iter()
            .find(|quantifier| quantifier.label() == label)
    }

    #[cfg(feature = "schema")]
    pub(super) const MANY_SCHEMA: [Self; 3] = [Self::Any, Self::All, Self::None];
    #[cfg(feature = "schema")]
    pub(super) const ONE_SCHEMA: [Self; 1] = [Self::Is];
}
