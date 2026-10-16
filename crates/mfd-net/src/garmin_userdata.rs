//! Garmin user data — waypoints and routes — as the ActiveCaptain "sync with plotter" carries them, decoded
//! from a live capture (see `~/src/rev/garmin/SYNC-AND-HTTP-API.md`). They travel over the plotter's device
//! message bus (TCP 51200's sibling on port 50615, framed `"MSG*" <len:u32le> <payload>`): each object type
//! has its own message (waypoints `0x1c`, routes `0x1d`), the two sides diff by a per-record version stamp,
//! and each changed record is a gzip-compressed JSON object.
//!
//! This module is the part that does not depend on the live transport: turning a decompressed record into the
//! vendor-neutral shape [`crate::gpx`] wants (the NEON REST shape — `latitude`/`longitude` in degrees, `name`,
//! `points` …), so a Garmin plotter's waypoints and routes can be exported as GPX like any other unit's. The
//! message-bus client that fetches the gzip records is still to come; these converters are tested against the
//! exact bytes seen on the wire.
//!
//! Coordinates on the wire are **Garmin semicircles**: a signed 32-bit count where degrees = n · 180 / 2³¹.

use serde_json::{json, Value};

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
}
