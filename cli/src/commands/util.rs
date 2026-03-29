pub fn decode_field(item: &serde_json::Value, key: &str) -> String {
    item["data"][key]
        .as_str()
        .and_then(base64_decode)
        .unwrap_or_default()
}

fn base64_decode(input: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for c in input.bytes() {
        let val = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn parse_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).map(|s| s.as_str()))
}

pub fn delete_secret(prefix: &str, name: &str, resource_label: &str) -> Result<(), String> {
    use crate::runner::run_check;
    let secret_name = format!("{prefix}{name}");
    match run_check("kubectl", &["delete", "secret", &secret_name]) {
        Ok(()) => {
            eprintln!("{resource_label} '{name}' deleted.");
            Ok(())
        }
        Err(e) if e.contains("NotFound") || e.contains("not found") => {
            eprintln!("{resource_label} '{name}' not found.");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_field_known_value() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"data":{"provider":"YW50aHJvcGlj"}}"#).unwrap();
        assert_eq!(decode_field(&json, "provider"), "anthropic");
    }

    #[test]
    fn decode_field_with_padding() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"data":{"model":"Y2xhdWRl"}}"#).unwrap();
        assert_eq!(decode_field(&json, "model"), "claude");
    }

    #[test]
    fn decode_field_missing_key() {
        let json: serde_json::Value = serde_json::from_str(r#"{"data":{}}"#).unwrap();
        assert_eq!(decode_field(&json, "nope"), "");
    }

    #[test]
    fn decode_field_invalid_base64() {
        let json: serde_json::Value = serde_json::from_str(r#"{"data":{"bad":"!!!"}}"#).unwrap();
        assert_eq!(decode_field(&json, "bad"), "");
    }

    #[test]
    fn parse_flag_present() {
        let args: Vec<String> = vec!["--url", "https://example.com"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(parse_flag(&args, "--url"), Some("https://example.com"));
    }

    #[test]
    fn parse_flag_missing() {
        let args: Vec<String> = vec!["--url", "https://example.com"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(parse_flag(&args, "--tools"), None);
    }

    #[test]
    fn parse_flag_no_value_after() {
        let args: Vec<String> = vec!["--url"].into_iter().map(String::from).collect();
        assert_eq!(parse_flag(&args, "--url"), None);
    }
}
