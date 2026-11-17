//! Read-only Phone Link Start menu companion card; no notification content is retained.
use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use serde_json::Value;
use std::{collections::HashSet, fs::OpenOptions, io::Read, os::windows::fs::OpenOptionsExt, time::SystemTime};

#[derive(Default)]
struct PhoneLink {
    seen: HashSet<String>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(PhoneLink::default())
}
fn first_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match value {
        Value::Object(map) => {
            map.get(key).and_then(Value::as_str).or_else(|| map.values().find_map(|v| first_string(v, key)))
        }
        Value::Array(items) => items.iter().find_map(|v| first_string(v, key)),
        _ => None,
    }
}
fn battery(text: &str) -> Option<u8> {
    for (offset, _) in text.match_indices("Battery ") {
        let rest = text.get(offset + 8..)?;
        let digits = rest.bytes().take_while(u8::is_ascii_digit).take(3).count();
        if digits > 0 && rest.get(digits..)?.starts_with("% remaining") {
            if let Ok(p) = rest.get(..digits)?.parse::<u8>() {
                if p <= 100 {
                    return Some(p);
                }
            }
        }
    }
    None
}
fn parse(bytes: &[u8]) -> Option<(String, u8)> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let name = first_string(&value, "speak")?.strip_suffix(" - Connected to your PC")?;
    if name.is_empty() {
        return None;
    }
    fn find_battery(v: &Value) -> Option<u8> {
        match v {
            Value::String(s) => battery(s),
            Value::Object(m) => m.values().find_map(find_battery),
            Value::Array(a) => a.iter().find_map(find_battery),
            _ => None,
        }
    }
    Some((name.to_owned(), find_battery(&value)?))
}
impl Driver for PhoneLink {
    fn name(&self) -> &'static str {
        "Phone Link"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        let Some(base) = std::env::var_os("LOCALAPPDATA") else { return };
        if base.to_string_lossy().encode_utf16().count() >= 260 {
            return;
        }
        let path = std::path::PathBuf::from(base)
            .join("Packages/Microsoft.YourPhone_8wekyb3d8bbwe/LocalState/StartMenu/StartMenuCompanion.json");
        let Ok(mut file) = OpenOptions::new().read(true).share_mode(1 | 2 | 4).open(path) else { return };
        let Ok(meta) = file.metadata() else { return };
        if meta.len() == 0 || meta.len() >= 4 * 1024 * 1024 {
            return;
        }
        let Ok(written) = meta.modified() else { return };
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        if file.by_ref().take(4 * 1024 * 1024).read_to_end(&mut bytes).is_err() || bytes.len() as u64 != meta.len() {
            return;
        }
        let Some((name, p)) = parse(&bytes) else { return };
        let stale = SystemTime::now().duration_since(written).unwrap_or_default().as_secs() > 10800;
        let seen = !self.seen.insert(name.clone());
        let obs = if stale && seen {
            Observation::new("phonelink", State::Unreachable).name(name).kind(Kind::Phone)
        } else {
            Observation::battery("phonelink", name, Kind::Phone, p, false)
        };
        ctx.out.push(obs.via("Phone Link"));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_cards() {
        assert_eq!(
            parse(br#"{"speak":"Phone - Connected to your PC","body":[{"tooltip":"Battery 73% remaining"}]}"#),
            Some(("Phone".into(), 73))
        );
        assert_eq!(parse(br#"{"speak":"Phone - Disconnected","tooltip":"Battery 73% remaining"}"#), None);
        assert_eq!(battery("Battery 101% remaining Battery 0% remaining"), Some(0));
        assert_eq!(battery("Battery 1234% remaining"), None);
        assert_eq!(parse(b"{"), None);
    }
}
