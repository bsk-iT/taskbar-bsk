//! Minimal HID++ transport for Logitech devices.
//!
//! Message layout: [report_id, device_index, feature_index, (function << 4) | sw_id, params...]
//!
//! On Windows each report size is a separate HID collection, so we write long reports to the
//! long collection but also listen on the short one: receivers answer some errors (e.g. "no
//! device in this slot") with short HID++ 1.0 reports.

use crate::hid::{self, HidColl, HidDev};
use std::fmt;
use std::time::{Duration, Instant};

pub const LOGITECH_VID: u16 = 0x046d;
const REPORT_SHORT: u8 = 0x10;
const REPORT_LONG: u8 = 0x11;
const SW_ID: u8 = 0x0d;
pub const ERR_UNKNOWN_DEVICE: u8 = 0x08;

#[derive(Debug)]
pub enum Error {
    Io,
    Timeout,
    Protocol(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io => write!(f, "HID I/O failed"),
            Self::Timeout => write!(f, "timeout"),
            Self::Protocol(c) => write!(f, "hid++ error 0x{c:02X}"),
        }
    }
}

pub struct Endpoint {
    pub pid: u16,
    pub iface: i32,
    pub product: String,
    pub suffix: String,
    pub(super) short: Option<HidColl>,
    pub(super) long: Option<HidColl>,
}

// Collections differ in &colNN and the final instance suffix, not physical device identity.
fn physical_key(c: &HidColl) -> String {
    let path = String::from_utf16_lossy(&c.path).to_ascii_lowercase();
    let mut parts = path.split('#');
    let (Some(_), Some(hw), Some(inst), Some(_)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return String::new();
    };
    let mut hw = hw.to_owned();
    if let Some(col) = hw.find("&col") {
        let end = hw.get(col..).and_then(|s| s.char_indices().nth(6)).map_or(hw.len(), |(offset, _)| col + offset);
        hw.replace_range(col..end, "");
    }
    let inst = inst.rsplit_once('&').map_or(inst, |(head, _)| head);
    format!("{hw}#{inst}")
}

pub fn endpoints(colls: &[HidColl]) -> Vec<Endpoint> {
    let mut endpoints = group_collections(
        colls
            .iter()
            .filter(|c| {
                c.vid == LOGITECH_VID
                    && ((c.page == 0xff00 && ((c.usage == 1 && c.out_len == 7) || (c.usage == 2 && c.out_len == 20)))
                        || (c.page == 0xff43 && c.usage == 0x0202 && c.out_len == 20))
            })
            .cloned(),
    );
    for ep in &mut endpoints {
        if let Some(c) = ep.long.as_ref().or(ep.short.as_ref()) {
            ep.product = hid::product(c);
        }
    }
    endpoints
}

fn group_collections(collections: impl IntoIterator<Item = HidColl>) -> Vec<Endpoint> {
    let mut groups: Vec<(String, Endpoint)> = Vec::new();
    for c in collections {
        if c.vid != LOGITECH_VID {
            continue;
        }
        let short = c.page == 0xff00 && c.usage == 1 && c.out_len == 7;
        let long = ((c.page == 0xff00 && c.usage == 2) || (c.page == 0xff43 && c.usage == 0x0202)) && c.out_len == 20;
        if !short && !long {
            continue;
        }
        let key = physical_key(&c);
        let pos = groups.iter().position(|(k, ep)| ep.pid == c.pid && ep.iface == c.iface && *k == key);
        let i = match pos {
            Some(i) => i,
            None => {
                let number = groups.iter().filter(|(_, ep)| ep.pid == c.pid).count() + 1;
                groups.push((
                    key,
                    Endpoint {
                        pid: c.pid,
                        iface: c.iface,
                        product: String::new(),
                        suffix: if number == 1 { String::new() } else { format!("#{number}") },
                        short: None,
                        long: None,
                    },
                ));
                groups.len() - 1
            }
        };
        if short {
            groups[i].1.short = Some(c);
        } else {
            groups[i].1.long = Some(c);
        }
    }
    groups.into_iter().map(|(_, ep)| ep).collect()
}

pub trait Hidpp {
    fn request(&self, index: u8, feature_idx: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error>;

    fn payload_capacity(&self) -> usize {
        16
    }

    fn raw_request(
        &self,
        index: u8,
        b2: u8,
        b3: u8,
        params: &[u8],
        short: bool,
        timeout_ms: u32,
    ) -> Result<[u8; 16], Error>;

    fn feature_index(&self, index: u8, feature: u16) -> Result<Option<u8>, Error> {
        let r = self.request(index, 0, 0, &feature.to_be_bytes())?;
        Ok((r[0] != 0).then_some(r[0]))
    }
}

pub trait Transport {
    fn write(&self, data: &[u8]) -> Result<usize, Error>;
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, Error>;
}

impl Transport for HidDev {
    fn write(&self, data: &[u8]) -> Result<usize, Error> {
        if HidDev::write(self, data, 500) {
            Ok(data.len())
        } else {
            Err(Error::Io)
        }
    }
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, Error> {
        let Some(reply) = self.read(timeout_ms.max(0) as u32) else { return Ok(0) };
        let n = buf.len().min(reply.len());
        buf[..n].copy_from_slice(&reply[..n]);
        Ok(n)
    }
}

pub struct Link<T: Transport = HidDev> {
    short: Option<T>,
    long: Option<T>,
}

impl Link {
    pub fn open(ep: &Endpoint) -> Result<Self, Error> {
        let short = ep.short.as_ref().and_then(HidDev::open);
        let long = ep.long.as_ref().and_then(HidDev::open);
        if short.is_none() && long.is_none() {
            return Err(Error::Io);
        }
        Ok(Self { short, long })
    }
}

impl<T: Transport> Hidpp for Link<T> {
    fn payload_capacity(&self) -> usize {
        if self.long.is_some() {
            16
        } else {
            3
        }
    }

    fn request(&self, index: u8, feature_idx: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
        self.raw_request(index, feature_idx, (function << 4) | SW_ID, params, false, 500)
    }

    fn raw_request(
        &self,
        index: u8,
        b2: u8,
        b3: u8,
        params: &[u8],
        force_short: bool,
        timeout_ms: u32,
    ) -> Result<[u8; 16], Error> {
        let use_long = self.long.is_some() && !force_short;
        let dev = if use_long { self.long.as_ref() } else { self.short.as_ref() }.ok_or(Error::Io)?;
        let mut tx = [0u8; 20];
        let len = if use_long { 20 } else { 7 };
        tx[..4].copy_from_slice(&[if use_long { REPORT_LONG } else { REPORT_SHORT }, index, b2, b3]);
        for (dst, src) in tx[4..len].iter_mut().zip(params) {
            *dst = *src;
        }
        let mut rx = [0u8; 64];
        // Root replies do not echo the queried feature, so discard stale lookups first.
        for d in self.long.iter().chain(self.short.iter()) {
            for _ in 0..64 {
                if d.read_timeout(&mut rx, 0)? == 0 {
                    break;
                }
            }
        }
        dev.write(&tx[..len])?;
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        while Instant::now() < deadline {
            for d in self.long.iter().chain(self.short.iter()) {
                let remaining = deadline.saturating_duration_since(Instant::now()).as_millis().min(15) as i32;
                if remaining == 0 {
                    break;
                }
                let n = d.read_timeout(&mut rx, remaining)?.min(rx.len());
                if let Some(result) = reply(&rx[..n], index, b2, b3) {
                    return result;
                }
            }
        }
        Err(Error::Timeout)
    }
}

fn reply(rx: &[u8], index: u8, b2: u8, b3: u8) -> Option<Result<[u8; 16], Error>> {
    let &[report, device, sub, function, ..] = rx else { return None };
    if rx.len() < 7 || !matches!(report, REPORT_SHORT | REPORT_LONG) || device != index {
        return None;
    }
    if matches!(sub, 0xff | 0x8f) && function == b2 && rx.get(4) == Some(&b3) {
        return Some(Err(Error::Protocol(*rx.get(5)?)));
    }
    if sub == b2 && function == b3 {
        let mut out = [0; 16];
        for (dst, src) in out.iter_mut().zip(rx.get(4..)?) {
            *dst = *src;
        }
        return Some(Ok(out));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(pid: u16, iface: i32, page: u16, usage: u16, path: &str) -> HidColl {
        HidColl {
            vid: LOGITECH_VID,
            pid,
            iface,
            page,
            usage,
            path: path.encode_utf16().chain([0]).collect(),
            in_len: 20,
            out_len: if usage == 1 { 7 } else { 20 },
            feat_len: 0,
        }
    }

    // G733 dongle as documented by HeadsetControl (usage page 0xFF43, usage 0x0202 for HID++
    // long reports). The exact collection list is not captured from a real device.
    #[test]
    fn g733_long_collection_alone_is_an_endpoint() {
        for pid in [0x0AB5, 0x0AFE, 0x0B1F] {
            let eps = group_collections([col(pid, 3, 0xFF43, 0x0202, "long")]);
            assert_eq!(eps.len(), 1, "pid {pid:04x}");
            assert_eq!((eps[0].pid, eps[0].iface), (pid, 3));
            assert!(eps[0].short.is_none());
        }
    }

    #[test]
    fn short_and_long_collections_are_paired() {
        let eps = group_collections([col(0xC52B, 2, 0xFF00, 0x0001, "short"), col(0xC52B, 2, 0xFF00, 0x0002, "long")]);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].short.as_ref().map(|c| String::from_utf16_lossy(&c.path)), Some("short\0".into()));
        assert_eq!(eps[0].long.as_ref().map(|c| String::from_utf16_lossy(&c.path)), Some("long\0".into()));
    }

    #[test]
    fn standard_hid_collections_are_ignored() {
        // Keyboard/consumer pages (< 0xFF00) never count, even with a "long" usage.
        assert!(group_collections([
            col(0x0AB5, 0, 0x000C, 0x0001, "consumer"),
            col(0x0AB5, 0, 0x0001, 0x0002, "mouse")
        ])
        .is_empty());
    }

    #[test]
    fn vendor_collection_without_long_usage_is_dropped() {
        assert!(group_collections([col(0x0AB5, 3, 0xFF43, 0x0001, "short only")]).is_empty());
    }

    /// Scripted handle: queued input reports, plus the reply to send for each write.
    struct FakeHandle {
        input: std::cell::RefCell<std::collections::VecDeque<Vec<u8>>>,
        answer: fn(&[u8]) -> Vec<u8>,
    }

    impl Transport for FakeHandle {
        fn write(&self, data: &[u8]) -> Result<usize, Error> {
            self.input.borrow_mut().push_back((self.answer)(data));
            Ok(data.len())
        }
        fn read_timeout(&self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize, Error> {
            Ok(match self.input.borrow_mut().pop_front() {
                Some(r) => {
                    buf[..r.len()].copy_from_slice(&r);
                    r.len()
                }
                None => 0,
            })
        }
    }

    #[test]
    fn late_reply_to_an_earlier_lookup_is_not_taken_as_the_answer() {
        // A device that just woke up answers the lookup that had already timed out
        // ("0x1004? not supported") after we gave up on it. Root lookup replies don't echo
        // the feature id, so that stale reply looks exactly like an answer to the next one.
        let stale = vec![REPORT_LONG, 1, 0x00, SW_ID, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let long = FakeHandle {
            input: std::cell::RefCell::new([stale].into()),
            // The device has 0x1001 at index 6.
            answer: |tx| {
                let mut r = tx.to_vec();
                r[4..].fill(0);
                r[4] = 6;
                r
            },
        };
        let link = Link { short: None, long: Some(long) };
        assert_eq!(link.feature_index(1, 0x1001).unwrap(), Some(6));
    }

    #[test]
    fn interfaces_and_pids_are_grouped_separately() {
        let eps = group_collections([
            col(0x0AB5, 3, 0xFF43, 0x0202, "a"),
            col(0x0AB5, 4, 0xFF43, 0x0202, "b"),
            col(0x0AFE, 3, 0xFF43, 0x0202, "c"),
        ]);
        assert_eq!(eps.len(), 3);
    }

    #[test]
    fn legacy_short_register_request() {
        let short = FakeHandle {
            input: std::cell::RefCell::new(Default::default()),
            answer: |tx| {
                assert_eq!(tx, &[0x10, 2, 0x81, 0x0d, 0, 0, 0]);
                vec![0x10, 2, 0x81, 0x0d, 55, 0, 0x22]
            },
        };
        let link = Link { short: Some(short), long: None };
        let r = link.raw_request(2, 0x81, 0x0d, &[], true, 500).unwrap();
        assert_eq!(&r[..3], &[55, 0, 0x22]);
        assert!(matches!(reply(&[0x10, 2, 0x8f, 0, 0x1d, 1, 0], 2, 0, 0x1d), Some(Err(Error::Protocol(1)))));
        assert!(reply(&[0x11, 2, 0, 0x1d], 2, 0, 0x1d).is_none());
    }

    #[test]
    fn identical_receivers_are_separate_physical_groups() {
        let eps = group_collections([
            col(0xc547, 2, 0xff00, 1, r"\\?\hid#vid_046d&pid_c547&mi_02&col01#8&aaaa&0&0000#{guid}"),
            col(0xc547, 2, 0xff00, 2, r"\\?\hid#vid_046d&pid_c547&mi_02&col02#8&aaaa&0&0001#{guid}"),
            col(0xc547, 2, 0xff00, 2, r"\\?\hid#vid_046d&pid_c547&mi_02&col02#8&bbbb&0&0001#{guid}"),
        ]);
        assert_eq!(eps.len(), 2);
        assert!(eps[0].short.is_some() && eps[0].long.is_some());
        assert_eq!(eps[1].suffix, "#2");
    }
}
