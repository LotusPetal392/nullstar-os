#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolIdError {
    InvalidLength,
    NonCanonical,
    InvalidHex,
    Nil,
    AllOnes,
    InvalidVersion,
    InvalidVariant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NegotiationError {
    TruncatedRoot,
    InvalidProtocolMajor,
    InvalidMinorRange,
    InvalidLimits,
    InvalidFlags,
    InvalidSlice,
    InvalidFeatureId,
    UnknownFeatureFlags,
    UnknownStatus,
    UnsortedFeatures,
    DuplicateFeature,
    InvalidSuccess,
    InvalidFailure,
    EchoMismatch,
    SelectionOutsideRange,
    LimitIncrease,
    MissingRequiredFeature,
    MissingFeatureProfile,
    FeatureUnavailableAtVersion,
    FeatureBoundsExceeded,
    UnexpectedFeature,
    OutputTooSmall,
    ArithmeticOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    PacketTooShort,
    InvalidMagic,
    UnsupportedWireVersion,
    InvalidHeaderSize,
    UnknownPacketKind,
    UnknownFlags,
    ReservedValueUsed,
    UnknownTransportStatus,
    InvalidPacketRelationship,
    InvalidDirection,
    InvalidConnectionState,
    LengthMismatch,
    BodyNotAligned,
    BodyLimitExceeded,
    HandleCountMismatch,
    HandleLimitExceeded,
    InvalidProtocolId(ProtocolIdError),
    InvalidNegotiation(NegotiationError),
    InvalidProtocolError,
    TransactionReuse,
    TransactionMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    InvalidHeader(DecodeError),
    InvalidNegotiation(NegotiationError),
    OutputTooSmall,
    ArithmeticOverflow,
}

impl From<ProtocolIdError> for DecodeError {
    fn from(error: ProtocolIdError) -> Self {
        Self::InvalidProtocolId(error)
    }
}

impl From<NegotiationError> for DecodeError {
    fn from(error: NegotiationError) -> Self {
        Self::InvalidNegotiation(error)
    }
}
