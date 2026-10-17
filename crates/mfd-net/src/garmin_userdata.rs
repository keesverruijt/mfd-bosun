//! Garmin user data — waypoints and routes — as the ActiveCaptain "sync with plotter" carries them, decoded
//! from a live capture (see `~/src/rev/garmin/SYNC-AND-HTTP-API.md`). They travel over the plotter's device
//! message bus (TCP 51200's sibling on port 50615, framed `"MSG*" <len:u32le> <payload>`): each object type
//! has its own message (waypoints `0x1c`, routes `0x1d`), the two sides diff by a per-record version stamp,
//! and each changed record is a gzip-compressed JSON object.
//!
//! It has two halves. The record converters ([`waypoint_to_gpx`], [`route_to_gpx`]) are transport-independent:
//! they turn a decompressed record into the vendor-neutral shape [`crate::gpx`] wants (the NEON REST shape —
//! `latitude`/`longitude` in degrees, `name`, `points` …), and are tested against the exact bytes seen on the
//! wire. [`fetch`] is the live client that drives the bus and runs its records through them, so a Garmin
//! plotter's waypoints and routes can be exported as GPX like any other unit's.
//!
//! Coordinates on the wire are **Garmin semicircles**: a signed 32-bit count where degrees = n · 180 / 2³¹.
//!
//! ## The bus
//!
//! Each object type is its own channel, framed `<type:u32le> <subtype:u16le> <bodylen:1B> <body>`:
//!
//!   0x01 subscribe   `<k> 00 <tok:4> <ctr:4>`  (k: session 0x0a, waypoints 0x03, routes 0x02)
//!   0x0a ack         plotter, 8 zero bytes
//!   0x0b manifest    client "what I have"; we send it **empty** so the plotter offers everything
//!   0x0c sync-state  plotter→ `<clk:4> <ctr:4> 03 01 01 00`; client must echo it back as 0x0d
//!   0x0f manifest    plotter "what I have": a clock block `<n+1 ×(clk:4 ctr:4)>` then the record list
//!                    (`… 07 11 10 <uuid:16> 0d <vstamp>` per record)
//!   0x0c request     client "send me these": the plotter's **current** clock (the pair with the highest
//!                    counter, learned from its 0x0f) then `01 01 <count> (01 07 11 10 <uuid:16>)*count`
//!   0x0d data        plotter then sends the records as gzip JSON streams (`1f 8b 08 …`)
//!
//! The protocol is clock-synchronised: the request must carry the plotter's current clock for that type, or it
//! withholds the records. A session channel (type 0x04) must be subscribed first; its store is empty, so its
//! request is the short "in sync" form. Decoded from `~/src/rev/garmin/SYNC-AND-HTTP-API.md` and a live pull.

use serde_json::{json, Value};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A Garmin semicircle (signed 32-bit) as degrees. 180°E/S wraps through the sign bit, so the raw value is
/// read as `i32` first.
pub fn semicircles_to_deg(raw: i64) -> f64 {
    (raw as i32) as f64 * (180.0 / 2_147_483_648.0)
}

/// A decompressed Garmin waypoint record (`0x1c`) as a waypoint in [`crate::gpx`]'s shape. `None` if it has no
/// position.
pub fn waypoint_to_gpx(record: &[u8]) -> Option<Value> {
    let w: Value = serde_json::from_slice(record).ok()?;
    let lat = semicircles_to_deg(w.get("lat")?.as_i64()?);
    let lon = semicircles_to_deg(w.get("lon")?.as_i64()?);
    Some(json!({
        "id": w.get("uuid").and_then(Value::as_str).unwrap_or(""),
        "name": w.get("name").and_then(Value::as_str).unwrap_or(""),
        "latitude": lat,
        "longitude": lon,
        // the plotter's symbol id and comment, as GPX <sym> and <cmt>
        "category_id": w.get("sym"),
        "tags": w.get("comment"),
        "deleted_at": 0,
    }))
}

/// A decompressed Garmin route record (`0x1d`) as a route in [`crate::gpx`]'s shape. A route carries its own
/// `{lon,lat}` points inline (not references to waypoints).
pub fn route_to_gpx(record: &[u8]) -> Option<Value> {
    let r: Value = serde_json::from_slice(record).ok()?;
    let points: Vec<Value> = r
        .get("points")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(json!({
                "latitude": semicircles_to_deg(p.get("lat")?.as_i64()?),
                "longitude": semicircles_to_deg(p.get("lon")?.as_i64()?),
            }))
        })
        .collect();
    Some(json!({
        "id": r.get("uuid").and_then(Value::as_str).unwrap_or(""),
        "name": r.get("name").and_then(Value::as_str).unwrap_or(""),
        "points": points,
        "deleted_at": 0,
    }))
}

/// The device user-data bus (waypoints, routes, …), framed `"MSG*" <len:u32le> <payload>` — a different port
/// from the helm control socket.
pub const DATA_PORT: u16 = 50615;

const T_SESSION: u32 = 0x04;
const T_WAYPOINTS: u32 = 0x1c;
const T_ROUTES: u32 = 0x1d;
const DATA_TYPES: [u32; 2] = [T_WAYPOINTS, T_ROUTES];
/// The client's starting counter in subscribes/manifests, as the ActiveCaptain app uses.
const CTR0: [u8; 4] = [0xfe, 0, 0, 0];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// How long to let the pull run before returning what arrived.
const PULL_TIMEOUT: Duration = Duration::from_secs(8);

/// Pulls all waypoints and routes from a Garmin plotter's user-data bus and returns them in [`crate::gpx`]'s
/// shape as `(waypoints, routes)`. Read-only: it only subscribes, acks, and requests records — it never writes
/// a record. The ActiveCaptain app should not be connected at the same time (then our client owns the session).
pub async fn fetch(ip: IpAddr) -> io::Result<(Vec<Value>, Vec<Value>)> {
    let addr = SocketAddr::new(ip, DATA_PORT);
    let s = tokio::time::timeout(CONNECT_TIMEOUT, crate::link::connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no answer"))??;
    s.set_nodelay(true)?;
    let (mut r, mut w) = s.into_split();

    // the session channel must be subscribed first, then the data channels; then an empty manifest for each so
    // the plotter offers everything it has. A channel's subscribe and manifest carry the same client token.
    let toks: [[u8; 4]; 3] = [rand4(), rand4(), rand4()];
    let tok = |t: u32| match t {
        T_SESSION => toks[0],
        T_WAYPOINTS => toks[1],
        _ => toks[2],
    };
    for t in [T_SESSION, T_WAYPOINTS, T_ROUTES] {
        w.write_all(&subscribe(t, tok(t))).await?;
    }
    for t in [T_SESSION, T_WAYPOINTS, T_ROUTES] {
        w.write_all(&empty_manifest(t, tok(t))).await?;
    }

    let mut waypoints = Vec::new();
    let mut routes = Vec::new();
    let mut replied_0d = [false; 3]; // session, waypoints, routes
    let mut requested = [false; 3];
    let mut seen_0f = [false; 3];
    let mut want_records = 0usize;
    let mut got_records = 0usize;
    let index = |t: u32| match t {
        T_SESSION => 0,
        T_WAYPOINTS => 1,
        _ => 2,
    };

    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    let deadline = Instant::now() + PULL_TIMEOUT;
    let mut last_hb = Instant::now();
    loop {
        if Instant::now() >= deadline {
            break;
        }
        // drive the ~2/s heartbeat the app sends, so the plotter keeps the session alive
        if last_hb.elapsed() >= Duration::from_millis(400) {
            w.write_all(&frame(&[0, 0, 0, 0, 0x07, 0x00, 0x01, 0x00]))
                .await?;
            last_hb = Instant::now();
        }
        let n = match tokio::time::timeout(Duration::from_millis(300), r.read(&mut chunk)).await {
            Ok(Ok(0)) => break, // closed
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue, // idle; loop to heartbeat / check deadline
        };
        buf.extend_from_slice(&chunk[..n]);
        for payload in take_frames(&mut buf) {
            if payload.len() < 6 {
                continue;
            }
            let t = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            let sub = u16::from_le_bytes(payload[4..6].try_into().unwrap());
            if ![T_SESSION, T_WAYPOINTS, T_ROUTES].contains(&t) {
                continue;
            }
            let i = index(t);
            match sub {
                // sync-state: echo the plotter's clock back
                0x0c if !replied_0d[i] && payload.len() >= 15 => {
                    let clk = payload[7..11].try_into().unwrap();
                    let ctr = payload[11..15].try_into().unwrap();
                    w.write_all(&sync_reply(t, clk, ctr)).await?;
                    replied_0d[i] = true;
                }
                // the plotter's manifest: learn its current clock and request every record it lists
                0x0f if !requested[i] => {
                    seen_0f[i] = true;
                    requested[i] = true;
                    if let Some((clk, ctr, uuids)) = parse_manifest(&payload) {
                        if !uuids.is_empty() {
                            want_records += 1;
                        }
                        w.write_all(&request(t, clk, ctr, &uuids)).await?;
                    }
                }
                _ => {}
            }
            // records (gzip JSON) ride 0x0d data frames; harvest from whatever carries the magic
            let mut found = false;
            for record in gzip_records(&payload) {
                found = true;
                match t {
                    T_WAYPOINTS => {
                        if let Some(v) = waypoint_to_gpx(&record) {
                            waypoints.push(v);
                        }
                    }
                    T_ROUTES => {
                        if let Some(v) = route_to_gpx(&record) {
                            routes.push(v);
                        }
                    }
                    _ => {}
                }
            }
            if found {
                got_records += 1;
            }
        }
        // done once both data channels have offered their manifest and every offered record has arrived
        if DATA_TYPES.iter().all(|&t| seen_0f[index(t)]) && got_records >= want_records {
            break;
        }
    }
    Ok((waypoints, routes))
}

/// A `"MSG*"` frame around a channel payload.
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(8 + payload.len());
    m.extend_from_slice(b"MSG*");
    m.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    m.extend_from_slice(payload);
    m
}

/// A channel payload header: the object type then the subtype.
fn head(t: u32, sub: u16) -> Vec<u8> {
    let mut p = t.to_le_bytes().to_vec();
    p.extend_from_slice(&sub.to_le_bytes());
    p
}

/// The `k` byte a type carries in its subscribe.
fn subscribe_k(t: u32) -> u8 {
    match t {
        T_SESSION => 0x0a,
        T_WAYPOINTS => 0x03,
        T_ROUTES => 0x02,
        _ => 0,
    }
}

/// Subscribe to a channel (subtype 0x01).
fn subscribe(t: u32, tok: [u8; 4]) -> Vec<u8> {
    let mut p = head(t, 0x01);
    p.push(0x0a); // body length
    p.extend_from_slice(&[subscribe_k(t), 0x00]);
    p.extend_from_slice(&tok);
    p.extend_from_slice(&CTR0);
    frame(&p)
}

/// An empty "what I have" manifest (subtype 0x0b): zero records, so the plotter offers everything.
fn empty_manifest(t: u32, tok: [u8; 4]) -> Vec<u8> {
    let mut p = head(t, 0x0b);
    p.push(0x0e); // body length
    p.extend_from_slice(&tok);
    p.extend_from_slice(&CTR0);
    p.extend_from_slice(&[0x05, 0x02, 0x01, 0x00, 0x09, 0x00]);
    frame(&p)
}

/// Echo the plotter's sync-state back (subtype 0x0d).
fn sync_reply(t: u32, clk: [u8; 4], ctr: [u8; 4]) -> Vec<u8> {
    let mut p = head(t, 0x0d);
    p.push(0x0c); // body length
    p.extend_from_slice(&clk);
    p.extend_from_slice(&ctr);
    p.extend_from_slice(&[0x03, 0x01, 0x01, 0x00]);
    frame(&p)
}

/// Request records (subtype 0x0c): the plotter's current clock, then the uuids wanted. With no uuids it is the
/// short "in sync" form the session channel uses.
fn request(t: u32, clk: [u8; 4], ctr: [u8; 4], uuids: &[[u8; 16]]) -> Vec<u8> {
    let mut p = head(t, 0x0c);
    let mut body = clk.to_vec();
    body.extend_from_slice(&ctr);
    if uuids.is_empty() {
        body.extend_from_slice(&[0x03, 0x01, 0x01, 0x00]);
    } else {
        let mut inner = vec![0x01, 0x01, uuids.len() as u8];
        for u in uuids {
            inner.extend_from_slice(&[0x01, 0x07, 0x11, 0x10]);
            inner.extend_from_slice(u);
        }
        body.push(inner.len() as u8);
        body.extend_from_slice(&inner);
    }
    p.push(body.len() as u8);
    p.extend_from_slice(&body);
    frame(&p)
}

/// A parsed 0x0f manifest: the plotter's current clock `(clk, ctr)` and the uuids it holds.
type Manifest = ([u8; 4], [u8; 4], Vec<[u8; 16]>);

/// The plotter's current clock (the `(clk, ctr)` pair with the highest counter) and the record uuids, from a
/// 0x0f manifest payload. Layout: type(4) subtype(2) varint(bodylen) count(1) then `count+1` clock pairs, then
/// the record list where each entry carries `07 11 10 <uuid:16>`.
fn parse_manifest(pl: &[u8]) -> Option<Manifest> {
    let mut i = 6;
    i = skip_varint(pl, i)?;
    let n = *pl.get(i)? as usize;
    i += 1;
    let mut best: Option<([u8; 4], [u8; 4])> = None;
    for _ in 0..=n {
        let clk: [u8; 4] = pl.get(i..i + 4)?.try_into().ok()?;
        let ctr: [u8; 4] = pl.get(i + 4..i + 8)?.try_into().ok()?;
        i += 8;
        if best.is_none_or(|(_, b)| u32::from_le_bytes(b) < u32::from_le_bytes(ctr)) {
            best = Some((clk, ctr));
        }
    }
    let (clk, ctr) = best?;
    let mut uuids = Vec::new();
    let mut j = i;
    while let Some(k) = find(&pl[j..], &[0x07, 0x11, 0x10]) {
        let p = j + k + 3;
        if p + 16 > pl.len() {
            break;
        }
        uuids.push(pl[p..p + 16].try_into().unwrap());
        j = p + 16;
    }
    Some((clk, ctr, uuids))
}

/// Advances past one LEB128 varint, returning the new offset.
fn skip_varint(b: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let x = *b.get(i)?;
        i += 1;
        if x & 0x80 == 0 {
            return Some(i);
        }
    }
}

/// The first offset of `needle` in `hay`, if any.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Every gzip-JSON record (`1f 8b 08 …`) carried in a frame payload, each decompressed and trimmed to its JSON
/// object (the stored blob has a trailing byte past the closing brace).
fn gzip_records(pl: &[u8]) -> Vec<Vec<u8>> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = find(&pl[from..], &[0x1f, 0x8b, 0x08]) {
        let start = from + off;
        let mut d = GzDecoder::new(&pl[start..]);
        let mut raw = Vec::new();
        if d.read_to_end(&mut raw).is_ok() {
            if let Some(end) = raw.iter().rposition(|&b| b == b'}') {
                raw.truncate(end + 1);
                out.push(raw);
            }
        }
        from = start + 3;
    }
    out
}

/// Splits whole `"MSG*"` frames off the front of `buf`, leaving any partial tail.
fn take_frames(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i + 8 <= buf.len() {
        if &buf[i..i + 4] != b"MSG*" {
            i += 1;
            continue;
        }
        let len = u32::from_le_bytes(buf[i + 4..i + 8].try_into().unwrap()) as usize;
        if i + 8 + len > buf.len() {
            break;
        }
        frames.push(buf[i + 8..i + 8 + len].to_vec());
        i += 8 + len;
    }
    buf.drain(..i);
    frames
}

/// Four random bytes for a channel's client token.
fn rand4() -> [u8; 4] {
    let mut b = [0u8; 4];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semicircles_match_degrees() {
        // the captured waypoint "0001": 53.17895 N, 5.43795 E
        assert!((semicircles_to_deg(634449540) - 53.17895).abs() < 1e-5);
        assert!((semicircles_to_deg(64877233) - 5.43795).abs() < 1e-5);
        assert_eq!(semicircles_to_deg(0), 0.0);
    }

    #[test]
    fn waypoint_record_becomes_a_gpx_waypoint() {
        // the exact decompressed 0x1c record from the capture
        let rec = br#"{"uuid":"728feb28-cfd3-4200-b452-a86b7f817ce4","proto_ver":2,"min_proto_ver":1,"lat":634449540,"lon":64877233,"dspl_optn":19,"depth":null,"temp":null,"mtime":1161088582,"vstamp":11475864600,"name":"0001","comment":null,"sym":18}"#;
        let w = waypoint_to_gpx(rec).unwrap();
        assert_eq!(w["name"], "0001");
        assert_eq!(w["id"], "728feb28-cfd3-4200-b452-a86b7f817ce4");
        assert_eq!(w["category_id"], 18);
        assert!((w["latitude"].as_f64().unwrap() - 53.17895).abs() < 1e-5);
        assert!((w["longitude"].as_f64().unwrap() - 5.43795).abs() < 1e-5);
    }

    #[test]
    fn route_record_becomes_a_gpx_route() {
        // the exact decompressed 0x1d record from the capture (6 points)
        let rec = br#"{"name":"1","uuid":"7a1441ec-c60b-41d5-a127-89367a0160b5","auto_name":true,"proto_ver":2,"min_proto_ver":1,"vstamp":133497449726,"points":[{"lon":59140803,"lat":635742847},{"lon":57480677,"lat":634942505},{"lon":58208991,"lat":634462300},{"lon":60640271,"lat":635205017},{"lon":60833060,"lat":636338300},{"lon":58862331,"lat":636597611}]}"#;
        let r = route_to_gpx(rec).unwrap();
        assert_eq!(r["name"], "1");
        let pts = r["points"].as_array().unwrap();
        assert_eq!(pts.len(), 6);
        assert!((pts[0]["latitude"].as_f64().unwrap() - 53.28735).abs() < 1e-5);
        assert!((pts[0]["longitude"].as_f64().unwrap() - 4.95712).abs() < 1e-5);
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn subscribe_frame_matches_the_capture() {
        // waypoints subscribe (k=0x03) with the token/counter seen on the wire
        let got = subscribe(T_WAYPOINTS, [0x84, 0xbd, 0x5b, 0x35]);
        let want = hex("4d53472a 11000000 1c000000 0100 0a 0300 84bd5b35 fe000000");
        assert_eq!(got, want);
        // session is subscribed with k=0x0a
        assert_eq!(subscribe(T_SESSION, [0; 4])[14], 0x0a);
    }

    #[test]
    fn empty_manifest_matches_the_capture() {
        let got = empty_manifest(T_SESSION, [0xe4, 0xbd, 0x5b, 0x35]);
        let want = hex("4d53472a 15000000 04000000 0b00 0e e4bd5b35 fe000000 050201000900");
        assert_eq!(got, want);
    }

    #[test]
    fn request_for_records_matches_the_capture() {
        // the live 0x1c request: current clock 17bd5b35/0c010000, five uuids wanted
        let uuids = [
            hex("728feb28cfd34200b452a86b7f817ce4"),
            hex("387a2592dad04fc38959c5e9dc4aac7e"),
            hex("613ff878b9d6408ea55c9361e75d7437"),
            hex("c451f046fe744a65bb1af8c04122b482"),
            hex("d2d806ab6ad14ca3852cd241c5614dfe"),
        ]
        .map(|v| <[u8; 16]>::try_from(v).unwrap());
        let got = request(
            T_WAYPOINTS,
            [0x17, 0xbd, 0x5b, 0x35],
            [0x0c, 0x01, 0x00, 0x00],
            &uuids,
        );
        let want = hex(
            "4d53472a 77000000 1c000000 0c00 70 17bd5b35 0c010000 67 010105 \
             01071110 728feb28cfd34200b452a86b7f817ce4 \
             01071110 387a2592dad04fc38959c5e9dc4aac7e \
             01071110 613ff878b9d6408ea55c9361e75d7437 \
             01071110 c451f046fe744a65bb1af8c04122b482 \
             01071110 d2d806ab6ad14ca3852cd241c5614dfe",
        );
        assert_eq!(got, want);
        // the session's request is the short "in sync" form (no uuids)
        let short = request(
            T_SESSION,
            [0xa2, 0xbd, 0x5b, 0x35],
            [0x05, 0x01, 0x00, 0x00],
            &[],
        );
        assert_eq!(
            short,
            hex("4d53472a 13000000 04000000 0c00 0c a2bd5b35 05010000 03010100")
        );
    }

    #[test]
    fn manifest_gives_the_current_clock_and_uuids() {
        // the live 0x1c 0x0f: three clock pairs, current = the highest counter (17bd5b35/0c010000), five uuids
        let pl = hex(
            "1c000000 0f00 a201 02 f7fdba2a fe000000 eebd5b35 0b010000 17bd5b35 0c010000 \
             87 01020105090002 071110 728feb28cfd34200b452a86b7f817ce4 0d 98988fe02a \
             02071110 387a2592dad04fc38959c5e9dc4aac7e 0d b8dbc5db2f \
             02071110 613ff878b9d6408ea55c9361e75d7437 0d cdfca4b037 \
             02071110 c451f046fe744a65bb1af8c04122b482 0d 849bb79923 \
             02071110 d2d806ab6ad14ca3852cd241c5614dfe 0d b7aea6e622",
        );
        let (clk, ctr, uuids) = parse_manifest(&pl).unwrap();
        assert_eq!(clk, [0x17, 0xbd, 0x5b, 0x35]);
        assert_eq!(ctr, [0x0c, 0x01, 0x00, 0x00]);
        assert_eq!(uuids.len(), 5);
        assert_eq!(&uuids[0], &hex("728feb28cfd34200b452a86b7f817ce4")[..]);
        // the session 0x0f advertises no records -> no uuids (short request)
        let sess = hex(
            "04000000 0f00 1f 02 b5af400d fe000000 48bd5b35 04010000 a2bd5b35 05010000 \
             05 0201000900",
        );
        let (sclk, sctr, su) = parse_manifest(&sess).unwrap();
        assert_eq!(
            (sclk, sctr),
            ([0xa2, 0xbd, 0x5b, 0x35], [0x05, 0x01, 0x00, 0x00])
        );
        assert!(su.is_empty());
    }

    #[test]
    fn frames_reassemble_across_chunk_boundaries() {
        let mut buf = hex("4d53472a 03000000 aabbcc 4d53472a 02000000 dd").to_vec();
        // only the first whole frame comes out; the second is incomplete
        let got = take_frames(&mut buf);
        assert_eq!(got, vec![hex("aabbcc")]);
        buf.push(0xee); // completes the second frame
        assert_eq!(take_frames(&mut buf), vec![hex("ddee")]);
        assert!(buf.is_empty());
    }
}
