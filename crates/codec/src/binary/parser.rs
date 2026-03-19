use serde_json::{Map, Value};

use super::definitions;
use super::field_id;
use crate::error::CodecError;

/// Binary parser for XRPL objects.
pub struct BinaryParser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BinaryParser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.pos + n > self.data.len() {
            return Err(CodecError::UnexpectedEnd);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, CodecError> {
        let bytes = self.read_bytes(1)?;
        Ok(bytes[0])
    }

    fn read_u16(&mut self) -> Result<u16, CodecError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, CodecError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64(&mut self) -> Result<u64, CodecError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_be_bytes(bytes.try_into().unwrap()))
    }

    fn read_vl_length(&mut self) -> Result<usize, CodecError> {
        let b1 = self.read_u8()? as usize;
        if b1 <= 192 {
            Ok(b1)
        } else if b1 <= 240 {
            let b2 = self.read_u8()? as usize;
            Ok(193 + ((b1 - 193) << 8) + b2)
        } else if b1 <= 254 {
            let b2 = self.read_u8()? as usize;
            let b3 = self.read_u8()? as usize;
            Ok(12481 + ((b1 - 241) << 16) + (b2 << 8) + b3)
        } else {
            Err(CodecError::UnexpectedEnd)
        }
    }

    /// Parse a complete XRPL binary object into JSON.
    pub fn parse_object(&mut self) -> Result<Value, CodecError> {
        let mut map = Map::new();

        while self.remaining() > 0 {
            let (type_code, field_code, consumed) =
                field_id::decode_field_id(&self.data[self.pos..])?;
            self.pos += consumed;

            // Check for object/array end markers
            if type_code == 14 && field_code == 1 {
                break; // ObjectEndMarker
            }
            if type_code == 15 && field_code == 1 {
                break; // ArrayEndMarker
            }

            let Some(def) = definitions::get_field_by_header(type_code, field_code) else {
                return Err(CodecError::UnknownField(format!(
                    "type={type_code}, field={field_code}"
                )));
            };

            let value = self.parse_field_value(&def.field_type, &def.name)?;
            map.insert(def.name.clone(), value);
        }

        Ok(Value::Object(map))
    }

    fn parse_field_value(
        &mut self,
        field_type: &str,
        field_name: &str,
    ) -> Result<Value, CodecError> {
        match field_type {
            "UInt8" => {
                let v = self.read_u8()?;
                Ok(Value::Number(v.into()))
            }
            "UInt16" => {
                let v = self.read_u16()?;
                // Convert known enum types to names
                match field_name {
                    "TransactionType" => {
                        if let Some(name) = definitions::get_transaction_type_name(v as i32) {
                            Ok(Value::String(name.to_string()))
                        } else {
                            Ok(Value::Number(v.into()))
                        }
                    }
                    "LedgerEntryType" => {
                        if let Some(name) = definitions::get_ledger_entry_type_name(v as i32) {
                            Ok(Value::String(name.to_string()))
                        } else {
                            Ok(Value::Number(v.into()))
                        }
                    }
                    _ => Ok(Value::Number(v.into())),
                }
            }
            "UInt32" => {
                let v = self.read_u32()?;
                Ok(Value::Number(v.into()))
            }
            "UInt64" => {
                let v = self.read_u64()?;
                Ok(Value::String(format!("{v:016X}")))
            }
            "Hash128" => {
                let bytes = self.read_bytes(16)?;
                Ok(Value::String(hex::encode_upper(bytes)))
            }
            "Hash160" => {
                let bytes = self.read_bytes(20)?;
                Ok(Value::String(hex::encode_upper(bytes)))
            }
            "Hash192" => {
                let bytes = self.read_bytes(24)?;
                Ok(Value::String(hex::encode_upper(bytes)))
            }
            "Hash256" => {
                let bytes = self.read_bytes(32)?;
                Ok(Value::String(hex::encode_upper(bytes)))
            }
            "Amount" => self.parse_amount(),
            "Blob" => {
                let len = self.read_vl_length()?;
                let bytes = self.read_bytes(len)?;
                Ok(Value::String(hex::encode_upper(bytes)))
            }
            "AccountID" => {
                let len = self.read_vl_length()?;
                let bytes = self.read_bytes(len)?;
                // Encode as classic address
                let account_id = rxrpl_primitives::AccountId::from_slice(bytes)
                    .map_err(|e| CodecError::InvalidAddress(e.to_string()))?;
                let address = crate::address::encode_account_id(&account_id);
                Ok(Value::String(address))
            }
            "STObject" => self.parse_object(),
            "STArray" => self.parse_array(),
            "Vector256" => {
                let len = self.read_vl_length()?;
                let count = len / 32;
                let mut arr = Vec::with_capacity(count);
                for _ in 0..count {
                    let bytes = self.read_bytes(32)?;
                    arr.push(Value::String(hex::encode_upper(bytes)));
                }
                Ok(Value::Array(arr))
            }
            "PathSet" => self.parse_pathset(),
            "Issue" => self.parse_issue(),
            "Currency" => {
                let bytes = self.read_bytes(20)?;
                Ok(Value::String(decode_currency_code(bytes)))
            }
            "XChainBridge" => self.parse_object(),
            "Number" => self.parse_number(),
            other => Err(CodecError::UnsupportedType(format!(
                "unsupported field type: {other}"
            ))),
        }
    }

    fn parse_amount(&mut self) -> Result<Value, CodecError> {
        let raw = self.read_u64()?;

        // Check if it's XRP (bit 63 is 0) or IOU (bit 63 is 1)
        if raw & 0x8000_0000_0000_0000 == 0 {
            // XRP amount
            let positive = raw & 0x4000_0000_0000_0000 != 0;
            let amount = raw & 0x3FFF_FFFF_FFFF_FFFF;
            let drops = if positive {
                amount as i64
            } else {
                -(amount as i64)
            };
            Ok(Value::String(drops.to_string()))
        } else {
            // IOU amount
            let value_str = decode_iou_value(raw);

            // Read currency (20 bytes) and issuer (20 bytes)
            let currency_bytes = self.read_bytes(20)?;
            let issuer_bytes = self.read_bytes(20)?;

            let currency = decode_currency_code(currency_bytes);
            let issuer = {
                let account_id = rxrpl_primitives::AccountId::from_slice(issuer_bytes)
                    .map_err(|e| CodecError::InvalidAddress(e.to_string()))?;
                crate::address::encode_account_id(&account_id)
            };

            let mut map = Map::new();
            map.insert("value".to_string(), Value::String(value_str));
            map.insert("currency".to_string(), Value::String(currency));
            map.insert("issuer".to_string(), Value::String(issuer));
            Ok(Value::Object(map))
        }
    }

    fn parse_array(&mut self) -> Result<Value, CodecError> {
        let mut arr = Vec::new();

        while self.remaining() > 0 {
            let (type_code, field_code, consumed) =
                field_id::decode_field_id(&self.data[self.pos..])?;
            self.pos += consumed;

            // Array end marker
            if type_code == 15 && field_code == 1 {
                break;
            }

            let Some(def) = definitions::get_field_by_header(type_code, field_code) else {
                return Err(CodecError::UnknownField(format!(
                    "type={type_code}, field={field_code}"
                )));
            };

            let inner = self.parse_object()?;
            let mut wrapper = Map::new();
            wrapper.insert(def.name.clone(), inner);
            arr.push(Value::Object(wrapper));
        }

        Ok(Value::Array(arr))
    }

    fn parse_pathset(&mut self) -> Result<Value, CodecError> {
        let mut paths = Vec::new();
        let mut current_path = Vec::new();

        loop {
            if self.remaining() == 0 {
                break;
            }

            let type_byte = self.read_u8()?;

            if type_byte == 0x00 {
                // End of PathSet
                if !current_path.is_empty() {
                    paths.push(Value::Array(std::mem::take(&mut current_path)));
                }
                break;
            } else if type_byte == 0xFF {
                // Path separator
                paths.push(Value::Array(std::mem::take(&mut current_path)));
            } else {
                // Path step
                let mut step = Map::new();

                if type_byte & 0x01 != 0 {
                    let bytes = self.read_bytes(20)?;
                    let account_id = rxrpl_primitives::AccountId::from_slice(bytes)
                        .map_err(|e| CodecError::InvalidAddress(e.to_string()))?;
                    let address = crate::address::encode_account_id(&account_id);
                    step.insert("account".to_string(), Value::String(address));
                }
                if type_byte & 0x10 != 0 {
                    let bytes = self.read_bytes(20)?;
                    step.insert(
                        "currency".to_string(),
                        Value::String(decode_currency_code(bytes)),
                    );
                }
                if type_byte & 0x20 != 0 {
                    let bytes = self.read_bytes(20)?;
                    let account_id = rxrpl_primitives::AccountId::from_slice(bytes)
                        .map_err(|e| CodecError::InvalidAddress(e.to_string()))?;
                    let address = crate::address::encode_account_id(&account_id);
                    step.insert("issuer".to_string(), Value::String(address));
                }

                current_path.push(Value::Object(step));
            }
        }

        Ok(Value::Array(paths))
    }

    pub(crate) fn parse_number(&mut self) -> Result<Value, CodecError> {
        let raw = self.read_u64()?;
        Ok(Value::String(decode_iou_value(raw)))
    }

    fn parse_issue(&mut self) -> Result<Value, CodecError> {
        let currency_bytes = self.read_bytes(20)?;
        let currency = decode_currency_code(currency_bytes);

        let mut map = Map::new();
        map.insert("currency".to_string(), Value::String(currency.clone()));

        // XRP issues don't have an issuer
        if currency != "XRP" && !currency_bytes.iter().all(|&b| b == 0) {
            let issuer_bytes = self.read_bytes(20)?;
            let account_id = rxrpl_primitives::AccountId::from_slice(issuer_bytes)
                .map_err(|e| CodecError::InvalidAddress(e.to_string()))?;
            let address = crate::address::encode_account_id(&account_id);
            map.insert("issuer".to_string(), Value::String(address));
        }

        Ok(Value::Object(map))
    }
}

fn decode_currency_code(bytes: &[u8]) -> String {
    if bytes.len() != 20 {
        return hex::encode_upper(bytes);
    }

    // Check if standard: bytes 0-11 and 15-19 are zero, 12-14 are ASCII
    let is_standard = bytes[..12].iter().all(|&b| b == 0)
        && bytes[15..].iter().all(|&b| b == 0)
        && bytes[12..15].iter().all(|&b| b.is_ascii_graphic());

    if is_standard && bytes[12..15] != [0, 0, 0] {
        String::from_utf8_lossy(&bytes[12..15]).to_string()
    } else if bytes.iter().all(|&b| b == 0) {
        "XRP".to_string()
    } else {
        hex::encode_upper(bytes)
    }
}

fn decode_iou_value(raw: u64) -> String {
    // bit 63: not XRP (always 1)
    // bit 62: positive (1) or negative (0)
    // bits 54-61: exponent + 97
    // bits 0-53: mantissa
    let positive = raw & 0x4000_0000_0000_0000 != 0;
    let exponent = ((raw >> 54) & 0xFF) as i32 - 97;
    let mantissa = raw & 0x003F_FFFF_FFFF_FFFF;

    if mantissa == 0 {
        return "0".to_string();
    }

    let sign = if positive { "" } else { "-" };

    // Convert mantissa and exponent to decimal string
    if exponent == 0 {
        format!("{sign}{mantissa}")
    } else if exponent > 0 {
        let mut s = mantissa.to_string();
        for _ in 0..exponent {
            s.push('0');
        }
        format!("{sign}{s}")
    } else {
        let s = mantissa.to_string();
        let decimal_pos = s.len() as i32 + exponent;
        if decimal_pos <= 0 {
            let zeros = (-decimal_pos) as usize;
            format!("{sign}0.{}{s}", "0".repeat(zeros))
        } else {
            let pos = decimal_pos as usize;
            let (integer, decimal) = s.split_at(pos);
            if decimal.is_empty() {
                format!("{sign}{integer}")
            } else {
                format!("{sign}{integer}.{decimal}")
            }
        }
    }
}
