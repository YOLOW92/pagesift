//! Compatibility for the error strings exposed by the existing CDP interface.
use crate::{Error, STANDARD};

fn invalid(offset: usize, byte: u8) -> String {
    format!("Invalid symbol {byte}, offset {offset}.")
}

/// Decode standard padded base64, retaining the error text of base64 0.23.
/// This compatibility formatter is used only on malformed input.
pub fn decode_standard(input: &[u8]) -> Result<Vec<u8>, String> {
    match STANDARD.decode_to_vec(input) {
        Ok(value) => Ok(value),
        Err(_) => Err(error_message(input)),
    }
}

fn error_message(input: &[u8]) -> String {
    let rem = input.len() % 4;
    if rem == 1 {
        let offset = input.len() - 1;
        let byte = input[offset];
        if byte != b'=' && STANDARD.value(byte, offset).is_err() {
            return invalid(offset, byte);
        }
    }
    let prefix = input
        .len()
        .saturating_sub(rem)
        .saturating_sub(if rem == 0 { 4 } else { 0 });
    for (offset, &byte) in input[..prefix].iter().enumerate() {
        if STANDARD.value(byte, offset).is_err() {
            return invalid(offset, byte);
        }
    }
    let mut pads = 0usize;
    let mut first_padding = 0usize;
    let mut digits = 0usize;
    let mut last = (0u8, 0u8, 0usize);
    for (i, &byte) in input[prefix..].iter().enumerate() {
        let offset = prefix + i;
        if byte == b'=' {
            if i < 2 {
                return invalid(offset, byte);
            }
            if pads == 0 {
                first_padding = offset;
            }
            pads += 1;
        } else {
            if pads > 0 {
                return invalid(first_padding, b'=');
            }
            match STANDARD.value(byte, offset) {
                Ok(value) => last = (byte, value, offset),
                Err(Error::InvalidByte { .. }) => return invalid(offset, byte),
                Err(_) => return "Invalid padding".into(),
            }
            digits += 1;
        }
    }
    if !input.is_empty() && digits < 2 {
        return format!("Invalid input length: {}", prefix + digits);
    }
    if !(digits + pads).is_multiple_of(4) {
        return "Invalid padding".into();
    }
    if (digits == 2 && last.1 & 15 != 0) || (digits == 3 && last.1 & 3 != 0) {
        return format!(
            "Invalid last symbol {:#4x} ('{}') at offset {}, decoded as {:#010b}.",
            last.0,
            char::from(last.0),
            last.2,
            last.1
        );
    }
    "Invalid padding".into()
}
