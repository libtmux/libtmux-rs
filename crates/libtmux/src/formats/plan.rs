use super::row::{FormatCodecError, FormatCodecErrorKind};
use super::{
    CLIENT_INFO_SUPPLEMENTS, CLIENT_NAME, FormatDescriptor, InfoPlacement, ListProfile, PANE_ID,
    PANE_INFO_SUPPLEMENTS, SESSION_ID, SESSION_INFO_SUPPLEMENTS, WINDOW_ID,
    WINDOW_INFO_SUPPLEMENTS,
};
use crate::version::{ReleaseSuffix, ReleaseVersion, TmuxVersion};

impl ListProfile {
    /// Return the mandatory identity descriptor for this profile.
    const fn baseline(self) -> &'static FormatDescriptor {
        match self {
            Self::Sessions => &SESSION_ID,
            Self::Windows => &WINDOW_ID,
            Self::Panes => &PANE_ID,
            Self::Clients => &CLIENT_NAME,
        }
    }

    /// Return version-gated descriptors after the mandatory identity.
    const fn supplements(self) -> &'static [&'static FormatDescriptor] {
        match self {
            Self::Sessions => SESSION_INFO_SUPPLEMENTS,
            Self::Windows => WINDOW_INFO_SUPPLEMENTS,
            Self::Panes => PANE_INFO_SUPPLEMENTS,
            Self::Clients => CLIENT_INFO_SUPPLEMENTS,
        }
    }
}

/// Version evidence retained by a format plan.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
pub(super) enum PlanVersion {
    /// Version detected from tmux.
    Detected(TmuxVersion),
    /// Fixed evidence used by descriptor-only codec fixtures.
    #[cfg(test)]
    MinimumSupportedFixture,
}

impl PlanVersion {
    /// Select the transport dialect implied by this plan's version evidence.
    fn dialect(&self) -> TransportDialect {
        match self {
            Self::Detected(version) => TransportDialect::for_version(version),
            #[cfg(test)]
            Self::MinimumSupportedFixture => TransportDialect::RawQ,
        }
    }
}

/// Escaping tmux applies to expanded format output before it reaches stdout.
///
/// The daemon that owns the socket decides this, not the client executable the
/// version probe ran. A mismatch is possible, so each dialect rejects escapes
/// the other produces instead of decoding them into different bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportDialect {
    /// `#{q:}` escaping only, printed verbatim.
    ///
    /// Applies to releases before 3.4 and from 3.6 onward.
    RawQ,
    /// `#{q:}` escaping wrapped in `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`.
    ///
    /// tmux 3.4 and 3.5 ran command output through `utf8_stravisx`, so control
    /// bytes and invalid UTF-8 arrive as `\r`-style or `\ooo` escapes.
    Vis,
}

impl TransportDialect {
    /// First release that visually encoded command output.
    ///
    /// Introduced before tag 3.4 by upstream commits `7e497c7f` and
    /// `93b1b781`.
    const VIS_FIRST: ReleaseVersion = ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);

    /// First release that restored verbatim command output.
    ///
    /// Restored before tag 3.6 by upstream commit `5fd45b38`, "Do not strvis
    /// output to terminal from commands."
    const VIS_RESTORED: ReleaseVersion = ReleaseVersion::new(3, 6, ReleaseSuffix::FINAL);

    /// Select the dialect a detected tmux version emits.
    ///
    /// `master` names no release and resolves to [`TransportDialect::RawQ`],
    /// matching every tmux tree since the 3.6 restore. A build that is wrong
    /// about this fails loudly during decoding rather than returning altered
    /// bytes, because neither dialect accepts the other's escapes.
    pub(crate) fn for_version(version: &TmuxVersion) -> Self {
        match version.behavior_release() {
            Some(release) if release >= Self::VIS_FIRST && release < Self::VIS_RESTORED => {
                Self::Vis
            }
            _ => Self::RawQ,
        }
    }
}

/// Consumer intent carried by a format plan.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanPurpose {
    /// Complete intrinsic snapshot for one placement.
    Intrinsic(InfoPlacement),
    /// Explicit trusted-static projection.
    Projection,
}

/// Version-selection state for one planned field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanFieldState {
    /// Field is rendered at this selected-slot coordinate.
    Selected { slot: usize },
    /// Numbered tmux release predates the field.
    Unsupported,
    /// Development build provides no numbered availability proof.
    Unproven,
}

/// Descriptor and availability evidence retained in request order.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlannedField {
    /// Trusted static descriptor.
    pub(crate) descriptor: &'static FormatDescriptor,
    /// Selected slot or unavailable evidence.
    pub(crate) state: PlanFieldState,
}

/// Ordered trusted metadata and exact tmux format template.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
pub(crate) struct FormatPlan {
    /// List operation represented by the plan.
    pub(super) profile: ListProfile,
    /// Version evidence used to select descriptors.
    pub(super) version: PlanVersion,
    /// Mandatory identity, stored independently from optional catalog entries.
    pub(super) baseline: &'static FormatDescriptor,
    /// Intrinsic or explicit projection intent.
    pub(super) purpose: PlanPurpose,
    /// Complete requested fields, including unavailable evidence.
    pub(super) planned: Box<[PlannedField]>,
    /// Descriptor order shared by template rendering and row parsing.
    pub(super) descriptors: Box<[&'static FormatDescriptor]>,
    /// Exact template rendered from `descriptors`.
    pub(super) template: Box<str>,
    /// Transport escaping the planned version emits.
    pub(super) dialect: TransportDialect,
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl FormatPlan {
    /// Build a plan from one supported detected tmux version.
    pub(crate) fn for_profile(profile: ListProfile, version: &TmuxVersion) -> Self {
        select_for_profile(profile, version, profile.supplements())
    }

    /// Build an explicit projection from trusted static descriptors.
    pub(crate) fn for_descriptors(
        profile: ListProfile,
        version: &TmuxVersion,
        requested: &[&'static FormatDescriptor],
    ) -> Result<Self, FormatCodecError> {
        let baseline = profile.baseline();
        let mut descriptors = vec![baseline];
        let mut planned = vec![PlannedField {
            descriptor: baseline,
            state: PlanFieldState::Selected { slot: 0 },
        }];
        let mut seen = std::collections::HashSet::with_capacity(requested.len());

        for descriptor in requested.iter().copied() {
            if !descriptor.profiles().contains(profile) {
                return Err(FormatCodecError::plan(
                    FormatCodecErrorKind::ScopeInapplicable,
                    descriptor,
                    profile,
                ));
            }
            if std::ptr::eq(descriptor, baseline) {
                continue;
            }
            if !seen.insert(std::ptr::from_ref(descriptor)) {
                return Err(FormatCodecError::plan(
                    FormatCodecErrorKind::DuplicateDescriptor,
                    descriptor,
                    profile,
                ));
            }

            let state = classify_field(version, descriptor, descriptors.len());
            if matches!(state, PlanFieldState::Selected { .. }) {
                descriptors.push(descriptor);
            }
            planned.push(PlannedField { descriptor, state });
        }

        Ok(Self::build(
            profile,
            PlanVersion::Detected(version.clone()),
            baseline,
            PlanPurpose::Projection,
            planned,
            descriptors,
        ))
    }

    /// Construct a descriptor-only plan for codec tests.
    #[cfg(test)]
    pub(crate) fn for_codec_test(
        descriptors: Vec<&'static FormatDescriptor>,
    ) -> Result<Self, FormatCodecError> {
        Self::for_codec_test_with(descriptors, PlanVersion::MinimumSupportedFixture)
    }

    /// Construct a descriptor-only plan whose dialect follows a real version.
    #[cfg(test)]
    pub(crate) fn for_codec_test_at(
        descriptors: Vec<&'static FormatDescriptor>,
        version: &TmuxVersion,
    ) -> Result<Self, FormatCodecError> {
        Self::for_codec_test_with(descriptors, PlanVersion::Detected(version.clone()))
    }

    /// Share codec-fixture plan construction across version evidence.
    #[cfg(test)]
    fn for_codec_test_with(
        descriptors: Vec<&'static FormatDescriptor>,
        version: PlanVersion,
    ) -> Result<Self, FormatCodecError> {
        let Some(baseline) = descriptors.first().copied() else {
            return Err(FormatCodecError::empty_plan());
        };

        Ok(Self::build(
            ListProfile::Sessions,
            version,
            baseline,
            PlanPurpose::Projection,
            descriptors
                .iter()
                .copied()
                .enumerate()
                .map(|(slot, descriptor)| PlannedField {
                    descriptor,
                    state: PlanFieldState::Selected { slot },
                })
                .collect(),
            descriptors,
        ))
    }

    /// Return the selected descriptor sequence to codec fixtures.
    #[cfg(test)]
    pub(crate) fn descriptors_for_test(&self) -> &[&'static FormatDescriptor] {
        &self.descriptors
    }

    /// Return this plan's list profile.
    pub(crate) const fn profile(&self) -> ListProfile {
        self.profile
    }

    /// Return this plan's purpose.
    pub(crate) const fn purpose(&self) -> PlanPurpose {
        self.purpose
    }

    /// Return complete planned availability evidence.
    pub(crate) fn planned(&self) -> &[PlannedField] {
        &self.planned
    }

    /// Return the exact format template passed to tmux.
    pub(crate) fn template(&self) -> &str {
        &self.template
    }

    /// Store one sound ordered selection and render from that same sequence.
    fn build(
        profile: ListProfile,
        version: PlanVersion,
        baseline: &'static FormatDescriptor,
        purpose: PlanPurpose,
        planned: Vec<PlannedField>,
        descriptors: Vec<&'static FormatDescriptor>,
    ) -> Self {
        let descriptors = descriptors.into_boxed_slice();
        let mut template = String::new();
        for descriptor in &descriptors {
            template.push_str("#{q:");
            template.push_str(descriptor.name());
            template.push_str("}%");
        }

        let dialect = version.dialect();
        Self {
            profile,
            version,
            baseline,
            purpose,
            planned: planned.into_boxed_slice(),
            descriptors,
            template: template.into_boxed_str(),
            dialect,
        }
    }
}

/// Select a profile's mandatory identity and supported supplements.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
fn select_for_profile(
    profile: ListProfile,
    version: &TmuxVersion,
    supplements: &'static [&'static FormatDescriptor],
) -> FormatPlan {
    let baseline = profile.baseline();
    let mut descriptors = Vec::with_capacity(supplements.len() + 1);
    descriptors.push(baseline);
    let mut planned = Vec::with_capacity(supplements.len() + 1);
    planned.push(PlannedField {
        descriptor: baseline,
        state: PlanFieldState::Selected { slot: 0 },
    });

    for descriptor in supplements.iter().copied() {
        if std::ptr::eq(descriptor, baseline) {
            continue;
        }

        let state = classify_field(version, descriptor, descriptors.len());
        if matches!(state, PlanFieldState::Selected { .. }) {
            descriptors.push(descriptor);
        }
        planned.push(PlannedField { descriptor, state });
    }

    FormatPlan::build(
        profile,
        PlanVersion::Detected(version.clone()),
        baseline,
        PlanPurpose::Intrinsic(baseline.placement()),
        planned,
        descriptors,
    )
}

/// Classify one nonbaseline field from numbered or development evidence.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
fn classify_field(
    version: &TmuxVersion,
    descriptor: &'static FormatDescriptor,
    selected_slot: usize,
) -> PlanFieldState {
    match version.release() {
        Some(release) if *release >= descriptor.minimum_release() => PlanFieldState::Selected {
            slot: selected_slot,
        },
        Some(_) => PlanFieldState::Unsupported,
        None if descriptor.minimum_release() <= TmuxVersion::MIN_SUPPORTED => {
            PlanFieldState::Selected {
                slot: selected_slot,
            }
        }
        None => PlanFieldState::Unproven,
    }
}

/// Exercise the production profile selector with synthetic static metadata.
#[cfg(test)]
pub(super) fn for_profile_selection_test(
    profile: ListProfile,
    version: &TmuxVersion,
    supplements: &'static [&'static FormatDescriptor],
) -> FormatPlan {
    select_for_profile(profile, version, supplements)
}
