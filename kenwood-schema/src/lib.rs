#![doc = include_str!("../README.md")]

pub use codec::{
    BooleanDecoding, CodecError, CodecSpec, DecodeOptions, DecodedFieldValue, Endian, FieldCodec,
    FieldValue, StringEncoding, TextPolicy, ValueDomain, ValueKind,
};
pub use domain::{ChoiceError, validate_choices};
pub use patch::{ByteClaims, MaskedByte, PatchError};

mod codec;
mod domain;
mod patch;
