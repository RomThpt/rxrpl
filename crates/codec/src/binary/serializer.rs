use serde_json::Value;

use super::definitions::{self, FieldDef};
use super::field;
use super::field_id;
use crate::error::CodecError;

/// Binary serializer for XRPL objects.
pub struct BinarySerializer {
    buf: Vec<u8>,
}

impl BinarySerializer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn write_u8(&mut self, val: u8) {
        self.buf.push(val);
    }

    fn write_u16(&mut self, val: u16) {
        self.buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_u32(&mut self, val: u32) {
        self.buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_u64(&mut self, val: u64) {
        self.buf.extend_from_slice(&val.to_be_bytes());
    }

    /// Write a variable-length prefix (VL encoding).
    fn write_vl_length(&mut self, len: usize) {
        if len <= 192 {
            self.write_u8(len as u8);
        } else if len <= 12480 {
            let adjusted = len - 193;
            self.write_u8((adjusted >> 8) as u8 + 193);
            self.write_u8((adjusted & 0xFF) as u8);
        } else if len <= 918744 {
            let adjusted = len - 12481;
            self.write_u8(241 + (adjusted >> 16) as u8);
            self.write_u8(((adjusted >> 8) & 0xFF) as u8);
            self.write_u8((adjusted & 0xFF) as u8);
        }
    }

    /// Serialize a JSON object in canonical field order.
    pub fn serialize_object(&mut self, json: &Value, include_all: bool) -> Result<(), CodecError> {
        let obj = json
            .as_object()
            .ok_or_else(|| CodecError::UnsupportedType("expected JSON object".to_string()))?;

        // Collect field names and sort canonically
        let mut field_names: Vec<String> = obj.keys().cloned().collect();
        field::sort_fields_canonical(&mut field_names);

        for name in &field_names {
            let Some(def) = definitions::get_field(name) else {
                continue; // Skip unknown fields
            };
            if !field::should_serialize(def, include_all) {
                continue;
            }
            let value = &obj[name];
            self.serialize_field(def, value)?;
        }

        Ok(())
    }

    fn serialize_field(&mut self, def: &FieldDef, value: &Value) -> Result<(), CodecError> {
        // Write field ID
        let id_bytes = field_id::encode_field_id(def.type_code, def.nth);
        self.write_bytes(&id_bytes);

        // Serialize the value based on the field type
        match def.field_type.as_str() {
            "UInt8" => self.serialize_uint8(value)?,
            "UInt16" => self.serialize_uint16(value, def)?,
            "UInt32" => self.serialize_uint32(value)?,
            "Int32" => self.serialize_int32(value)?,
            "UInt64" => self.serialize_uint64(value)?,
            "Hash128" => self.serialize_hash(value, 16)?,
            "Hash160" => self.serialize_hash(value, 20)?,
            "Hash192" => self.serialize_hash(value, 24)?,
            "Hash256" => self.serialize_hash(value, 32)?,
            "Amount" => self.serialize_amount(value)?,
            "Blob" => self.serialize_blob(value)?,
            "AccountID" => self.serialize_account_id(value)?,
            "STObject" => {
                self.serialize_object(value, true)?;
                // Object end marker: type=14(STObject), field=1(ObjectEndMarker)
                let end = field_id::encode_field_id(14, 1);
                self.write_bytes(&end);
            }
            "STArray" => {
                self.serialize_array(value)?;
                // Array end marker: type=15(STArray), field=1(ArrayEndMarker)
                let end = field_id::encode_field_id(15, 1);
                self.write_bytes(&end);
            }
            "Vector256" => self.serialize_vector256(value)?,
            "PathSet" => self.serialize_pathset(value)?,
            "Issue" => self.serialize_issue(value)?,
            "Currency" => self.serialize_currency(value)?,
            "XChainBridge" => {
                self.serialize_object(value, true)?;
                let end = field_id::encode_field_id(14, 1);
                self.write_bytes(&end);
            }
            "Number" => self.serialize_number(value)?,
            other => {
                return Err(CodecError::UnsupportedType(format!(
                    "unsupported field type: {other}"
                )));
            }
        }

        Ok(())
    }

    fn serialize_uint8(&mut self, value: &Value) -> Result<(), CodecError> {
        let v = value
            .as_u64()
            .ok_or_else(|| CodecError::UnsupportedType("expected u8 integer".to_string()))?;
        self.write_u8(v as u8);
        Ok(())
    }

    fn serialize_uint16(&mut self, value: &Value, def: &FieldDef) -> Result<(), CodecError> {
        // TransactionType and LedgerEntryType can be string names
        let v = if let Some(s) = value.as_str() {
            match def.name.as_str() {
                "TransactionType" => definitions::get_transaction_type_code(s)
                    .ok_or_else(|| CodecError::UnknownField(s.to_string()))?
                    as u16,
                "LedgerEntryType" => definitions::get_ledger_entry_type_code(s)
                    .ok_or_else(|| CodecError::UnknownField(s.to_string()))?
                    as u16,
                _ => return Err(CodecError::UnsupportedType("expected u16".to_string())),
            }
        } else {
            value
                .as_u64()
                .ok_or_else(|| CodecError::UnsupportedType("expected u16 integer".to_string()))?
                as u16
        };
        self.write_u16(v);
        Ok(())
    }

    fn serialize_uint32(&mut self, value: &Value) -> Result<(), CodecError> {
        let v = value
            .as_u64()
            .ok_or_else(|| CodecError::UnsupportedType("expected u32 integer".to_string()))?;
        self.write_u32(v as u32);
        Ok(())
    }

    fn serialize_int32(&mut self, value: &Value) -> Result<(), CodecError> {
        let v = value
            .as_i64()
            .ok_or_else(|| CodecError::UnsupportedType("expected i32 integer".to_string()))?;
        self.write_u32(v as i32 as u32);
        Ok(())
    }

    fn serialize_uint64(&mut self, value: &Value) -> Result<(), CodecError> {
        // UInt64 in XRPL JSON is typically a hex string
        let v = if let Some(s) = value.as_str() {
            u64::from_str_radix(s, 16)
                .map_err(|_| CodecError::UnsupportedType("invalid u64 hex string".to_string()))?
        } else {
            value
                .as_u64()
                .ok_or_else(|| CodecError::UnsupportedType("expected u64".to_string()))?
        };
        self.write_u64(v);
        Ok(())
    }

    fn serialize_hash(&mut self, value: &Value, len: usize) -> Result<(), CodecError> {
        let hex_str = value.as_str().ok_or_else(|| {
            CodecError::UnsupportedType("expected hex string for hash".to_string())
        })?;
        let bytes = hex::decode(hex_str).map_err(|e| CodecError::Hex(e.to_string()))?;
        if bytes.len() != len {
            return Err(CodecError::InvalidLength {
                expected: len,
                got: bytes.len(),
            });
        }
        self.write_bytes(&bytes);
        Ok(())
    }

    fn serialize_amount(&mut self, value: &Value) -> Result<(), CodecError> {
        if let Some(s) = value.as_str() {
            // XRP amount (drops as string)
            let drops: i64 = s
                .parse()
                .map_err(|_| CodecError::UnsupportedType("invalid XRP drops amount".to_string()))?;
            // XRP amounts: positive bit set (bit 62), then the amount
            let serialized = if drops >= 0 {
                (drops as u64) | 0x4000_0000_0000_0000
            } else {
                ((-drops) as u64) & !0x4000_0000_0000_0000
            };
            self.write_u64(serialized);
        } else if let Some(obj) = value.as_object() {
            // IOU amount
            let value_str = obj.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                CodecError::UnsupportedType("IOU amount missing value".to_string())
            })?;
            let currency_str = obj
                .get("currency")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    CodecError::UnsupportedType("IOU amount missing currency".to_string())
                })?;
            let issuer_str = obj.get("issuer").and_then(|v| v.as_str()).ok_or_else(|| {
                CodecError::UnsupportedType("IOU amount missing issuer".to_string())
            })?;

            // Serialize IOU amount (8 bytes) + currency (20 bytes) + issuer (20 bytes) = 48 bytes
            self.serialize_iou_value(value_str)?;
            self.serialize_currency_code(currency_str)?;
            self.serialize_account_id_raw(issuer_str)?;
        } else {
            return Err(CodecError::UnsupportedType(
                "amount must be string or object".to_string(),
            ));
        }
        Ok(())
    }

    fn serialize_iou_value(&mut self, value_str: &str) -> Result<(), CodecError> {
        if value_str == "0" || value_str == "0.0" {
            // Zero IOU amount: special encoding
            self.write_u64(0x8000_0000_0000_0000);
            return Ok(());
        }

        let negative = value_str.starts_with('-');
        let abs_str = if negative { &value_str[1..] } else { value_str };

        // Parse into mantissa and exponent
        let (mantissa, exponent) = parse_decimal(abs_str)?;

        // XRPL IOU encoding:
        // bit 63: not XRP (always 1 for IOU)
        // bit 62: positive (1) or negative (0)
        // bits 54-61: exponent + 97 (8 bits)
        // bits 0-53: mantissa (54 bits)
        let sign_bit: u64 = if negative { 0 } else { 1 << 62 };
        let exp_bits = ((exponent + 97) as u64 & 0xFF) << 54;
        let mantissa_bits = mantissa & 0x003F_FFFF_FFFF_FFFF;

        let serialized = 0x8000_0000_0000_0000 | sign_bit | exp_bits | mantissa_bits;
        self.write_u64(serialized);
        Ok(())
    }

    fn serialize_currency_code(&mut self, currency: &str) -> Result<(), CodecError> {
        if currency.len() == 3 {
            // Standard 3-char currency code
            let mut bytes = [0u8; 20];
            bytes[12] = currency.as_bytes()[0];
            bytes[13] = currency.as_bytes()[1];
            bytes[14] = currency.as_bytes()[2];
            self.write_bytes(&bytes);
        } else if currency.len() == 40 {
            // Non-standard 20-byte hex currency
            let bytes = hex::decode(currency).map_err(|e| CodecError::Hex(e.to_string()))?;
            self.write_bytes(&bytes);
        } else {
            return Err(CodecError::UnsupportedType(format!(
                "invalid currency code: {currency}"
            )));
        }
        Ok(())
    }

    fn serialize_account_id_raw(&mut self, account: &str) -> Result<(), CodecError> {
        // Can be a classic address or hex
        if account.len() == 40 {
            // Hex account ID
            let bytes = hex::decode(account).map_err(|e| CodecError::Hex(e.to_string()))?;
            self.write_bytes(&bytes);
        } else {
            // Classic address -- decode it
            let account_id = crate::address::decode_account_id(account)?;
            self.write_bytes(account_id.as_bytes());
        }
        Ok(())
    }

    fn serialize_blob(&mut self, value: &Value) -> Result<(), CodecError> {
        let hex_str = value.as_str().ok_or_else(|| {
            CodecError::UnsupportedType("expected hex string for blob".to_string())
        })?;
        let bytes = hex::decode(hex_str).map_err(|e| CodecError::Hex(e.to_string()))?;
        self.write_vl_length(bytes.len());
        self.write_bytes(&bytes);
        Ok(())
    }

    fn serialize_account_id(&mut self, value: &Value) -> Result<(), CodecError> {
        let s = value.as_str().ok_or_else(|| {
            CodecError::UnsupportedType("expected string for AccountID".to_string())
        })?;

        let account_bytes = if s.len() == 40 {
            hex::decode(s).map_err(|e| CodecError::Hex(e.to_string()))?
        } else {
            let account_id = crate::address::decode_account_id(s)?;
            account_id.as_bytes().to_vec()
        };

        // AccountID is VL-encoded
        self.write_vl_length(account_bytes.len());
        self.write_bytes(&account_bytes);
        Ok(())
    }

    fn serialize_array(&mut self, value: &Value) -> Result<(), CodecError> {
        let arr = value
            .as_array()
            .ok_or_else(|| CodecError::UnsupportedType("expected JSON array".to_string()))?;

        for item in arr {
            // Each array element is a wrapper object with one key
            if let Some(obj) = item.as_object() {
                for (key, val) in obj {
                    if let Some(def) = definitions::get_field(key) {
                        let id_bytes = field_id::encode_field_id(def.type_code, def.nth);
                        self.write_bytes(&id_bytes);
                        self.serialize_object(val, true)?;
                        // Object end marker
                        let end = field_id::encode_field_id(14, 1);
                        self.write_bytes(&end);
                    }
                }
            }
        }

        Ok(())
    }

    fn serialize_vector256(&mut self, value: &Value) -> Result<(), CodecError> {
        let arr = value.as_array().ok_or_else(|| {
            CodecError::UnsupportedType("expected array for Vector256".to_string())
        })?;

        let total_len = arr.len() * 32;
        self.write_vl_length(total_len);

        for item in arr {
            let hex_str = item.as_str().ok_or_else(|| {
                CodecError::UnsupportedType("expected hex string in Vector256".to_string())
            })?;
            let bytes = hex::decode(hex_str).map_err(|e| CodecError::Hex(e.to_string()))?;
            if bytes.len() != 32 {
                return Err(CodecError::InvalidLength {
                    expected: 32,
                    got: bytes.len(),
                });
            }
            self.write_bytes(&bytes);
        }

        Ok(())
    }

    fn serialize_pathset(&mut self, value: &Value) -> Result<(), CodecError> {
        let paths = value
            .as_array()
            .ok_or_else(|| CodecError::UnsupportedType("expected array for PathSet".to_string()))?;

        for (i, path) in paths.iter().enumerate() {
            let steps = path.as_array().ok_or_else(|| {
                CodecError::UnsupportedType("expected array for path".to_string())
            })?;

            for step in steps {
                let obj = step.as_object().ok_or_else(|| {
                    CodecError::UnsupportedType("expected object for path step".to_string())
                })?;

                let mut type_byte: u8 = 0;
                if obj.contains_key("account") {
                    type_byte |= 0x01;
                }
                if obj.contains_key("currency") {
                    type_byte |= 0x10;
                }
                if obj.contains_key("issuer") {
                    type_byte |= 0x20;
                }

                self.write_u8(type_byte);

                if let Some(account) = obj.get("account").and_then(|v| v.as_str()) {
                    self.serialize_account_id_raw(account)?;
                }
                if let Some(currency) = obj.get("currency").and_then(|v| v.as_str()) {
                    self.serialize_currency_code(currency)?;
                }
                if let Some(issuer) = obj.get("issuer").and_then(|v| v.as_str()) {
                    self.serialize_account_id_raw(issuer)?;
                }
            }

            if i < paths.len() - 1 {
                self.write_u8(0xFF); // Path separator
            }
        }
        self.write_u8(0x00); // PathSet end
        Ok(())
    }

    fn serialize_issue(&mut self, value: &Value) -> Result<(), CodecError> {
        let obj = value
            .as_object()
            .ok_or_else(|| CodecError::UnsupportedType("expected object for Issue".to_string()))?;

        let currency = obj
            .get("currency")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CodecError::UnsupportedType("Issue missing currency".to_string()))?;

        self.serialize_currency_code(currency)?;

        if currency != "XRP" {
            if let Some(issuer) = obj.get("issuer").and_then(|v| v.as_str()) {
                self.serialize_account_id_raw(issuer)?;
            }
        }

        Ok(())
    }

    fn serialize_number(&mut self, value: &Value) -> Result<(), CodecError> {
        let value_str = value
            .as_str()
            .ok_or_else(|| CodecError::UnsupportedType("expected string for Number".to_string()))?;
        self.serialize_iou_value(value_str)
    }

    fn serialize_currency(&mut self, value: &Value) -> Result<(), CodecError> {
        let s = value.as_str().ok_or_else(|| {
            CodecError::UnsupportedType("expected string for Currency".to_string())
        })?;
        self.serialize_currency_code(s)
    }
}

/// Parse a decimal string into (mantissa, exponent) where value = mantissa * 10^exponent.
/// The mantissa has up to 16 significant digits.
fn parse_decimal(s: &str) -> Result<(u64, i32), CodecError> {
    let parts: Vec<&str> = s.split('.').collect();
    let (integer_part, decimal_part) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err(CodecError::UnsupportedType(format!("invalid decimal: {s}"))),
    };

    // Check for scientific notation
    if s.contains('e') || s.contains('E') {
        let val: f64 = s
            .parse()
            .map_err(|_| CodecError::UnsupportedType(format!("invalid decimal: {s}")))?;
        if val == 0.0 {
            return Ok((0, 0));
        }
        let abs_val = val.abs();
        let mut exponent = (abs_val.log10().floor()) as i32 - 15;
        let mut mantissa = (abs_val / 10f64.powi(exponent)).round() as u64;

        // Normalize: mantissa should be between 10^15 and 10^16-1
        while mantissa < 1_000_000_000_000_000 && mantissa > 0 {
            mantissa *= 10;
            exponent -= 1;
        }
        while mantissa >= 10_000_000_000_000_000 {
            mantissa /= 10;
            exponent += 1;
        }

        return Ok((mantissa, exponent));
    }

    // Combine into a single integer string
    let combined = format!("{integer_part}{decimal_part}");
    let trimmed = combined.trim_start_matches('0');
    if trimmed.is_empty() {
        return Ok((0, 0));
    }

    let mantissa: u64 = trimmed
        .parse()
        .map_err(|_| CodecError::UnsupportedType(format!("invalid mantissa: {trimmed}")))?;

    let exponent = -(decimal_part.len() as i32);

    // Normalize mantissa to 16 significant digits
    let mut m = mantissa;
    let mut e = exponent;
    while m < 1_000_000_000_000_000 && m > 0 {
        m *= 10;
        e -= 1;
    }
    while m >= 10_000_000_000_000_000 {
        m /= 10;
        e += 1;
    }

    Ok((m, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decimal_integer() {
        let (m, e) = parse_decimal("100").unwrap();
        // 100 normalized: mantissa=1_000_000_000_000_000, exponent=-13
        assert!(m >= 1_000_000_000_000_000);
        assert!(m < 10_000_000_000_000_000);
        // Verify: m * 10^e == 100
        assert_eq!(e, -13);
        assert_eq!(m, 1_000_000_000_000_000);
    }

    #[test]
    fn parse_decimal_with_fraction() {
        let (m, _e) = parse_decimal("100.50").unwrap();
        assert!(m >= 1_000_000_000_000_000);
        assert!(m < 10_000_000_000_000_000);
    }

    #[test]
    fn serialize_xrp_amount() {
        let mut s = BinarySerializer::new();
        s.serialize_amount(&Value::String("1000000".to_string()))
            .unwrap();
        let bytes = s.into_bytes();
        assert_eq!(bytes.len(), 8);
        // Should have the positive bit set
        assert!(bytes[0] & 0x40 != 0);
    }

    #[test]
    fn serialize_number_zero() {
        let mut s = BinarySerializer::new();
        s.serialize_number(&Value::String("0".to_string())).unwrap();
        let bytes = s.into_bytes();
        assert_eq!(bytes.len(), 8);
        // Zero IOU encoding
        assert_eq!(
            u64::from_be_bytes(bytes.try_into().unwrap()),
            0x8000_0000_0000_0000
        );
    }

    #[test]
    fn number_roundtrip() {
        use super::super::parser::BinaryParser;

        for value_str in &["1.5", "0", "-42.7", "1000000", "-0.001"] {
            let mut s = BinarySerializer::new();
            s.serialize_number(&Value::String(value_str.to_string()))
                .unwrap();
            let bytes = s.into_bytes();

            let mut p = BinaryParser::new(&bytes);
            let parsed = p.parse_number().unwrap();
            let parsed_str = parsed.as_str().unwrap();

            // Verify round-trip by comparing numeric values
            let original: f64 = value_str.parse().unwrap();
            let roundtripped: f64 = parsed_str.parse().unwrap();
            assert!(
                (original - roundtripped).abs() < 1e-10,
                "Number round-trip failed for {value_str}: got {parsed_str}"
            );
        }
    }

    #[test]
    fn xchain_bridge_roundtrip() {
        use super::super::parser::BinaryParser;

        let bridge = serde_json::json!({
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "IssuingChainDoor": "r9cZA1mLK5R5Am25ArfXFmqgNwjZgnfk59",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainIssue": {"currency": "XRP"}
        });

        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "XChainBridge": bridge,
            "SignatureReward": "100",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut s = BinarySerializer::new();
        s.serialize_object(&tx, false).unwrap();
        let bytes = s.into_bytes();

        let mut p = BinaryParser::new(&bytes);
        let parsed = p.parse_object().unwrap();

        assert_eq!(
            parsed["XChainBridge"]["LockingChainDoor"],
            tx["XChainBridge"]["LockingChainDoor"]
        );
        assert_eq!(
            parsed["XChainBridge"]["IssuingChainDoor"],
            tx["XChainBridge"]["IssuingChainDoor"]
        );
    }
}
