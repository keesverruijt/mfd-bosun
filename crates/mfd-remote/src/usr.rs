//! Navico's USR file ("Navico export data file"), versions 4 to 6: what a NOS unit's
//! `/cgi-bin/download.cgi` gives, with all its waypoints, routes and trails. Read into the shapes
//! [`crate::gpx`] takes.
//!
//! All little-endian. A string is an i32 byte count (-1: none) and the bytes: one byte a character in the
//! file header, UTF-16 elsewhere. Positions are Mercator metres on a sphere of the WGS 84 semi-minor axis,
//! except in trails (radians, f64). Times are a Julian day and milliseconds into it; in trails, Unix
//! seconds.
//!
//!   header    u16 version, u16, u32, title, date text, u32 day, u32 ms, u8, u32 serial, comment
//!   waypoints u32 n, n ×: [v5+: uuid 16], u32 unit, u64 seq, u16 stream version, name,
//!             [v5+: u32 unit], i32 x, i32 y, u32 flags, u16 icon, u16 colour, description, f32 alarm
//!             radius, u32 day, u32 ms, u8, f32 depth, 3 × u32 (Loran)
//!   routes    u32 n, n ×: [v5+: uuid], u32 unit, u64 seq, u16, name, [v5+: u32], u32 legs,
//!             legs × (v5+: waypoint uuid; v4: u32 unit, u64 seq), [v5+: u32, u32, u8], u8
//!   trails    u32 n, n ×: u32 unit, u64 seq, u16 trail version, name, u32 flags, u32 colour,
//!             description, u32 day, u32 ms, 3 × u8, [trail version < 6: u32 n, n × (v5: u32; v4: u8)],
//!             u32 points, points × (u16, u8, u32 time, f64 lon, f64 lat, u32 n, n × (u8, f32))

use crate::gpx::Track;
use serde_json::{json, Value};
use std::collections::HashMap;

const SEMIMINOR: f64 = 6_356_752.314_2;
/// Julian day of 1970-01-01.
const UNIX_DAY: i64 = 2_440_588;

pub struct Usr {
    pub waypoints: Vec<Value>,
    pub routes: Vec<Value>,
    pub tracks: Vec<Track>,
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

type R<T> = Result<T, String>;

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.d.len());
        let end = end.ok_or_else(|| format!("the file ends early, at byte {}", self.at))?;
        let b = &self.d[self.at..end];
        self.at = end;
        Ok(b)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> R<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> R<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn skip(&mut self, n: usize) -> R<()> {
        self.take(n).map(|_| ())
    }
    fn text(&mut self, wide: bool) -> R<String> {
        let n = self.i32()?;
        if n <= 0 {
            return Ok(String::new());
        }
        let b = self.take(n as usize)?;
        Ok(if wide {
            let u: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16_lossy(&u)
        } else {
            b.iter().map(|&c| c as char).collect()
        })
    }
    /// A count, not more than what could still fit in the file.
    fn count(&mut self, min_size: usize) -> R<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_size) > self.d.len() - self.at {
            return Err(format!("a count of {n} at byte {} is too big", self.at - 4));
        }
        Ok(n)
    }
}

fn lat(y: i32) -> f64 {
    (2.0 * (y as f64 / SEMIMINOR).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}

fn lon(x: i32) -> f64 {
    (x as f64 / SEMIMINOR).to_degrees()
}

/// Epoch milliseconds of a Julian day and milliseconds into it; None for no date.
fn epoch_ms(day: u32, ms: u32) -> Option<i64> {
    let day = day as i64;
    (day > UNIX_DAY).then(|| (day - UNIX_DAY) * 86_400_000 + ms as i64)
}

pub fn parse(d: &[u8]) -> R<Usr> {
    let mut r = Reader { d, at: 0 };
    let version = r.u16()?;
    if !(4..=6).contains(&version) {
        return Err(format!("USR version {version}: only 4 to 6 are known"));
    }
    r.u16()?;
    r.u32()?;
    r.text(false)?; // title
    r.text(false)?; // date
    r.skip(4 + 4 + 1 + 4)?; // date, time, byte, serial number
    r.text(false)?; // comment
    let uuids = version >= 5;

    // waypoints, also by their ids, for the routes
    let n = r.count(40)?;
    let mut waypoints = Vec::with_capacity(n);
    let mut by_uuid: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut by_seq: HashMap<(u32, u32, u32), usize> = HashMap::new();
    for _ in 0..n {
        let uuid = if uuids {
            Some(r.take(16)?.to_vec())
        } else {
            None
        };
        let seq = (r.u32()?, r.u32()?, r.u32()?);
        r.u16()?;
        let name = r.text(true)?;
        if uuids {
            r.u32()?;
        }
        let (x, y) = (r.i32()?, r.i32()?);
        r.skip(4 + 2 + 2)?; // flags, icon, colour
        let description = r.text(true)?;
        r.f32()?; // alarm radius
        let created = epoch_ms(r.u32()?, r.u32()?);
        r.skip(1 + 4 + 12)?; // byte, depth, Loran
        let mut w = json!({ "name": name, "latitude": lat(y), "longitude": lon(x), "description": description });
        if let Some(ms) = created {
            w["created_at"] = ms.into();
        }
        if let Some(u) = uuid {
            by_uuid.insert(u, waypoints.len());
        }
        by_seq.insert(seq, waypoints.len());
        waypoints.push(w);
    }

    let n = r.count(20)?;
    let mut routes = Vec::with_capacity(n);
    for _ in 0..n {
        if uuids {
            r.skip(16)?;
        }
        r.skip(12)?;
        r.u16()?;
        let name = r.text(true)?;
        if uuids {
            r.u32()?;
        }
        let legs = r.count(if uuids { 16 } else { 12 })?;
        let mut points = Vec::with_capacity(legs);
        for _ in 0..legs {
            let i = if uuids {
                by_uuid.get(r.take(16)?).copied()
            } else {
                by_seq.get(&(r.u32()?, r.u32()?, r.u32()?)).copied()
            };
            if let Some(w) = i.map(|i| &waypoints[i]) {
                points.push(json!({ "name": w["name"], "latitude": w["latitude"], "longitude": w["longitude"] }));
            }
        }
        if uuids {
            r.skip(9)?;
        }
        r.u8()?;
        routes.push(json!({ "name": name, "points": points }));
    }

    let n = r.count(30)?;
    let mut tracks = Vec::with_capacity(n);
    for _ in 0..n {
        r.skip(12)?;
        let trail_version = r.u16()?;
        let name = r.text(true)?;
        r.u32()?;
        r.u32()?;
        r.text(true)?;
        r.skip(8 + 3)?;
        if trail_version < 6 {
            let attrs = r.count(1)?;
            r.skip(attrs * if trail_version == 5 { 4 } else { 1 })?;
        }
        let points = r.count(27)?;
        let mut t = Track {
            name,
            points: Vec::with_capacity(points),
        };
        for _ in 0..points {
            r.skip(3)?;
            let time = r.u32()?;
            let (x, y) = (r.f64()?, r.f64()?);
            let values = r.count(5)?;
            r.skip(values * 5)?; // (valid, value) of what the unit measured there
            t.points.push((
                y.to_degrees(),
                x.to_degrees(),
                (time > 0).then(|| time as i64 * 1000),
            ));
        }
        tracks.push(t);
    }
    Ok(Usr {
        waypoints,
        routes,
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small USR 6 file, as a unit writes it.
    struct W(Vec<u8>);
    impl W {
        fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        fn u16(&mut self, v: u16) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn f64(&mut self, v: f64) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn text(&mut self, s: &str, wide: bool) -> &mut Self {
            let b: Vec<u8> = if wide {
                s.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else {
                s.bytes().collect()
            };
            self.u32(b.len() as u32);
            self.0.extend_from_slice(&b);
            self
        }
        fn waypoint(&mut self, id: u8, name: &str, lat_deg: f64, lon_deg: f64) -> &mut Self {
            let y = (SEMIMINOR
                * ((lat_deg.to_radians() + std::f64::consts::FRAC_PI_2) / 2.0)
                    .tan()
                    .ln())
            .round();
            let x = (SEMIMINOR * lon_deg.to_radians()).round();
            self.0.extend_from_slice(&[id; 16]);
            self.u32(1)
                .u32(id as u32)
                .u32(0)
                .u16(2)
                .text(name, true)
                .u32(1);
            self.u32(x as i32 as u32)
                .u32(y as i32 as u32)
                .u32(0)
                .u16(0)
                .u16(0)
                .text("", true);
            self.u32(0)
                .u32(2_461_312)
                .u32(3_600_000)
                .u8(0)
                .u32(0)
                .u32(0)
                .u32(0)
                .u32(0)
        }
    }

    #[test]
    fn reads_waypoints_routes_and_trails() {
        let mut w = W(Vec::new());
        w.u16(6)
            .u16(0)
            .u32(10)
            .text("Navico export data file", false)
            .text("28/09/2026", false);
        w.u32(2_461_312)
            .u32(0)
            .u8(0xff)
            .u32(1)
            .text("Waypoints, routes, and trails", false);
        w.u32(2)
            .waypoint(1, "Home", 33.39, 129.85)
            .waypoint(2, "Away", 34.0, 130.0);
        // a route Home -> Away
        w.u32(1);
        w.0.extend_from_slice(&[9; 16]);
        w.u32(1)
            .u32(9)
            .u32(0)
            .u16(1)
            .text("Out", true)
            .u32(1)
            .u32(2);
        w.0.extend_from_slice(&[1; 16]);
        w.0.extend_from_slice(&[2; 16]);
        w.u32(0).u32(0).u8(0).u8(0xff);
        // a trail of one point, version 6 (no attributes)
        w.u32(1)
            .u32(1)
            .u32(1)
            .u32(0)
            .u16(6)
            .text("Track001", true)
            .u32(2)
            .u32(0);
        w.u32(u32::MAX)
            .u32(2_461_312)
            .u32(0)
            .u8(2)
            .u8(1)
            .u8(1)
            .u32(1);
        w.u16(3)
            .u8(3)
            .u32(1_790_000_000)
            .f64(129.85f64.to_radians())
            .f64(33.39f64.to_radians());
        w.u32(1).u8(1).u32(0);
        w.u32(0);

        let u = parse(&w.0).unwrap();
        assert_eq!(u.waypoints.len(), 2);
        let h = &u.waypoints[0];
        assert_eq!(h["name"], "Home");
        // whole metres in the file: about 1e-5 degrees
        assert!((h["latitude"].as_f64().unwrap() - 33.39).abs() < 1e-5);
        assert!((h["longitude"].as_f64().unwrap() - 129.85).abs() < 1e-5);
        // 2026-09-28 01:00 UTC
        assert_eq!(h["created_at"], 1_790_557_200_000i64);
        assert_eq!(u.routes[0]["name"], "Out");
        assert_eq!(u.routes[0]["points"][1]["name"], "Away");
        assert_eq!(u.tracks[0].name, "Track001");
        let (la, lo, t) = u.tracks[0].points[0];
        assert!((la - 33.39).abs() < 1e-9 && (lo - 129.85).abs() < 1e-9);
        assert_eq!(t, Some(1_790_000_000_000));
    }

    /// `USR`: a file from a unit, to see that all of it reads.
    #[test]
    #[ignore]
    fn reads_a_units_file() {
        let d = std::fs::read(std::env::var("USR").unwrap()).unwrap();
        let u = parse(&d).unwrap();
        eprintln!(
            "{} waypoints, {} routes, {} tracks ({} points)",
            u.waypoints.len(),
            u.routes.len(),
            u.tracks.len(),
            u.tracks.iter().map(|t| t.points.len()).sum::<usize>()
        );
    }
}
