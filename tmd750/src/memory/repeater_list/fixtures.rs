//! Stored records shared by the repeater-list tests: four entries of a
//! captured list, one per hemisphere pair, and the empty-slot form.

use super::REPEATER_RECORD_SIZE;

/// Slot 0 of the captured list: Akihabara, JP1YLA A, north and east.
pub(super) const AKIHABARA: &str = "416b6968616261726100000000000000546f6b796f00000000000000000000004a5031594c4120414a5031594c4120478032e319404b4c002329e4258b2e6009f3015c02ffff0000000000eeffff0100";
/// Slot 221: Reykjavik, TF3RPI B, north and west.
pub(super) const REYKJAVIK: &str = "5265796b6a6176696b000000000000004361706974616c00000000000000000054463352504920425446335250492047b01a391a404b4c003f3abc1b1526c800fb013c01ffff021e470000eedc00de00";
/// Slot 97: Farm Ozondjisse, V53DV C, south and east.
pub(super) const OZONDJISSE: &str = "4661726d204f7a6f6e646a697373650045726f6e676f000000000000000000005635334456202043563533445620204700aead08c0270900150c1c2510110019f7013c01ffff010a140000ee60006200";
/// Slot 1410: Buenos Aires, LU3AOC B, south and west.
pub(super) const BUENOS_AIRES: &str = "4275656e6f73204169726573000000004275656e6f73204169726573000000004c5533414f4320424c5533414f43204770cfec19404b4c00222498083a169821ff012c02ffff043cc70000ee81058305";
/// A slot that holds no repeater.
pub(super) const UNUSED: &str = "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffffffffffff0000000000000000fbffffffffffffffffff00f0ffffffff";

/// The record bytes `hex` spells.
pub(super) fn bytes(hex: &str) -> Result<[u8; REPEATER_RECORD_SIZE], Box<dyn std::error::Error>> {
    let raw = (0..hex.len())
        .step_by(2)
        .map(|index| {
            let digits = hex.get(index..index + 2).ok_or("an even digit count")?;
            Ok(u8::from_str_radix(digits, 16)?)
        })
        .collect::<Result<Vec<u8>, Box<dyn std::error::Error>>>()?;
    Ok(raw
        .try_into()
        .map_err(|_raw: Vec<u8>| "a record is 80 bytes")?)
}

/// The record `hex` spells with its links replaced (`None` stores `FFFF`).
pub(super) fn linked(
    hex: &str,
    previous: Option<u16>,
    next: Option<u16>,
) -> Result<[u8; REPEATER_RECORD_SIZE], Box<dyn std::error::Error>> {
    let mut raw = bytes(hex)?;
    let links = [previous, next].map(|link| link.unwrap_or(u16::MAX).to_le_bytes());
    raw.get_mut(76..80)
        .ok_or("the link bytes")?
        .copy_from_slice(&links.concat());
    Ok(raw)
}
