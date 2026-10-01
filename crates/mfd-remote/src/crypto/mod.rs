//! str0m's crypto: str0m-rust-crypto 0.6.0 (MIT or Apache-2.0), but with its own self-signed certificate.
//! str0m-rust-crypto makes that with dimpl's "rcgen" feature, which builds aws-lc (C, so no static Linux
//! binary from rust-lld alone). Here rcgen signs with ring, which this program has anyway. The other files
//! are as in str0m-rust-crypto; see https://github.com/algesten/dimpl/issues/164.

mod dtls;
mod sha1;
mod sha256;
mod srtp;

use dtls::RustCryptoDtlsProvider;
use sha1::RustCryptoSha1HmacProvider;
use sha256::RustCryptoSha256Provider;
use srtp::RustCryptoSrtpProvider;
use str0m_proto::crypto::dtls::DtlsCert;
use str0m_proto::crypto::CryptoProvider;

/// The crypto for an [`str0m::Rtc`]: DTLS (dimpl), SRTP, SHA-1 HMAC and SHA-256 in pure Rust.
pub fn provider() -> CryptoProvider {
    static SRTP: RustCryptoSrtpProvider = RustCryptoSrtpProvider;
    static SHA1_HMAC: RustCryptoSha1HmacProvider = RustCryptoSha1HmacProvider;
    static SHA256: RustCryptoSha256Provider = RustCryptoSha256Provider;
    static DTLS: RustCryptoDtlsProvider = RustCryptoDtlsProvider;

    CryptoProvider {
        srtp_provider: &SRTP,
        sha1_hmac_provider: &SHA1_HMAC,
        sha256_provider: &SHA256,
        dtls_provider: &DTLS,
    }
}

/// A DTLS certificate as dimpl's `generate_self_signed_certificate` makes it: ECDSA P-256, a year, and a
/// random serial (which Firefox wants unique, https://github.com/algesten/str0m/issues/517).
fn self_signed_certificate() -> Result<DtlsCert, rcgen::Error> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256,
    };
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut name = DistinguishedName::new();
    name.push(DnType::OrganizationName, "DTLS");
    name.push(DnType::CommonName, "DTLS Peer");
    params.distinguished_name = name;
    params.is_ca = IsCa::NoCa;
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = params.not_before + time::Duration::days(365);
    let mut serial = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut serial)
        .map_err(|_| rcgen::Error::RingUnspecified)?;
    params.serial_number = Some(serial.to_vec().into());
    let cert = params.self_signed(&key_pair)?;
    Ok(DtlsCert {
        certificate: cert.der().to_vec(),
        private_key: key_pair.serialize_der(),
    })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use str0m::format::Codec;
    use str0m::media::{Direction, MediaKind, MediaTime};
    use str0m::net::Receive;
    use str0m::{Candidate, Event, Input, Output, Rtc};
    use str0m_proto::crypto::DtlsVersion;

    /// Everything `from` sends, to `to`; `from`'s events; and when it wants a timeout.
    fn pump(from: &mut Rtc, to: &mut Rtc, now: Instant, events: &mut Vec<Event>) -> Instant {
        loop {
            match from.poll_output().unwrap() {
                Output::Timeout(t) => return t,
                Output::Transmit(t) => {
                    let r = Receive {
                        proto: t.proto,
                        source: t.source,
                        destination: t.destination,
                        contents: (&t.contents[..]).try_into().unwrap(),
                    };
                    to.handle_input(Input::Receive(now, r)).unwrap();
                }
                Output::Event(e) => events.push(e),
            }
        }
    }

    /// Two peers on this provider: ICE, DTLS (with our certificate), then media both ways over SRTP.
    fn connect_and_send(version: DtlsVersion) {
        let start = Instant::now();
        let peer = || {
            Rtc::builder()
                .set_crypto_provider(Arc::new(super::provider()))
                .set_dtls_version(version)
                .clear_codecs()
                .enable_opus(true, false)
                .build(start)
        };
        let (mut l, mut r) = (peer(), peer());
        let a = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
        let b = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
        l.add_local_candidate(a);
        r.add_local_candidate(b);
        let mut change = l.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, pending) = change.apply().unwrap();
        let answer = r.sdp_api().accept_offer(offer).unwrap();
        l.sdp_api().accept_answer(pending, answer).unwrap();

        let (mut el, mut er) = (Vec::new(), Vec::new());
        let mut now = start;
        let mut sent = 0u32;
        while now - start < Duration::from_secs(10) {
            if l.is_connected() && r.is_connected() && sent < 50 {
                for rtc in [&mut l, &mut r] {
                    let pt = rtc
                        .codec_config()
                        .iter()
                        .find(|p| p.spec().codec == Codec::Opus)
                        .unwrap()
                        .pt();
                    let time = MediaTime::from_90khz(sent as u64 * 960);
                    rtc.writer(mid)
                        .unwrap()
                        .write(pt, now, time, vec![sent as u8; 80])
                        .unwrap();
                }
                sent += 1;
            }
            let tl = pump(&mut l, &mut r, now, &mut el);
            let tr = pump(&mut r, &mut l, now, &mut er);
            now = (tl.min(tr))
                .max(now + Duration::from_millis(1))
                .min(now + Duration::from_millis(20));
            l.handle_input(Input::Timeout(now)).unwrap();
            r.handle_input(Input::Timeout(now)).unwrap();
        }
        assert!(
            l.is_connected() && r.is_connected(),
            "{version:?}: no connection"
        );
        let media = |e: &[Event]| {
            e.iter()
                .filter(|e| matches!(e, Event::MediaData(_)))
                .count()
        };
        assert!(
            media(&el) > 40 && media(&er) > 40,
            "{version:?}: media {} and {}",
            media(&el),
            media(&er)
        );
    }

    #[test]
    fn dtls12() {
        connect_and_send(DtlsVersion::Dtls12);
    }

    #[test]
    fn dtls13() {
        connect_and_send(DtlsVersion::Dtls13);
    }

    #[test]
    fn dtls_auto() {
        connect_and_send(DtlsVersion::Auto);
    }
}
