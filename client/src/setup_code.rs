use anyhow::{anyhow, Context, Result};

pub fn parse_setup_code(input: &str) -> Result<u32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("setup code is empty"));
    }

    if let Some(qr_payload) = extract_qr_payload(trimmed) {
        return decode_qr_payload_passcode(&qr_payload);
    }

    let numeric = trimmed.replace('-', "").replace(' ', "");
    if numeric.len() == 8 && numeric.chars().all(|ch| ch.is_ascii_digit()) {
        return numeric
            .parse::<u32>()
            .context("invalid 8-digit setup passcode");
    }

    if (numeric.len() == 11 || numeric.len() == 21)
        && numeric.chars().all(|ch| ch.is_ascii_digit())
    {
        return matc::onboarding::decode_manual_pairing_code(&numeric)
            .map(|info| info.passcode)
            .context("invalid manual pairing code");
    }

    Err(anyhow!(
        "unsupported setup code format; use an 8-digit passcode, manual pairing code, or MT: QR payload"
    ))
}

fn extract_qr_payload(input: &str) -> Option<String> {
    if input.starts_with("MT:") {
        return Some(input.to_string());
    }

    if let Some(pos) = input.find("MT%3A") {
        let encoded = &input[pos..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        return Some(encoded[..end].replacen("MT%3A", "MT:", 1));
    }

    if let Some(pos) = input.find("MT:") {
        let tail = &input[pos..];
        let end = tail.find('&').unwrap_or(tail.len());
        return Some(tail[..end].to_string());
    }

    None
}

fn decode_qr_payload_passcode(payload: &str) -> Result<u32> {
    let encoded = payload
        .strip_prefix("MT:")
        .context("QR payload must start with MT:")?;
    let bytes = base38_decode(encoded)?;
    if bytes.len() < 11 {
        return Err(anyhow!("QR payload is too short"));
    }

    let packed = bytes
        .iter()
        .take(16)
        .enumerate()
        .fold(0u128, |acc, (idx, byte)| acc | ((*byte as u128) << (idx * 8)));

    Ok(extract_bits(packed, 57, 27) as u32)
}

fn extract_bits(value: u128, offset: u32, width: u32) -> u128 {
    (value >> offset) & ((1u128 << width) - 1)
}

fn base38_decode(input: &str) -> Result<Vec<u8>> {
    const ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-.";

    let chars = input.as_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        let remaining = chars.len() - index;
        let (chunk_len, output_len) = if remaining >= 5 {
            (5, 3)
        } else if remaining == 4 {
            (4, 2)
        } else if remaining == 2 {
            (2, 1)
        } else {
            return Err(anyhow!("invalid base38 payload length"));
        };

        let mut value = 0u32;
        let mut factor = 1u32;
        for ch in &chars[index..index + chunk_len] {
            let chr = *ch as char;
            let digit = ALPHABET
                .find(chr)
                .with_context(|| format!("invalid base38 character: {chr}"))? as u32;
            value = value
                .checked_add(digit.saturating_mul(factor))
                .context("base38 overflow")?;
            factor = factor.checked_mul(38).context("base38 overflow")?;
        }

        for _ in 0..output_len {
            out.push((value & 0xFF) as u8);
            value >>= 8;
        }

        index += chunk_len;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{base38_decode, decode_qr_payload_passcode, parse_setup_code};

    #[test]
    fn parses_raw_passcode() {
        assert_eq!(parse_setup_code("20202021").unwrap(), 20202021);
    }

    #[test]
    fn parses_manual_pairing_code() {
        assert_eq!(parse_setup_code("34970112332").unwrap(), 20202021);
        assert_eq!(parse_setup_code("2585-103-3238").unwrap(), 54453390);
    }

    #[test]
    fn parses_qr_payload() {
        assert_eq!(
            decode_qr_payload_passcode("MT:Y.K9042C00KA0648G00").unwrap(),
            20202021
        );
    }

    #[test]
    fn parses_qr_payload_from_url() {
        assert_eq!(
            parse_setup_code(
                "https://project-chip.github.io/connectedhomeip/qrcode.html?data=MT%3AY.K9042C00KA0648G00"
            )
            .unwrap(),
            20202021
        );
    }

    #[test]
    fn decodes_base38_known_payload() {
        let bytes = base38_decode("Y.K9042C00KA0648G00").unwrap();
        assert!(bytes.len() >= 11);
    }
}
