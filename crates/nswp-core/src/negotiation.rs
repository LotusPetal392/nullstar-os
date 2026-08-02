use crate::{
    BoundProtocol, ConnectionLimits, DecodeError, EncodeError, NegotiationError, ProtocolId,
};

pub const NEGOTIATE_REQUEST_ROOT_BYTES: usize = 48;
pub const NEGOTIATE_RESPONSE_ROOT_BYTES: usize = 64;
pub const FEATURE_RECORD_BYTES: usize = 8;

const REQUEST_FEATURES_OFFSET: usize = 0x20;
const RESPONSE_FEATURES_OFFSET: usize = 0x28;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureFlags(u32);

impl FeatureFlags {
    pub const OPTIONAL: Self = Self(0);
    pub const REQUIRED: Self = Self(1);

    pub const fn bits(self) -> u32 {
        self.0
    }

    const fn from_request_bits(bits: u32) -> Result<Self, NegotiationError> {
        match bits {
            0 => Ok(Self::OPTIONAL),
            1 => Ok(Self::REQUIRED),
            _ => Err(NegotiationError::UnknownFeatureFlags),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureRecord {
    pub id: u32,
    pub flags: FeatureFlags,
}

impl FeatureRecord {
    pub const fn optional(id: u32) -> Self {
        Self {
            id,
            flags: FeatureFlags::OPTIONAL,
        }
    }

    pub const fn required(id: u32) -> Self {
        Self {
            id,
            flags: FeatureFlags::REQUIRED,
        }
    }

    pub const fn enabled(id: u32) -> Self {
        Self::optional(id)
    }

    pub const fn is_required(self) -> bool {
        self.flags.bits() == FeatureFlags::REQUIRED.bits()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeatureEncoding {
    Request,
    Response,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureList<'a> {
    bytes: &'a [u8],
    encoding: FeatureEncoding,
}

impl<'a> FeatureList<'a> {
    const fn empty(encoding: FeatureEncoding) -> Self {
        Self {
            bytes: &[],
            encoding,
        }
    }

    pub const fn len(self) -> usize {
        self.bytes.len() / FEATURE_RECORD_BYTES
    }

    pub const fn is_empty(self) -> bool {
        self.bytes.is_empty()
    }

    pub fn get(self, index: usize) -> Option<FeatureRecord> {
        let offset = index.checked_mul(FEATURE_RECORD_BYTES)?;
        let end = offset.checked_add(FEATURE_RECORD_BYTES)?;
        let bytes = self.bytes.get(offset..end)?;
        let flags = get_u32(bytes, 4);
        Some(FeatureRecord {
            id: get_u32(bytes, 0),
            flags: match self.encoding {
                FeatureEncoding::Request => FeatureFlags::from_request_bits(flags).ok()?,
                FeatureEncoding::Response if flags == 0 => FeatureFlags::OPTIONAL,
                FeatureEncoding::Response => return None,
            },
        })
    }

    pub fn iter(self) -> FeatureIter<'a> {
        FeatureIter {
            list: self,
            index: 0,
        }
    }

    pub fn contains(self, id: u32) -> bool {
        self.iter().any(|feature| feature.id == id)
    }
}

pub struct FeatureIter<'a> {
    list: FeatureList<'a>,
    index: usize,
}

impl Iterator for FeatureIter<'_> {
    type Item = FeatureRecord;

    fn next(&mut self) -> Option<Self::Item> {
        let feature = self.list.get(self.index)?;
        self.index += 1;
        Some(feature)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.list.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for FeatureIter<'_> {}

pub trait SelectedFeatures {
    fn contains(&self, id: u32) -> bool;
}

impl SelectedFeatures for FeatureList<'_> {
    fn contains(&self, id: u32) -> bool {
        (*self).contains(id)
    }
}

struct FeatureRecords<'a>(&'a [FeatureRecord]);

impl SelectedFeatures for FeatureRecords<'_> {
    fn contains(&self, id: u32) -> bool {
        self.0.iter().any(|feature| feature.id == id)
    }
}

struct FeatureRecordsWithExtra<'a> {
    records: &'a [FeatureRecord],
    extra_id: u32,
}

impl SelectedFeatures for FeatureRecordsWithExtra<'_> {
    fn contains(&self, id: u32) -> bool {
        id == self.extra_id || self.records.iter().any(|feature| feature.id == id)
    }
}

pub type FeatureSetValidator = fn(u16, &dyn SelectedFeatures, ConnectionLimits) -> bool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiationRequest {
    pub protocol_id: ProtocolId,
    pub protocol_major: u16,
    pub min_minor: u16,
    pub max_minor: u16,
    pub max_body_bytes: u32,
    pub max_handles: u16,
    pub max_outstanding: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedNegotiationRequest<'a> {
    root: NegotiationRequest,
    features: FeatureList<'a>,
}

impl DecodedNegotiationRequest<'_> {
    pub const fn root(&self) -> &NegotiationRequest {
        &self.root
    }

    pub const fn features(&self) -> FeatureList<'_> {
        self.features
    }
}

impl NegotiationRequest {
    pub fn encode(
        &self,
        features: &[FeatureRecord],
        output: &mut [u8],
    ) -> Result<usize, EncodeError> {
        validate_request_root(self).map_err(EncodeError::InvalidNegotiation)?;
        validate_feature_records(features, FeatureEncoding::Request)
            .map_err(EncodeError::InvalidNegotiation)?;
        let feature_bytes = features
            .len()
            .checked_mul(FEATURE_RECORD_BYTES)
            .ok_or(EncodeError::ArithmeticOverflow)?;
        let encoded_bytes = NEGOTIATE_REQUEST_ROOT_BYTES
            .checked_add(feature_bytes)
            .ok_or(EncodeError::ArithmeticOverflow)?;
        let encoded = output
            .get_mut(..encoded_bytes)
            .ok_or(EncodeError::OutputTooSmall)?;
        encoded.fill(0);
        encoded[0..16].copy_from_slice(self.protocol_id.as_bytes());
        put_u16(encoded, 0x10, self.protocol_major);
        put_u16(encoded, 0x12, self.min_minor);
        put_u16(encoded, 0x14, self.max_minor);
        put_u32(encoded, 0x18, self.max_body_bytes);
        put_u16(encoded, 0x1c, self.max_handles);
        put_u16(encoded, 0x1e, self.max_outstanding);
        encode_feature_ref(
            encoded,
            REQUEST_FEATURES_OFFSET,
            NEGOTIATE_REQUEST_ROOT_BYTES,
            features.len(),
        )?;
        encode_features(
            features,
            &mut encoded[NEGOTIATE_REQUEST_ROOT_BYTES..],
            FeatureEncoding::Request,
        );
        Ok(encoded_bytes)
    }

    pub fn decode(input: &[u8]) -> Result<DecodedNegotiationRequest<'_>, DecodeError> {
        if input.len() < NEGOTIATE_REQUEST_ROOT_BYTES {
            return Err(NegotiationError::TruncatedRoot.into());
        }
        let root = NegotiationRequest {
            protocol_id: ProtocolId::from_bytes(copy_16(input, 0)?)?,
            protocol_major: get_u16(input, 0x10),
            min_minor: get_u16(input, 0x12),
            max_minor: get_u16(input, 0x14),
            max_body_bytes: get_u32(input, 0x18),
            max_handles: get_u16(input, 0x1c),
            max_outstanding: get_u16(input, 0x1e),
        };
        if get_u16(input, 0x16) != 0 {
            return Err(NegotiationError::InvalidFlags.into());
        }
        validate_request_root(&root)?;
        let features = decode_feature_ref(
            input,
            REQUEST_FEATURES_OFFSET,
            NEGOTIATE_REQUEST_ROOT_BYTES,
            FeatureEncoding::Request,
        )?;
        Ok(DecodedNegotiationRequest { root, features })
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NegotiationStatus {
    #[default]
    Ok = 0,
    UnsupportedProtocol = 1,
    UnsupportedMajor = 2,
    NoCommonMinor = 3,
    RequiredFeatureUnavailable = 4,
    TransportBoundsTooSmall = 5,
    PolicyDenied = 6,
    Busy = 7,
    Internal = 8,
}

impl TryFrom<u32> for NegotiationStatus {
    type Error = NegotiationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::UnsupportedProtocol),
            2 => Ok(Self::UnsupportedMajor),
            3 => Ok(Self::NoCommonMinor),
            4 => Ok(Self::RequiredFeatureUnavailable),
            5 => Ok(Self::TransportBoundsTooSmall),
            6 => Ok(Self::PolicyDenied),
            7 => Ok(Self::Busy),
            8 => Ok(Self::Internal),
            _ => Err(NegotiationError::UnknownStatus),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiationResponse {
    pub protocol_id: ProtocolId,
    pub status: NegotiationStatus,
    pub protocol_major: u16,
    pub selected_minor: u16,
    pub server_min_minor: u16,
    pub server_max_minor: u16,
    pub max_body_bytes: u32,
    pub max_handles: u16,
    pub max_outstanding: u16,
    pub service_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedNegotiationResponse<'a> {
    root: NegotiationResponse,
    features: FeatureList<'a>,
}

impl DecodedNegotiationResponse<'_> {
    pub const fn root(&self) -> &NegotiationResponse {
        &self.root
    }

    pub const fn features(&self) -> FeatureList<'_> {
        self.features
    }

    pub fn validate_against(
        &self,
        request: &DecodedNegotiationRequest<'_>,
        feature_profiles: &[AvailableFeature],
        feature_set_fits: FeatureSetValidator,
    ) -> Result<(), NegotiationError> {
        validate_response_root(&self.root, self.features.is_empty())?;
        validate_available_features(feature_profiles)?;
        if self.root.protocol_id != request.root.protocol_id
            || self.root.protocol_major != request.root.protocol_major
        {
            return Err(NegotiationError::EchoMismatch);
        }
        if self.root.status != NegotiationStatus::Ok {
            return Ok(());
        }
        if self.root.selected_minor < request.root.min_minor
            || self.root.selected_minor > request.root.max_minor
            || self.root.selected_minor < self.root.server_min_minor
            || self.root.selected_minor > self.root.server_max_minor
        {
            return Err(NegotiationError::SelectionOutsideRange);
        }
        if self.root.max_body_bytes > request.root.max_body_bytes
            || self.root.max_handles > request.root.max_handles
            || self.root.max_outstanding > request.root.max_outstanding
        {
            return Err(NegotiationError::LimitIncrease);
        }
        for requested in request.features.iter() {
            if feature_profile(feature_profiles, requested.id).is_none() {
                return Err(NegotiationError::MissingFeatureProfile);
            }
        }
        for selected in self.features.iter() {
            if !request.features.contains(selected.id) {
                return Err(NegotiationError::UnexpectedFeature);
            }
            let profile = feature_profile(feature_profiles, selected.id)
                .ok_or(NegotiationError::MissingFeatureProfile)?;
            if profile.since_minor > self.root.selected_minor {
                return Err(NegotiationError::FeatureUnavailableAtVersion);
            }
        }
        if !feature_set_fits(
            self.root.selected_minor,
            &self.features,
            ConnectionLimits {
                max_body_bytes: self.root.max_body_bytes,
                max_handles: self.root.max_handles,
                max_outstanding: self.root.max_outstanding,
            },
        ) {
            return Err(NegotiationError::FeatureBoundsExceeded);
        }
        for requested in request
            .features
            .iter()
            .filter(|feature| feature.is_required())
        {
            if !self.features.contains(requested.id) {
                return Err(NegotiationError::MissingRequiredFeature);
            }
        }
        Ok(())
    }

    pub fn bind<'features>(
        &self,
        request: &DecodedNegotiationRequest<'_>,
        feature_profiles: &[AvailableFeature],
        feature_set_fits: FeatureSetValidator,
        feature_ids: &'features [u32],
    ) -> Result<BoundProtocol<'features>, NegotiationError> {
        self.validate_against(request, feature_profiles, feature_set_fits)?;
        if self.root.status != NegotiationStatus::Ok || feature_ids.len() != self.features.len() {
            return Err(NegotiationError::InvalidSuccess);
        }
        for (id, feature) in feature_ids.iter().copied().zip(self.features.iter()) {
            if id != feature.id {
                return Err(NegotiationError::UnexpectedFeature);
            }
        }
        BoundProtocol::new(
            self.root.protocol_id,
            self.root.protocol_major,
            self.root.selected_minor,
            ConnectionLimits {
                max_body_bytes: self.root.max_body_bytes,
                max_handles: self.root.max_handles,
                max_outstanding: self.root.max_outstanding,
            },
            self.root.service_generation,
            feature_ids,
        )
        .map_err(|_| NegotiationError::InvalidSuccess)
    }
}

impl NegotiationResponse {
    pub fn encode(
        &self,
        features: &[FeatureRecord],
        output: &mut [u8],
    ) -> Result<usize, EncodeError> {
        validate_response_root(self, features.is_empty())
            .map_err(EncodeError::InvalidNegotiation)?;
        validate_feature_records(features, FeatureEncoding::Response)
            .map_err(EncodeError::InvalidNegotiation)?;
        let feature_bytes = features
            .len()
            .checked_mul(FEATURE_RECORD_BYTES)
            .ok_or(EncodeError::ArithmeticOverflow)?;
        let encoded_bytes = NEGOTIATE_RESPONSE_ROOT_BYTES
            .checked_add(feature_bytes)
            .ok_or(EncodeError::ArithmeticOverflow)?;
        let encoded = output
            .get_mut(..encoded_bytes)
            .ok_or(EncodeError::OutputTooSmall)?;
        encoded.fill(0);
        encoded[0..16].copy_from_slice(self.protocol_id.as_bytes());
        put_u32(encoded, 0x10, self.status as u32);
        put_u16(encoded, 0x14, self.protocol_major);
        put_u16(encoded, 0x16, self.selected_minor);
        put_u16(encoded, 0x18, self.server_min_minor);
        put_u16(encoded, 0x1a, self.server_max_minor);
        put_u32(encoded, 0x1c, self.max_body_bytes);
        put_u16(encoded, 0x20, self.max_handles);
        put_u16(encoded, 0x22, self.max_outstanding);
        encode_feature_ref(
            encoded,
            RESPONSE_FEATURES_OFFSET,
            NEGOTIATE_RESPONSE_ROOT_BYTES,
            features.len(),
        )?;
        put_u64(encoded, 0x38, self.service_generation);
        encode_features(
            features,
            &mut encoded[NEGOTIATE_RESPONSE_ROOT_BYTES..],
            FeatureEncoding::Response,
        );
        Ok(encoded_bytes)
    }

    pub fn decode(input: &[u8]) -> Result<DecodedNegotiationResponse<'_>, DecodeError> {
        if input.len() < NEGOTIATE_RESPONSE_ROOT_BYTES {
            return Err(NegotiationError::TruncatedRoot.into());
        }
        let root = NegotiationResponse {
            protocol_id: ProtocolId::from_bytes(copy_16(input, 0)?)?,
            status: NegotiationStatus::try_from(get_u32(input, 0x10))?,
            protocol_major: get_u16(input, 0x14),
            selected_minor: get_u16(input, 0x16),
            server_min_minor: get_u16(input, 0x18),
            server_max_minor: get_u16(input, 0x1a),
            max_body_bytes: get_u32(input, 0x1c),
            max_handles: get_u16(input, 0x20),
            max_outstanding: get_u16(input, 0x22),
            service_generation: get_u64(input, 0x38),
        };
        if get_u32(input, 0x24) != 0 {
            return Err(NegotiationError::InvalidFlags.into());
        }
        let features = decode_feature_ref(
            input,
            RESPONSE_FEATURES_OFFSET,
            NEGOTIATE_RESPONSE_ROOT_BYTES,
            FeatureEncoding::Response,
        )?;
        validate_response_root(&root, features.is_empty())?;
        Ok(DecodedNegotiationResponse { root, features })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinorVersionProfile {
    pub minor: u16,
    pub minimum_body_bytes: u32,
    pub minimum_handles: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvailableFeature {
    pub id: u32,
    pub since_minor: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct ServerProfile<'a> {
    pub protocol_id: ProtocolId,
    pub protocol_major: u16,
    pub server_limits: ConnectionLimits,
    pub transport_limits: ConnectionLimits,
    pub protocol_limits: ConnectionLimits,
    pub service_generation: u64,
    pub versions: &'a [MinorVersionProfile],
    pub available_features: &'a [AvailableFeature],
    pub feature_set_fits: FeatureSetValidator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiationOutcome {
    pub response: NegotiationResponse,
    pub feature_count: usize,
}

pub fn negotiate(
    request: &DecodedNegotiationRequest<'_>,
    profile: &ServerProfile<'_>,
    selected_features: &mut [FeatureRecord],
) -> Result<NegotiationOutcome, NegotiationError> {
    validate_profile(profile)?;
    if request.root.max_handles > profile.transport_limits.max_handles {
        return Err(NegotiationError::InvalidLimits);
    }
    let server_min_minor = profile.versions[0].minor;
    let server_max_minor = profile.versions[profile.versions.len() - 1].minor;
    let failure = |status| {
        let (diagnostic_min, diagnostic_max) = match status {
            NegotiationStatus::UnsupportedProtocol | NegotiationStatus::UnsupportedMajor => (0, 0),
            _ => (server_min_minor, server_max_minor),
        };
        NegotiationOutcome {
            response: NegotiationResponse {
                protocol_id: request.root.protocol_id,
                status,
                protocol_major: request.root.protocol_major,
                selected_minor: 0,
                server_min_minor: diagnostic_min,
                server_max_minor: diagnostic_max,
                max_body_bytes: 0,
                max_handles: 0,
                max_outstanding: 0,
                service_generation: 0,
            },
            feature_count: 0,
        }
    };

    if request.root.protocol_id != profile.protocol_id {
        return Ok(failure(NegotiationStatus::UnsupportedProtocol));
    }
    if request.root.protocol_major != profile.protocol_major {
        return Ok(failure(NegotiationStatus::UnsupportedMajor));
    }

    let requested_limits = ConnectionLimits {
        max_body_bytes: request.root.max_body_bytes,
        max_handles: request.root.max_handles,
        max_outstanding: request.root.max_outstanding,
    };
    let limits = requested_limits
        .component_min(profile.server_limits)
        .component_min(profile.transport_limits)
        .component_min(profile.protocol_limits);
    let mut has_common_minor = false;
    let mut required_features_available = false;
    let mut selected_minor = None;
    for version in profile.versions.iter().rev().filter(|version| {
        version.minor >= request.root.min_minor && version.minor <= request.root.max_minor
    }) {
        has_common_minor = true;
        if limits.max_body_bytes < version.minimum_body_bytes
            || limits.max_handles < version.minimum_handles
        {
            continue;
        }
        let mut required_count = 0;
        let mut required_available = true;
        for requested in request
            .features
            .iter()
            .filter(|feature| feature.is_required())
        {
            if feature_profile_at_minor(profile, requested.id, version.minor).is_none() {
                required_available = false;
                continue;
            }
            let destination = selected_features
                .get_mut(required_count)
                .ok_or(NegotiationError::OutputTooSmall)?;
            *destination = FeatureRecord::enabled(requested.id);
            required_count += 1;
        }
        if !required_available {
            continue;
        }
        required_features_available = true;
        if !(profile.feature_set_fits)(
            version.minor,
            &FeatureRecords(&selected_features[..required_count]),
            limits,
        ) {
            continue;
        }
        let mut feature_count = required_count;
        for requested in request
            .features
            .iter()
            .filter(|feature| !feature.is_required())
        {
            if feature_profile_at_minor(profile, requested.id, version.minor).is_none() {
                continue;
            }
            let candidate = FeatureRecordsWithExtra {
                records: &selected_features[..feature_count],
                extra_id: requested.id,
            };
            if !(profile.feature_set_fits)(version.minor, &candidate, limits) {
                continue;
            }
            let destination = selected_features
                .get_mut(feature_count)
                .ok_or(NegotiationError::OutputTooSmall)?;
            *destination = FeatureRecord::enabled(requested.id);
            feature_count += 1;
        }
        selected_minor = Some((version.minor, feature_count));
        break;
    }
    let Some((selected_minor, feature_count)) = selected_minor else {
        return Ok(failure(if !has_common_minor {
            NegotiationStatus::NoCommonMinor
        } else if required_features_available {
            NegotiationStatus::TransportBoundsTooSmall
        } else if request.features.iter().any(|feature| feature.is_required()) {
            NegotiationStatus::RequiredFeatureUnavailable
        } else {
            NegotiationStatus::TransportBoundsTooSmall
        }));
    };

    selected_features[..feature_count].sort_unstable_by_key(|feature| feature.id);

    Ok(NegotiationOutcome {
        response: NegotiationResponse {
            protocol_id: request.root.protocol_id,
            status: NegotiationStatus::Ok,
            protocol_major: request.root.protocol_major,
            selected_minor,
            server_min_minor,
            server_max_minor,
            max_body_bytes: limits.max_body_bytes,
            max_handles: limits.max_handles,
            max_outstanding: limits.max_outstanding,
            service_generation: profile.service_generation,
        },
        feature_count,
    })
}

fn validate_request_root(request: &NegotiationRequest) -> Result<(), NegotiationError> {
    if request.protocol_major == 0 {
        return Err(NegotiationError::InvalidProtocolMajor);
    }
    if request.min_minor > request.max_minor {
        return Err(NegotiationError::InvalidMinorRange);
    }
    if request.max_body_bytes == 0 || request.max_outstanding == 0 {
        return Err(NegotiationError::InvalidLimits);
    }
    Ok(())
}

fn validate_response_root(
    response: &NegotiationResponse,
    features_empty: bool,
) -> Result<(), NegotiationError> {
    if response.protocol_major == 0 {
        return Err(NegotiationError::InvalidProtocolMajor);
    }
    if response.server_min_minor > response.server_max_minor {
        return Err(NegotiationError::InvalidMinorRange);
    }
    if response.status == NegotiationStatus::Ok {
        if response.selected_minor < response.server_min_minor
            || response.selected_minor > response.server_max_minor
            || response.max_body_bytes == 0
            || response.max_outstanding == 0
            || response.service_generation == 0
        {
            return Err(NegotiationError::InvalidSuccess);
        }
    } else if response.selected_minor != 0
        || matches!(
            response.status,
            NegotiationStatus::UnsupportedProtocol | NegotiationStatus::UnsupportedMajor
        ) && (response.server_min_minor != 0 || response.server_max_minor != 0)
        || response.max_body_bytes != 0
        || response.max_handles != 0
        || response.max_outstanding != 0
        || !features_empty
        || response.service_generation != 0
    {
        return Err(NegotiationError::InvalidFailure);
    }
    Ok(())
}

fn validate_profile(profile: &ServerProfile<'_>) -> Result<(), NegotiationError> {
    if profile.protocol_major == 0 {
        return Err(NegotiationError::InvalidProtocolMajor);
    }
    if profile.server_limits.max_body_bytes == 0
        || profile.server_limits.max_outstanding == 0
        || profile.transport_limits.max_body_bytes == 0
        || profile.transport_limits.max_outstanding == 0
        || profile.protocol_limits.max_body_bytes == 0
        || profile.protocol_limits.max_outstanding == 0
        || profile.service_generation == 0
        || profile.versions.is_empty()
    {
        return Err(NegotiationError::InvalidLimits);
    }
    let mut previous_minor: Option<u16> = None;
    for version in profile.versions {
        if version.minimum_body_bytes == 0
            || previous_minor.is_some_and(|previous| previous.checked_add(1) != Some(version.minor))
        {
            return Err(NegotiationError::InvalidMinorRange);
        }
        previous_minor = Some(version.minor);
    }
    validate_available_features(profile.available_features)?;
    let server_max_minor = profile.versions[profile.versions.len() - 1].minor;
    if profile
        .available_features
        .iter()
        .any(|feature| feature.since_minor > server_max_minor)
    {
        return Err(NegotiationError::InvalidFeatureId);
    }
    Ok(())
}

fn validate_available_features(features: &[AvailableFeature]) -> Result<(), NegotiationError> {
    let mut previous_id = None;
    for feature in features {
        if feature.id == 0 || previous_id.is_some_and(|previous| feature.id <= previous) {
            return Err(NegotiationError::InvalidFeatureId);
        }
        previous_id = Some(feature.id);
    }
    Ok(())
}

fn feature_profile(features: &[AvailableFeature], id: u32) -> Option<&AvailableFeature> {
    features
        .binary_search_by_key(&id, |feature| feature.id)
        .ok()
        .map(|index| &features[index])
}

fn feature_profile_at_minor<'a>(
    profile: &'a ServerProfile<'_>,
    id: u32,
    selected_minor: u16,
) -> Option<&'a AvailableFeature> {
    feature_profile(profile.available_features, id)
        .filter(|feature| feature.since_minor <= selected_minor)
}

fn validate_feature_records(
    features: &[FeatureRecord],
    encoding: FeatureEncoding,
) -> Result<(), NegotiationError> {
    let mut previous = None;
    for feature in features {
        if feature.id == 0 {
            return Err(NegotiationError::InvalidFeatureId);
        }
        match encoding {
            FeatureEncoding::Request if feature.flags.bits() <= 1 => {}
            FeatureEncoding::Response if feature.flags.bits() == 0 => {}
            _ => return Err(NegotiationError::UnknownFeatureFlags),
        }
        if let Some(previous) = previous {
            if feature.id == previous {
                return Err(NegotiationError::DuplicateFeature);
            }
            if feature.id < previous {
                return Err(NegotiationError::UnsortedFeatures);
            }
        }
        previous = Some(feature.id);
    }
    Ok(())
}

fn decode_feature_ref<'a>(
    input: &'a [u8],
    field_offset: usize,
    root_bytes: usize,
    encoding: FeatureEncoding,
) -> Result<FeatureList<'a>, NegotiationError> {
    let relative_offset = get_u32(input, field_offset) as usize;
    let count = get_u32(input, field_offset + 4) as usize;
    let region_bytes = get_u32(input, field_offset + 8) as usize;
    let reserved = get_u32(input, field_offset + 12);
    if reserved != 0 {
        return Err(NegotiationError::InvalidSlice);
    }
    if count == 0 {
        if relative_offset != 0 || region_bytes != 0 || input.len() != root_bytes {
            return Err(NegotiationError::InvalidSlice);
        }
        return Ok(FeatureList::empty(encoding));
    }
    let expected_region = count
        .checked_mul(FEATURE_RECORD_BYTES)
        .ok_or(NegotiationError::ArithmeticOverflow)?;
    let expected_relative = root_bytes
        .checked_sub(field_offset)
        .ok_or(NegotiationError::ArithmeticOverflow)?;
    if relative_offset != expected_relative
        || region_bytes != expected_region
        || !region_bytes.is_multiple_of(8)
        || input.len()
            != root_bytes
                .checked_add(region_bytes)
                .ok_or(NegotiationError::ArithmeticOverflow)?
    {
        return Err(NegotiationError::InvalidSlice);
    }
    let bytes = &input[root_bytes..];
    validate_encoded_features(bytes, encoding)?;
    Ok(FeatureList { bytes, encoding })
}

fn validate_encoded_features(
    bytes: &[u8],
    encoding: FeatureEncoding,
) -> Result<(), NegotiationError> {
    let mut previous = None;
    for record in bytes.chunks_exact(FEATURE_RECORD_BYTES) {
        let id = get_u32(record, 0);
        let flags = get_u32(record, 4);
        if id == 0 {
            return Err(NegotiationError::InvalidFeatureId);
        }
        match encoding {
            FeatureEncoding::Request => {
                FeatureFlags::from_request_bits(flags)?;
            }
            FeatureEncoding::Response if flags == 0 => {}
            FeatureEncoding::Response => return Err(NegotiationError::UnknownFeatureFlags),
        }
        if let Some(previous) = previous {
            if id == previous {
                return Err(NegotiationError::DuplicateFeature);
            }
            if id < previous {
                return Err(NegotiationError::UnsortedFeatures);
            }
        }
        previous = Some(id);
    }
    Ok(())
}

fn encode_feature_ref(
    output: &mut [u8],
    field_offset: usize,
    root_bytes: usize,
    count: usize,
) -> Result<(), EncodeError> {
    if count == 0 {
        return Ok(());
    }
    let relative_offset = root_bytes
        .checked_sub(field_offset)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(EncodeError::ArithmeticOverflow)?;
    let region_bytes = count
        .checked_mul(FEATURE_RECORD_BYTES)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(EncodeError::ArithmeticOverflow)?;
    put_u32(output, field_offset, relative_offset);
    put_u32(output, field_offset + 4, count as u32);
    put_u32(output, field_offset + 8, region_bytes);
    Ok(())
}

fn encode_features(features: &[FeatureRecord], output: &mut [u8], encoding: FeatureEncoding) {
    for (feature, record) in features
        .iter()
        .zip(output.chunks_exact_mut(FEATURE_RECORD_BYTES))
    {
        put_u32(record, 0, feature.id);
        put_u32(
            record,
            4,
            match encoding {
                FeatureEncoding::Request => feature.flags.bits(),
                FeatureEncoding::Response => 0,
            },
        );
    }
}

fn copy_16(input: &[u8], offset: usize) -> Result<[u8; 16], NegotiationError> {
    let source = input
        .get(offset..offset + 16)
        .ok_or(NegotiationError::TruncatedRoot)?;
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(source);
    Ok(bytes)
}

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
