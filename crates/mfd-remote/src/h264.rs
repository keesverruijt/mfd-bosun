//! A NOS unit's H.264 has one IDR frame, when its stream starts; after that, about once a second, SPS, PPS
//! and a whole picture of I slices, but as an ordinary (non-IDR) frame. A decoder that starts anywhere
//! (ffmpeg, in the vendor's app) starts there; a browser's WebRTC waits for an IDR. So for a viewer who joins
//! a running stream, [`Starter`] turns the next such I frame into an IDR (`frame_num` 0, an `idr_pic_id`,
//! the IDR form of `dec_ref_pic_marking`; the slice data as it was), and then numbers the frames after it
//! on from there (`frame_num` less that of the I frame), as if the stream had started with it.

/// What the slice headers need of the SPS and PPS.
#[derive(Clone, Debug, PartialEq)]
struct Params {
    separate_colour_plane: bool,
    log2_max_frame_num: u32,
    poc_type: u32,
    log2_max_poc_lsb: u32,
    delta_pic_order_always_zero: bool,
    frame_mbs_only: bool,
    cabac: bool,
    bottom_field_pic_order_in_frame_present: bool,
    redundant_pic_cnt_present: bool,
    deblocking_filter_control_present: bool,
}

struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8]) -> Self {
        Reader { d, pos: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        let b = *self.d.get(self.pos >> 3)?;
        let v = (b >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0, |v, _| Some(v << 1 | self.bit()?))
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k & 1 == 1 {
            k.div_ceil(2) as i32
        } else {
            -((k / 2) as i32)
        })
    }
}

#[derive(Default)]
struct Writer {
    d: Vec<u8>,
    pos: usize,
}

impl Writer {
    fn bit(&mut self, v: u32) {
        if self.pos & 7 == 0 {
            self.d.push(0);
        }
        if v & 1 == 1 {
            *self.d.last_mut().unwrap() |= 0x80 >> (self.pos & 7);
        }
        self.pos += 1;
    }
    fn bits(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit(v >> i);
        }
    }
    fn ue(&mut self, v: u32) {
        let v1 = v as u64 + 1;
        let n = 64 - v1.leading_zeros();
        self.bits(0, n - 1);
        for i in (0..n).rev() {
            self.bit((v1 >> i) as u32);
        }
    }
    fn se(&mut self, v: i32) {
        self.ue(if v > 0 {
            2 * v as u32 - 1
        } else {
            (-2 * v) as u32
        });
    }
    fn aligned(&self) -> bool {
        self.pos & 7 == 0
    }
}

/// The RBSP of a NAL unit (without its header byte): emulation prevention bytes taken out.
fn unescape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn escape(d: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
}

fn parse_sps(rbsp: &[u8]) -> Option<SpsPart> {
    let mut r = Reader::new(rbsp);
    let profile = r.bits(8)?;
    r.bits(16)?; // constraints, level
    r.ue()?; // sps id
    let mut separate_colour_plane = false;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        if r.ue()? == 3 {
            separate_colour_plane = r.bit()? == 1;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            return None; // scaling matrices: not worth it here
        }
    }
    let log2_max_frame_num = r.ue()? + 4;
    let poc_type = r.ue()?;
    let (mut log2_max_poc_lsb, mut delta_pic_order_always_zero) = (0, false);
    match poc_type {
        0 => log2_max_poc_lsb = r.ue()? + 4,
        1 => {
            delta_pic_order_always_zero = r.bit()? == 1;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?;
    r.ue()?;
    r.ue()?;
    let frame_mbs_only = r.bit()? == 1;
    Some(SpsPart {
        separate_colour_plane,
        log2_max_frame_num,
        poc_type,
        log2_max_poc_lsb,
        delta_pic_order_always_zero,
        frame_mbs_only,
    })
}

#[derive(Clone, Debug, PartialEq)]
struct SpsPart {
    separate_colour_plane: bool,
    log2_max_frame_num: u32,
    poc_type: u32,
    log2_max_poc_lsb: u32,
    delta_pic_order_always_zero: bool,
    frame_mbs_only: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct PpsPart {
    cabac: bool,
    bottom_field_pic_order_in_frame_present: bool,
    redundant_pic_cnt_present: bool,
    deblocking_filter_control_present: bool,
}

fn parse_pps(rbsp: &[u8]) -> Option<PpsPart> {
    let mut r = Reader::new(rbsp);
    r.ue()?;
    r.ue()?;
    let cabac = r.bit()? == 1;
    let bottom_field_pic_order_in_frame_present = r.bit()? == 1;
    if r.ue()? != 0 {
        return None; // slice groups
    }
    r.ue()?;
    r.ue()?;
    r.bit()?; // weighted_pred: P slices only
    r.bits(2)?;
    r.se()?;
    r.se()?;
    r.se()?;
    let deblocking_filter_control_present = r.bit()? == 1;
    r.bit()?;
    let redundant_pic_cnt_present = r.bit()? == 1;
    Some(PpsPart {
        cabac,
        bottom_field_pic_order_in_frame_present,
        redundant_pic_cnt_present,
        deblocking_filter_control_present,
    })
}

/// The index of the last 1 bit (the RBSP stop bit).
fn stop_bit(d: &[u8]) -> Option<usize> {
    let i = d.iter().rposition(|&b| b != 0)?;
    Some(i * 8 + 7 - d[i].trailing_zeros() as usize)
}

/// A slice's `frame_num`, and its slice type.
fn slice_info(rbsp: &[u8], p: &Params) -> Option<(u32, u32)> {
    let mut r = Reader::new(rbsp);
    r.ue()?;
    let slice_type = r.ue()? % 5;
    r.ue()?;
    if p.separate_colour_plane {
        r.bits(2)?;
    }
    Some((r.bits(p.log2_max_frame_num)?, slice_type))
}

/// A slice header written again: `frame_num` changed; for `idr`, an I slice made an IDR one. The slice data
/// as it was. None when the slice is not one we can do that to.
fn rewrite_slice(
    rbsp: &[u8],
    p: &Params,
    frame_num: u32,
    idr: Option<u32>,
    ref_idc: u8,
) -> Option<Vec<u8>> {
    let mut r = Reader::new(rbsp);
    let mut w = Writer::default();
    let first_mb = r.ue()?;
    w.ue(first_mb);
    let slice_type = r.ue()?;
    w.ue(slice_type);
    w.ue(r.ue()?); // pps id
    if p.separate_colour_plane {
        w.bits(r.bits(2)?, 2);
    }
    r.bits(p.log2_max_frame_num)?;
    w.bits(frame_num, p.log2_max_frame_num);
    let Some(idr_pic_id) = idr else {
        // only frame_num changed, at the same width: the rest as it was
        let end = rbsp.len() * 8;
        while r.pos < end {
            w.bit(r.bit()?);
        }
        return Some(w.d);
    };
    if slice_type % 5 != 2 || ref_idc == 0 {
        return None;
    }
    let mut field = false;
    if !p.frame_mbs_only {
        field = r.bit()? == 1;
        w.bit(field as u32);
        if field {
            w.bit(r.bit()?);
        }
    }
    w.ue(idr_pic_id);
    if p.poc_type == 0 {
        w.bits(r.bits(p.log2_max_poc_lsb)?, p.log2_max_poc_lsb);
        if p.bottom_field_pic_order_in_frame_present && !field {
            w.se(r.se()?);
        }
    } else if p.poc_type == 1 && !p.delta_pic_order_always_zero {
        w.se(r.se()?);
        if p.bottom_field_pic_order_in_frame_present && !field {
            w.se(r.se()?);
        }
    }
    if p.redundant_pic_cnt_present {
        w.ue(r.ue()?);
    }
    // an I slice: no reference lists, no weights; then dec_ref_pic_marking
    if r.bit()? != 0 {
        return None; // adaptive marking: not in a plain I frame
    }
    w.bits(0, 2); // no_output_of_prior_pics, long_term_reference
    if p.cabac {
        // the rest of the header, then the data from the next byte boundary on
        w.se(r.se()?); // slice_qp_delta
        if p.deblocking_filter_control_present {
            let idc = r.ue()?;
            w.ue(idc);
            if idc != 1 {
                w.se(r.se()?);
                w.se(r.se()?);
            }
        }
        while r.pos & 7 != 0 {
            r.bit()?;
        }
        while !w.aligned() {
            w.bit(1); // cabac_alignment_one_bit
        }
        w.d.extend_from_slice(rbsp.get(r.pos / 8..)?);
    } else {
        // CAVLC: the data bit for bit, then a new stop bit
        let stop = stop_bit(rbsp)?;
        while r.pos < stop {
            w.bit(r.bit()?);
        }
        w.bit(1);
        while !w.aligned() {
            w.bit(0);
        }
    }
    Some(w.d)
}

/// The NAL units of an Annex B access unit (without their start codes).
fn nal_units(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(d.len(), |&n| n - 3);
        while e > s && d[e - 1] == 0 {
            e -= 1; // the leading zero of a 4-byte start code, or trailing zeros
        }
        if e > s {
            out.push(&d[s..e]);
        }
    }
    out
}

/// Makes a stream that a viewer joins in the middle start (see the module).
#[derive(Default)]
pub struct Starter {
    sps: Option<SpsPart>,
    pps: Option<PpsPart>,
    /// frame_num of the frame we started from; None: not started
    offset: Option<u32>,
    idr_pic_id: u32,
}

impl Starter {
    /// Start again, at the next frame that can be made a start.
    pub fn resync(&mut self) {
        self.offset = None;
    }

    pub fn started(&self) -> bool {
        self.offset.is_some()
    }

    /// The access unit to send (maybe rewritten), or None while there is nothing to start from yet.
    /// `params`: the stream's SPS and PPS (Annex B), for when the access unit has none of its own.
    pub fn frame(&mut self, au: &[u8], params: &[u8]) -> Option<Vec<u8>> {
        let nals = nal_units(au);
        if self.offset.is_none() {
            for n in nal_units(params) {
                match n[0] & 0x1f {
                    7 => self.sps = parse_sps(&unescape(&n[1..])),
                    8 => self.pps = parse_pps(&unescape(&n[1..])),
                    _ => {}
                }
            }
        }
        for n in &nals {
            match n[0] & 0x1f {
                7 => self.sps = parse_sps(&unescape(&n[1..])),
                8 => self.pps = parse_pps(&unescape(&n[1..])),
                5 => {
                    // a real IDR: its numbers are right as they are
                    self.offset = Some(0);
                    return Some(au.to_vec());
                }
                _ => {}
            }
        }
        match self.offset {
            Some(0) => return Some(au.to_vec()),
            Some(off) => {
                return self.renumber(&nals, off).or_else(|| {
                    self.offset = None;
                    None
                })
            }
            None => {}
        }
        // a whole picture: all its slices I slices
        let p = self.params()?;
        let slices: Vec<_> = nals.iter().filter(|n| n[0] & 0x1f == 1).collect();
        let infos: Vec<_> = slices
            .iter()
            .map(|n| slice_info(&unescape(&n[1..]), &p))
            .collect::<Option<_>>()?;
        if infos.is_empty() || infos.iter().any(|&(_, t)| t != 2) {
            return None;
        }
        let frame_num = infos[0].0;
        let idr_pic_id = self.idr_pic_id;
        let mut out = Vec::with_capacity(au.len() + params.len() + 16);
        let has_params =
            nals.iter().any(|n| n[0] & 0x1f == 7) && nals.iter().any(|n| n[0] & 0x1f == 8);
        if !has_params {
            for n in nal_units(params) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(n);
            }
        }
        for n in &nals {
            let t = n[0] & 0x1f;
            out.extend_from_slice(&[0, 0, 0, 1]);
            if t == 1 {
                let rbsp = unescape(&n[1..]);
                let new = rewrite_slice(&rbsp, &p, 0, Some(idr_pic_id), n[0] >> 5)?;
                out.push(n[0] & 0xe0 | 5);
                escape(&new, &mut out);
            } else {
                out.extend_from_slice(n);
            }
        }
        self.idr_pic_id = (idr_pic_id + 1) % 2;
        self.offset = Some(frame_num);
        Some(out)
    }

    fn params(&self) -> Option<Params> {
        let (s, q) = (self.sps.clone()?, self.pps.clone()?);
        Some(Params {
            separate_colour_plane: s.separate_colour_plane,
            log2_max_frame_num: s.log2_max_frame_num,
            poc_type: s.poc_type,
            log2_max_poc_lsb: s.log2_max_poc_lsb,
            delta_pic_order_always_zero: s.delta_pic_order_always_zero,
            frame_mbs_only: s.frame_mbs_only,
            cabac: q.cabac,
            bottom_field_pic_order_in_frame_present: q.bottom_field_pic_order_in_frame_present,
            redundant_pic_cnt_present: q.redundant_pic_cnt_present,
            deblocking_filter_control_present: q.deblocking_filter_control_present,
        })
    }

    fn renumber(&self, nals: &[&[u8]], off: u32) -> Option<Vec<u8>> {
        let p = self.params()?;
        let max = 1u32 << p.log2_max_frame_num;
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            if n[0] & 0x1f == 1 {
                let rbsp = unescape(&n[1..]);
                let (fnum, _) = slice_info(&rbsp, &p)?;
                let new = rewrite_slice(&rbsp, &p, (fnum + max - off) % max, None, n[0] >> 5)?;
                out.push(n[0]);
                escape(&new, &mut out);
            } else {
                out.extend_from_slice(n);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_round_trips() {
        let mut w = Writer::default();
        for v in [0, 1, 2, 7, 255, 1000] {
            w.ue(v);
        }
        for v in [0, 1, -1, 5, -300] {
            w.se(v);
        }
        let mut r = Reader::new(&w.d);
        for v in [0, 1, 2, 7, 255, 1000] {
            assert_eq!(r.ue(), Some(v));
        }
        for v in [0, 1, -1, 5, -300] {
            assert_eq!(r.se(), Some(v));
        }
    }

    #[test]
    fn escaping_round_trips() {
        let raw = [0, 0, 0, 1, 0, 0, 3, 0, 0, 2, 5];
        let mut e = Vec::new();
        escape(&raw, &mut e);
        assert!(!e.windows(3).any(|w| w[0] == 0 && w[1] == 0 && w[2] <= 2));
        assert_eq!(unescape(&e), raw);
    }

    /// `H264_IN`: access units, each after its u32 BE length; writes what a late joiner gets, as Annex B, to
    /// `H264_OUT` (for a decoder to check).
    #[test]
    #[ignore]
    fn starts_a_captured_stream() {
        let d = std::fs::read(std::env::var("H264_IN").unwrap()).unwrap();
        let mut s = Starter::default();
        let (mut out, mut i, mut sent) = (Vec::new(), 0, 0);
        while i + 4 <= d.len() {
            let n = u32::from_be_bytes(d[i..i + 4].try_into().unwrap()) as usize;
            if let Some(f) = s.frame(&d[i + 4..i + 4 + n], &[]) {
                out.extend_from_slice(&f);
                sent += 1;
            }
            i += 4 + n;
        }
        eprintln!("{sent} access units out");
        std::fs::write(std::env::var("H264_OUT").unwrap(), out).unwrap();
    }

    #[test]
    fn reads_the_vulcan_parameters() {
        // sprop-parameter-sets of a B&G Vulcan 7R: Z0JAHqaAyD2QAA==,aM48gAA=
        let sps = parse_sps(&unescape(
            &[0x42, 0x40, 0x1e, 0xa6, 0x80, 0xc8, 0x3d, 0x90, 0, 0][..],
        ))
        .unwrap();
        assert!(sps.frame_mbs_only);
        let pps = parse_pps(&unescape(&[0xce, 0x3c, 0x80, 0, 0][..])).unwrap();
        assert!(!pps.cabac);
    }
}
