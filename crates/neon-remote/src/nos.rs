//! The remote screen of a NOS unit (see [`neon_net::nos`]), for the same page as a NEON chartplotter's.
//!
//! A NOS unit has no WebRTC, so this side plays the chartplotter's part: it makes the WebRTC offer on the
//! page's signalling socket, sends the unit's RTSP screen as the video (the H.264 as it comes, re-packetized,
//! not re-encoded), and takes the page's input channels: tap, drag, pinch and hardkey become the unit's own
//! remote-control protocol on TCP 6633. The page does not see the difference, apart from the keys it shows.
//!
//! One RTSP stream per unit ([`Feeds`]) goes to every page looking at it. The unit's encoder makes an IDR
//! frame only when a stream starts, so a page that joins later starts from the next whole picture it sends,
//! made an IDR frame for it ([`crate::h264::Starter`]).

use crate::h264;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use neon_net::discovery::Plotter;
use neon_net::nos::{self, Access, Control, Touch};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use str0m::change::SdpAnswer;
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, Notify};

/// A NOS unit's screen, one H.264 access unit (Annex B), and the stream's SPS and PPS (Annex B): the RTSP
/// client puts those before IDR frames only.
#[derive(Clone)]
struct Frame {
    data: Arc<[u8]>,
    params: Arc<[u8]>,
}

struct Feed {
    frames: broadcast::Sender<Frame>,
    /// Start the stream again, for an IDR frame (when a page cannot start from what comes).
    restart: Notify,
}

/// The RTSP streams, one per unit (by serial) while anyone watches it.
#[derive(Clone, Default)]
pub struct Feeds(Arc<Mutex<HashMap<String, Arc<Feed>>>>);

impl Feeds {
    /// The unit's frames, from now on.
    fn watch(&self, serial: &str, ip: IpAddr) -> broadcast::Receiver<Frame> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = g.get(serial) {
            return f.frames.subscribe();
        }
        let (frames, rx) = broadcast::channel(64);
        let feed = Arc::new(Feed {
            frames,
            restart: Notify::new(),
        });
        g.insert(serial.to_string(), feed.clone());
        tokio::spawn(self.clone().run(serial.to_string(), ip, feed));
        rx
    }

    fn restart(&self, serial: &str) {
        if let Some(f) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(serial) {
            f.restart.notify_one();
        }
    }

    /// Stops when nobody watches any more (checked under the lock, so no new watcher is missed).
    fn done(&self, serial: &str, feed: &Arc<Feed>) -> bool {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if feed.frames.receiver_count() > 0 {
            return false;
        }
        if g.get(serial).is_some_and(|f| Arc::ptr_eq(f, feed)) {
            g.remove(serial);
        }
        true
    }

    async fn run(self, serial: String, ip: IpAddr, feed: Arc<Feed>) {
        let url = format!(
            "rtsp://{}/screenmirror",
            SocketAddr::new(ip, nos::RTSP_PORT)
        );
        log::info!("screen of {serial}: {url}");
        loop {
            let stop = tokio::select! {
                r = stream(&url, &feed, || self.done(&serial, &feed)) => match r {
                    Ok(stop) => stop,
                    Err(e) => {
                        log::info!("screen of {serial}: {e}");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        false
                    }
                },
                _ = feed.restart.notified() => {
                    log::debug!("screen of {serial}: again, for a key frame");
                    false
                }
            };
            if stop || self.done(&serial, &feed) {
                break;
            }
        }
        log::info!("screen of {serial}: nobody watches, stopped");
    }
}

/// One RTSP session, until it fails or `done()` (then Ok(true)).
async fn stream(
    url: &str,
    feed: &Feed,
    done: impl Fn() -> bool,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use retina::client::{
        PlayOptions, Session, SessionOptions, SetupOptions, TcpTransportOptions, Transport,
    };
    use retina::codec::{CodecItem, FrameFormat};
    let mut s = Session::describe(url::Url::parse(url)?, SessionOptions::default()).await?;
    let video = s
        .streams()
        .iter()
        .position(|s| s.media() == "video" && s.encoding_name() == "h264")
        .ok_or("no H.264 video")?;
    s.setup(
        video,
        SetupOptions::default()
            .transport(Transport::Tcp(TcpTransportOptions::default()))
            .frame_format(FrameFormat::SIMPLE),
    )
    .await?;
    let mut d = s.play(PlayOptions::default()).await?.demuxed()?;
    let params_of = |d: &retina::client::Demuxed| -> Arc<[u8]> {
        match d.streams()[video].parameters() {
            Some(retina::codec::ParametersRef::Video(v)) => v.extra_data().into(),
            _ => Arc::from(&[][..]),
        }
    };
    let mut params = params_of(&d);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            item = d.next() => match item {
                Some(Ok(CodecItem::VideoFrame(f))) => {
                    if f.has_new_parameters() {
                        params = params_of(&d);
                    }
                    let _ = feed.frames.send(Frame { data: f.into_data().into(), params: params.clone() });
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Err("the stream ended".into()),
            },
            _ = tick.tick() => if done() {
                return Ok(true);
            }
        }
    }
}

/// What the page sends on its input channels (as for a NEON chartplotter).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    x: f64,
    y: f64,
    client_width: f64,
    client_height: f64,
    #[serde(default)]
    is_long_press: bool,
    /// the end of a drag (only this page sends it)
    #[serde(default)]
    end: bool,
}

#[derive(Deserialize)]
struct PinchPoint {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct Pinch {
    phase: String,
    p1: PinchPoint,
    p2: PinchPoint,
}

enum In {
    Tap(u16, u16, bool),
    Drag(u16, u16, bool),
    Key(String),
}

/// From a point in the page's <video> box (the picture letterboxed in it) to the unit's pixels.
fn to_screen(p: &Point, w: u16, h: u16) -> (u16, u16) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let scale = (p.client_width / w).min(p.client_height / h);
    if scale <= 0.0 {
        return (0, 0);
    }
    let x = (p.x - (p.client_width - w * scale) / 2.0) / scale;
    let y = (p.y - (p.client_height - h * scale) / 2.0) / scale;
    (
        x.clamp(0.0, w - 1.0).round() as u16,
        y.clamp(0.0, h - 1.0).round() as u16,
    )
}

/// The page's keys (Android key names, as for a NEON chartplotter; "NOS:<name>" for the unit's own) as the
/// names of NOS keys.
fn key_name(key: &str) -> Option<&str> {
    if let Some(k) = key.strip_prefix("NOS:") {
        return Some(k);
    }
    Some(match key {
        "KEYCODE_HOME" => "page",
        "KEYCODE_MENU" => "menu",
        "KEYCODE_BACK" => "cancel",
        "KEYCODE_ENTER" => "enter",
        "KEYCODE_ZOOM_IN" => "zoomin",
        "KEYCODE_ZOOM_OUT" => "zoomout",
        "KEYCODE_NAVIGATE_IN" => "waypoint",
        "KEYCODE_F1" | "KEYCODE_F2" => "user",
        "KEYCODE_POWER" => "power",
        _ => return None,
    })
}

/// The MAC this program gives a NOS unit, which remembers it once the user allowed it: the same every time
/// (from `seed`), and a locally administered one.
pub fn client_mac(seed: &str) -> [u8; 6] {
    let d = ring::digest::digest(&ring::digest::SHA256, seed.as_bytes());
    let mut m = [0; 6];
    m.copy_from_slice(&d.as_ref()[..6]);
    m[0] = (m[0] & 0xfc) | 0x02;
    m
}

/// Talks to the unit's remote-control port: the page's input as touches and keys.
async fn control(
    addr: SocketAddr,
    mac: [u8; 6],
    name: String,
    mut input: mpsc::Receiver<In>,
    status: mpsc::Sender<String>,
) {
    let say = |s: &str| {
        let _ = status.try_send(s.to_string());
    };
    let mut c = loop {
        match Control::connect(addr, mac, &name).await {
            Ok(c) => break c,
            Err(e) => {
                log::info!("remote control of {addr}: {e}");
                say("input: the chartplotter does not answer, trying again…");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = status.closed() => return,
                }
            }
        }
    };
    log::info!(
        "remote control of {} ({addr}): {}×{}",
        c.info.model,
        c.info.width,
        c.info.height
    );
    let mut access = c.access.clone();
    // a drag in progress: where the finger is, and when the page last moved it
    let mut drag: Option<(u16, u16, Instant)> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut waiting_said = false;
    // a device the unit does not know yet gets no answer at all until the user allows it on the unit
    let ask_text = format!("allow \"{name}\" on the chartplotter's screen to control it");
    let ask = tokio::time::sleep(Duration::from_millis(1500));
    tokio::pin!(ask);
    loop {
        let r = tokio::select! {
            m = input.recv() => {
                let Some(m) = m else { break };
                handle(&mut c, &mut drag, m).await
            }
            ch = access.changed() => {
                if ch.is_err() {
                    say("input: the chartplotter closed the connection");
                    break;
                }
                match *access.borrow_and_update() {
                    Some(Access::Allowed) => {
                        waiting_said = true;
                        say("connected");
                    }
                    Some(Access::Waiting) if !waiting_said => {
                        waiting_said = true;
                        say(&ask_text);
                    }
                    _ => {}
                }
                Ok(())
            }
            _ = &mut ask, if !waiting_said => {
                waiting_said = true;
                if access.borrow().is_none() {
                    say(&ask_text);
                }
                Ok(())
            }
            _ = tick.tick(), if drag.is_some() => {
                // a finger held still still moves, as the vendor's app does; one forgotten is lifted
                let (x, y, at) = drag.unwrap();
                if at.elapsed() > Duration::from_secs(3) {
                    drag = None;
                    c.touch(x, y, Touch::Up).await
                } else {
                    c.touch(x, y, Touch::Move).await
                }
            }
        };
        if let Err(e) = r {
            log::info!("remote control of {addr}: {e}");
            say("input: the connection to the chartplotter broke");
            break;
        }
    }
}

/// The most a drag moves between two touches sent to the unit, in its pixels.
const DRAG_STEP_PX: f64 = 12.0;

async fn handle(
    c: &mut Control,
    drag: &mut Option<(u16, u16, Instant)>,
    m: In,
) -> std::io::Result<()> {
    match m {
        In::Tap(x, y, long) => {
            if let Some((dx, dy, _)) = drag.take() {
                c.touch(dx, dy, Touch::Up).await?;
            }
            c.touch(x, y, Touch::Down).await?;
            let hold = Duration::from_millis(if long { 900 } else { 60 });
            let t0 = Instant::now();
            while t0.elapsed() < hold {
                tokio::time::sleep(Duration::from_millis(30)).await;
                c.touch(x, y, Touch::Move).await?;
            }
            c.touch(x, y, Touch::Up).await
        }
        In::Drag(x, y, end) => {
            match *drag {
                None => c.touch(x, y, Touch::Down).await?,
                // a jump goes in steps, as a finger is seen moving, or the unit takes it for a tap
                Some((x0, y0, _)) => {
                    let (dx, dy) = (x as f64 - x0 as f64, y as f64 - y0 as f64);
                    let steps = (dx.hypot(dy) / DRAG_STEP_PX).ceil().max(1.0) as u32;
                    for i in 1..=steps {
                        let f = i as f64 / steps as f64;
                        let (sx, sy) = (x0 as f64 + dx * f, y0 as f64 + dy * f);
                        c.touch(sx.round() as u16, sy.round() as u16, Touch::Move)
                            .await?;
                        if i < steps {
                            tokio::time::sleep(Duration::from_millis(8)).await;
                        }
                    }
                }
            }
            if end {
                *drag = None;
                c.touch(x, y, Touch::Up).await
            } else {
                *drag = Some((x, y, Instant::now()));
                Ok(())
            }
        }
        In::Key(name) => {
            let Some(code) = c.info.key(&name) else {
                log::debug!("no {name} key on {}", c.info.model);
                return Ok(());
            };
            c.key(code, true).await?;
            tokio::time::sleep(Duration::from_millis(60)).await;
            c.key(code, false).await
        }
    }
}

/// Local addresses a browser may reach us at, for the WebRTC candidates.
fn local_ips() -> Vec<IpAddr> {
    let mut v: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| ip.is_ipv4() && !ip.is_loopback())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The page's session with a NOS unit: this side is the WebRTC offerer.
pub async fn serve(
    feeds: Feeds,
    browser: WebSocket,
    p: Plotter,
    client_key: u64,
    mac: [u8; 6],
    device_name: String,
) {
    let Some(info) = p.nos.clone() else { return };
    let serial = p.serial.clone();
    let (mut to_browser, mut from_browser) = browser.split();
    let send = |command: &str, data: serde_json::Value| {
        let mut env = json!({ "command": command, "clientKey": client_key });
        if !data.is_null() {
            env["dataString"] = data.to_string().into();
        }
        Message::Text(env.to_string().into())
    };

    // the UDP side: a socket per local address, their datagrams all to one queue
    let (udp_tx, mut udp_rx) = mpsc::channel::<(Vec<u8>, SocketAddr, SocketAddr)>(256);
    let mut sockets = Vec::new();
    for ip in local_ips() {
        let Ok(s) = UdpSocket::bind(SocketAddr::new(ip, 0)).await else {
            continue;
        };
        let Ok(local) = s.local_addr() else { continue };
        let s = Arc::new(s);
        let (s2, tx) = (s.clone(), udp_tx.clone());
        tokio::spawn(async move {
            let mut buf = vec![0; 2000];
            while let Ok((n, from)) = s2.recv_from(&mut buf).await {
                if tx.send((buf[..n].to_vec(), from, local)).await.is_err() {
                    break;
                }
            }
        });
        sockets.push((local, s));
    }
    drop(udp_tx);

    str0m::crypto::from_feature_flags().install_process_default();
    let mut rtc = Rtc::builder()
        .set_ice_lite(true)
        .clear_codecs()
        .enable_h264(true)
        .build(Instant::now());
    for (a, _) in &sockets {
        if let Ok(c) = Candidate::host(*a, "udp") {
            let _ = rtc.add_local_candidate(c);
        }
    }

    let (in_tx, in_rx) = mpsc::channel::<In>(64);
    let (status_tx, mut status_rx) = mpsc::channel::<String>(8);
    tokio::spawn(control(p.addr, mac, device_name, in_rx, status_tx));

    let mut pending = None;
    let mut mid: Option<Mid> = None;
    let mut pt: Option<Pt> = None;
    let mut channels: HashMap<str0m::channel::ChannelId, String> = HashMap::new();
    let mut frames: Option<broadcast::Receiver<Frame>> = None;
    let mut starter = h264::Starter::default();
    let mut last_restart = Instant::now() - Duration::from_secs(10);
    // since when we wait for a frame to start from, and whether the page was told why it waits
    let mut watching_since = Instant::now();
    let mut told_shared = false;
    let t0 = Instant::now();
    let mut pinch_start: Option<f64> = None;
    log::info!("screen session for {serial} (receiver {client_key})");

    'session: loop {
        // everything str0m has to say, until it wants time to pass
        let timeout = loop {
            if !rtc.is_alive() {
                break 'session;
            }
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Transmit(t)) => {
                    if let Some((_, s)) = sockets.iter().find(|(a, _)| *a == t.source) {
                        let _ = s.send_to(&t.contents, t.destination).await;
                    }
                }
                Ok(Output::Event(e)) => match e {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        log::info!("screen session for {serial}: the page went");
                        break 'session;
                    }
                    Event::Connected => {
                        log::info!("screen session for {serial}: connected");
                        frames = Some(feeds.watch(&serial, p.addr.ip()));
                        watching_since = Instant::now();
                        starter.resync();
                    }
                    Event::ChannelOpen(id, label) => {
                        channels.insert(id, label);
                    }
                    Event::ChannelData(d) => {
                        let label = channels.get(&d.id).map(String::as_str).unwrap_or("");
                        let m = match label {
                            "tap" => serde_json::from_slice::<Point>(&d.data).ok().map(|q| {
                                let (x, y) = to_screen(&q, info.width, info.height);
                                In::Tap(x, y, q.is_long_press)
                            }),
                            "drag" => serde_json::from_slice::<Point>(&d.data).ok().map(|q| {
                                let (x, y) = to_screen(&q, info.width, info.height);
                                In::Drag(x, y, q.end)
                            }),
                            "hardkey" => serde_json::from_slice::<serde_json::Value>(&d.data)
                                .ok()
                                .and_then(|v| {
                                    v["key"].as_str().and_then(key_name).map(String::from)
                                })
                                .map(In::Key),
                            // no two fingers on 6633 that we know of: a pinch zooms with the keys
                            "pinch" => {
                                serde_json::from_slice::<Pinch>(&d.data).ok().and_then(|q| {
                                    let dist = (q.p1.x - q.p2.x).hypot(q.p1.y - q.p2.y).max(1.0);
                                    match q.phase.as_str() {
                                        "start" => {
                                            pinch_start = Some(dist);
                                            None
                                        }
                                        "end" => pinch_start.take().and_then(|d0| {
                                            let r = dist / d0;
                                            (r > 1.25).then(|| In::Key("zoomin".into())).or_else(
                                                || (r < 0.8).then(|| In::Key("zoomout".into())),
                                            )
                                        }),
                                        _ => None,
                                    }
                                })
                            }
                            _ => None, // files: not on these units
                        };
                        if let Some(m) = m {
                            let _ = in_tx.try_send(m);
                        }
                    }
                    // the browser lost track: start it again at the next whole picture
                    Event::KeyframeRequest(_) if starter.started() => {
                        log::debug!("screen session for {serial}: key frame asked for");
                        starter.resync();
                        watching_since = Instant::now();
                    }
                    _ => {}
                },
                Err(e) => {
                    log::info!("screen session for {serial}: {e}");
                    break 'session;
                }
            }
        };
        let wait = timeout
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));

        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            d = udp_rx.recv() => {
                let Some((buf, source, destination)) = d else { break 'session };
                if let Ok(contents) = buf.as_slice().try_into() {
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination, contents });
                    if rtc.accepts(&input) && rtc.handle_input(input).is_err() {
                        break 'session;
                    }
                }
            }
            f = async { frames.as_mut().unwrap().recv().await }, if frames.is_some() => match f {
                Ok(f) => {
                    let was = starter.started();
                    let data = starter.frame(&f.data, &f.params);
                    if !was && starter.started() {
                        log::debug!("screen session for {serial}: started, {:.1} s after joining", watching_since.elapsed().as_secs_f32());
                        if told_shared {
                            told_shared = false;
                            let _ = to_browser.send(send("status", json!("connected"))).await;
                        }
                    }
                    if let (Some(data), Some(mid), Some(pt)) = (data, mid, pt) {
                        if let Some(w) = rtc.writer(mid) {
                            let t = MediaTime::new(
                                (t0.elapsed().as_micros() * 9 / 100) as u64,
                                Frequency::NINETY_KHZ,
                            );
                            if let Err(e) = w.write(pt, Instant::now(), t, data) {
                                log::debug!("screen session for {serial}: {e}");
                            }
                        }
                    }
                }
                // too slow: frames were lost, so start again at the next whole picture
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    starter.resync();
                    watching_since = Instant::now();
                }
                Err(broadcast::error::RecvError::Closed) => {
                    frames = None;
                }
            },
            s = status_rx.recv() => if let Some(s) = s {
                if to_browser.send(send("status", json!(s))).await.is_err() {
                    break 'session;
                }
            },
            m = from_browser.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let Ok(env) = serde_json::from_str::<serde_json::Value>(t.as_str()) else { continue };
                    let data = env["dataString"]
                        .as_str()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                        .unwrap_or_default();
                    match env["command"].as_str() {
                        Some("receiver-online") if mid.is_none() => {
                            let mut api = rtc.sdp_api();
                            let m = api.add_media(MediaKind::Video, Direction::SendOnly, Some("nos".into()), Some("screen".into()), None);
                            api.add_channel("nos".into());
                            let Some((offer, p)) = api.apply() else { break 'session };
                            mid = Some(m);
                            pending = Some(p);
                            let offer = send("offer", json!({ "type": "offer", "sdp": offer.to_sdp_string() }));
                            if to_browser.send(offer).await.is_err() {
                                break 'session;
                            }
                        }
                        Some("answer") => {
                            let (Some(pend), Some(sdp)) = (pending.take(), data["sdp"].as_str()) else { continue };
                            let answer = match SdpAnswer::from_sdp_string(sdp) {
                                Ok(a) => a,
                                Err(e) => {
                                    log::info!("screen session for {serial}: a bad answer: {e}");
                                    break 'session;
                                }
                            };
                            if let Err(e) = rtc.sdp_api().accept_answer(pend, answer) {
                                log::info!("screen session for {serial}: answer refused: {e}");
                                break 'session;
                            }
                            pt = mid.and_then(|m| rtc.writer(m)).and_then(|w| {
                                w.payload_params()
                                    .find(|p| p.spec().codec == Codec::H264 && p.spec().format.packetization_mode == Some(1))
                                    .map(|p| p.pt())
                            });
                            if pt.is_none() {
                                log::info!("screen session for {serial}: the browser takes no H.264");
                                let _ = to_browser.send(send("status", json!("this browser does not play H.264"))).await;
                                break 'session;
                            }
                        }
                        // ice-lite: the browser finds us; its own candidates are not needed
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break 'session,
                Some(Ok(_)) => {}
            },
        }
        // A stream we cannot start from (one without whole pictures, or H.264 the starter does not
        // know): start ours again now and then for an IDR frame, which comes only when nobody else watches
        // (the vendor's app, for one), and say why the screen stays black.
        if frames.is_some()
            && !starter.started()
            && watching_since.elapsed() > Duration::from_secs(4)
            && last_restart.elapsed() > Duration::from_secs(5)
        {
            last_restart = Instant::now();
            feeds.restart(&serial);
            if !told_shared {
                told_shared = true;
                let text =
                    "no picture while another app watches this chartplotter's screen: close it";
                if to_browser.send(send("status", json!(text))).await.is_err() {
                    break;
                }
            }
        }
        if rtc.handle_input(Input::Timeout(Instant::now())).is_err() {
            break;
        }
    }
    rtc.disconnect();
    log::info!("screen session for {serial} closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_letterboxed_picture_to_pixels() {
        let p = |x, y| Point {
            x,
            y,
            client_width: 1000.0,
            client_height: 1000.0,
            is_long_press: false,
            end: false,
        };
        // 800×480 in a 1000×1000 box: scale 1.25, 200 px bars above and below
        assert_eq!(to_screen(&p(500.0, 500.0), 800, 480), (400, 240));
        assert_eq!(to_screen(&p(0.0, 200.0), 800, 480), (0, 0));
        assert_eq!(to_screen(&p(1000.0, 0.0), 800, 480), (799, 0));
    }

    #[test]
    fn mac_is_stable_and_local() {
        let m = client_mac("x");
        assert_eq!(m, client_mac("x"));
        assert_eq!(m[0] & 0x03, 0x02);
    }
}
