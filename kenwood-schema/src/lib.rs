#![doc = include_str!("../README.md")]

mod codec;
mod domain;
mod patch;

pub use codec::{
    BooleanDecoding, CodecError, DecodeOptions, DecodedFieldValue, Endian, FieldCodec, FieldValue,
    StringEncoding, TextPolicy, ValueDomain,
};
pub use domain::{ChoiceError, validate_choices};
pub use patch::{ByteClaims, MaskedByte, PatchError};
