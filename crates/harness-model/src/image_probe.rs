use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// A local diagnostic image with shuffled quadrants. Never a screenshot of user data.
pub(crate) struct ImageProbe {
    pub data_url: String,
    expected: Value,
}

impl ImageProbe {
    pub fn new(seed: u128) -> Self {
        let mut colors = [
            ("red", [255, 0, 0]),
            ("green", [0, 255, 0]),
            ("blue", [0, 0, 255]),
            ("yellow", [255, 255, 0]),
        ];
        let mut value = (seed as u64) ^ ((seed >> 64) as u64);
        for i in (1..4).rev() {
            value = value
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            colors.swap(i, ((value >> 32) as usize) % (i + 1));
        }
        let width = 128u32;
        let height = 128u32;
        let mut pixels = Vec::new();
        for y in 0..height {
            pixels.push(0); // PNG filter: none.
            for x in 0..width {
                let quadrant = usize::from(y >= height / 2) * 2 + usize::from(x >= width / 2);
                pixels.extend_from_slice(&colors[quadrant].1);
            }
        }
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        let mut zlib = vec![0x78, 0x01];
        // This 49 KiB RGB image fits a single uncompressed DEFLATE block.
        zlib.push(1);
        let length = pixels.len() as u16;
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(&pixels);
        let (mut a, mut b) = (1u32, 0u32);
        for byte in &pixels {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
        chunk(&mut png, b"IDAT", &zlib);
        chunk(&mut png, b"IEND", &[]);
        Self {
            data_url: format!("data:image/png;base64,{}", base64(&png)),
            expected: json!({"topLeft": colors[0].0, "topRight": colors[1].0, "bottomLeft": colors[2].0, "bottomRight": colors[3].0}),
        }
    }

    pub fn instruction() -> &'static str {
        "Inspect the attached image. Return ONLY a JSON object naming the solid color in each quadrant, with keys topLeft, topRight, bottomLeft, bottomRight. Use lowercase English color names."
    }

    pub fn validate(&self, answer: &str) -> Result<()> {
        let trimmed = answer
            .trim()
            .strip_prefix("```json")
            .or_else(|| answer.trim().strip_prefix("```"))
            .unwrap_or(answer.trim())
            .trim()
            .trim_end_matches("```")
            .trim();
        let value: Value = serde_json::from_str(trimmed)
            .context("Vision probe did not return the requested JSON object")?;
        for key in ["topLeft", "topRight", "bottomLeft", "bottomRight"] {
            let observed = value
                .get(key)
                .and_then(Value::as_str)
                .map(|v| v.trim().to_ascii_lowercase());
            let expected = self.expected[key].as_str().unwrap_or("");
            if observed.as_deref() != Some(expected) {
                let reported = observed
                    .as_deref()
                    .unwrap_or("[missing]")
                    .chars()
                    .take(60)
                    .collect::<String>();
                bail!("Vision probe quadrant {key}: expected {expected}, received {reported}");
            }
        }
        Ok(())
    }
}

fn chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc = 0xffffffffu32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_probe_requires_observed_pixels() {
        let probe = ImageProbe::new(173);
        assert!(probe
            .data_url
            .starts_with("data:image/png;base64,iVBORw0KGgo"));
        assert!(probe.validate(&probe.expected.to_string()).is_ok());
        assert!(probe.validate("{\"topLeft\":\"red\"}").is_err());
        let error = probe
            .validate("{\"topLeft\":\"purple\"}")
            .unwrap_err()
            .to_string();
        assert!(error.contains("topLeft") && error.contains("purple"));
    }
    #[test]
    fn base64_handles_padding() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }
}
