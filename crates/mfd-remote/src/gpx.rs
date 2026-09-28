//! A chartplotter's waypoints, routes and tracks as GPX 1.1.
//!
//! Waypoints and routes as a NEON chartplotter's `/api/v1/sync/...` has them (a NOS unit's are read into
//! the same shape). Waypoint: id, created_at, updated_at, deleted_at (ms), name, latitude, longitude,
//! category_id, color_id, tags, description. Route: the same plus points [{id, name, latitude, longitude,
//! point_type}] and legs. Deleted items (deleted_at > 0) are left out.

use serde_json::Value;
use std::fmt::Write;

/// A track: its points as (latitude, longitude, epoch ms).
pub struct Track {
    pub name: String,
    pub points: Vec<(f64, f64, Option<i64>)>,
}

pub fn gpx(creator: &str, waypoints: &[Value], routes: &[Value], tracks: &[Track]) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        s,
        "<gpx version=\"1.1\" creator=\"{}\" xmlns=\"http://www.topografix.com/GPX/1/1\">",
        esc(creator)
    );
    for w in waypoints.iter().filter(|v| live(v)) {
        let Some((lat, lon)) = position(w) else {
            continue;
        };
        let _ = writeln!(s, "  <wpt lat=\"{lat}\" lon=\"{lon}\">");
        if let Some(t) = time(w) {
            let _ = writeln!(s, "    <time>{t}</time>");
        }
        text(&mut s, "    ", "name", w.get("name"));
        text(&mut s, "    ", "cmt", w.get("tags"));
        text(&mut s, "    ", "desc", w.get("description"));
        text(&mut s, "    ", "sym", w.get("category_id"));
        s.push_str("  </wpt>\n");
    }
    for r in routes.iter().filter(|v| live(v)) {
        s.push_str("  <rte>\n");
        text(&mut s, "    ", "name", r.get("name"));
        text(&mut s, "    ", "desc", r.get("description"));
        for p in r
            .get("points")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some((lat, lon)) = position(p) else {
                continue;
            };
            let _ = writeln!(s, "    <rtept lat=\"{lat}\" lon=\"{lon}\">");
            text(&mut s, "      ", "name", p.get("name"));
            s.push_str("    </rtept>\n");
        }
        s.push_str("  </rte>\n");
    }
    for t in tracks.iter().filter(|t| !t.points.is_empty()) {
        s.push_str("  <trk>\n");
        text(&mut s, "    ", "name", Some(&Value::from(t.name.as_str())));
        s.push_str("    <trkseg>\n");
        for &(lat, lon, ms) in &t.points {
            if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
                continue;
            }
            match ms.and_then(iso) {
                Some(t) => {
                    let _ = writeln!(
                        s,
                        "      <trkpt lat=\"{lat:.7}\" lon=\"{lon:.7}\"><time>{t}</time></trkpt>"
                    );
                }
                None => {
                    let _ = writeln!(s, "      <trkpt lat=\"{lat:.7}\" lon=\"{lon:.7}\"/>");
                }
            }
        }
        s.push_str("    </trkseg>\n  </trk>\n");
    }
    s.push_str("</gpx>\n");
    s
}

fn live(v: &Value) -> bool {
    v.get("deleted_at").and_then(Value::as_i64).unwrap_or(0) <= 0
}

fn position(v: &Value) -> Option<(f64, f64)> {
    let lat = v.get("latitude")?.as_f64()?;
    let lon = v.get("longitude")?.as_f64()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// created_at (epoch ms) as ISO 8601 UTC.
fn time(v: &Value) -> Option<String> {
    iso(v.get("created_at")?.as_i64()?)
}

/// Epoch ms as ISO 8601 UTC.
fn iso(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1000;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

fn text(s: &mut String, indent: &str, tag: &str, v: Option<&Value>) {
    if let Some(t) = v
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let _ = writeln!(s, "{indent}<{tag}>{}</{tag}>", esc(t));
    }
}

fn esc(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_waypoints_and_routes() {
        let wps = [
            json!({"id": "a", "name": "Neon & Two", "latitude": 33.39, "longitude": 129.84,
                   "category_id": "Anchoring", "created_at": 1790000000000i64, "deleted_at": 0}),
            json!({"id": "b", "name": "gone", "latitude": 1.0, "longitude": 2.0, "deleted_at": 5}),
        ];
        let rts = [json!({"id": "r", "name": "Home", "points": [
            {"id": "p1", "name": "Start", "latitude": 33.0, "longitude": 129.0},
            {"id": "p2", "latitude": 33.1, "longitude": 129.1}]})];
        let trk = [Track {
            name: "Track001".into(),
            points: vec![(33.5, 129.5, Some(1790000000000)), (33.6, 129.6, None)],
        }];
        let g = gpx("test", &wps, &rts, &trk);
        assert!(g.contains("<wpt lat=\"33.39\" lon=\"129.84\">"));
        assert!(g.contains("<name>Neon &amp; Two</name>"));
        assert!(g.contains("<sym>Anchoring</sym>"));
        assert!(g.contains("<time>2026-09-21T14:13:20Z</time>"));
        assert!(!g.contains("gone"));
        assert!(g.contains("<rtept lat=\"33\" lon=\"129\">") && g.contains("<name>Start</name>"));
        assert!(g.contains(
            "<trkpt lat=\"33.5000000\" lon=\"129.5000000\"><time>2026-09-21T14:13:20Z</time>"
        ));
        assert!(g.contains("<trkpt lat=\"33.6000000\" lon=\"129.6000000\"/>"));
    }
}
