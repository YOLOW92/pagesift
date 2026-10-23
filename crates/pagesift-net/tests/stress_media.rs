//! Adversarial stress testing for HLS/DASH manifest parsers and pure Rust stream container verifiers.

use pagesift_net::media::{HlsPlaylist, StreamVerifier, parse_m3u8, parse_mpd};

#[test]
fn test_stress_m3u8_large_manifest_and_byte_ranges() {
    let mut manifest =
        String::from("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:1000\n");

    let num_segments = 2000;
    for i in 0..num_segments {
        manifest.push_str(&format!(
            "#EXTINF:4.000,\n#EXT-X-BYTERANGE:102400@{}\nsegment.ts\n",
            i * 102400
        ));
    }
    manifest.push_str("#EXT-X-ENDLIST\n");

    let parsed = parse_m3u8(&manifest, None).unwrap();
    match parsed {
        HlsPlaylist::Media(m) => {
            assert_eq!(m.segments.len(), num_segments);
            assert!(m.is_vod);
            assert_eq!(m.segments[0].byte_range, Some((0, 102400)));
            assert_eq!(
                m.segments[num_segments - 1].byte_range,
                Some(((num_segments - 1) as u64 * 102400, 102400))
            );
        }
        _ => panic!("expected media playlist"),
    }
}

#[test]
fn test_stress_mpd_huge_timeline_expansion() {
    let mut mpd = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD minBufferTime="PT1S" mediaPresentationDuration="PT3600S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="90000" initialization="init.mp4" media="chunk-$Number%06d$.m4s" startNumber="1">
        <SegmentTimeline>
"#,
    );

    // 1,000 timeline entries with repeats
    for i in 0..1000 {
        mpd.push_str(&format!(
            r#"          <S t="{}" d="180000" r="1"/>"#,
            i * 360000
        ));
        mpd.push('\n');
    }

    mpd.push_str(
        r#"        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="4k" bandwidth="15000000" width="3840" height="2160" />
    </AdaptationSet>
  </Period>
</MPD>"#,
    );

    let manifest = parse_mpd(&mpd, None).unwrap();
    let rep = &manifest.adaptation_sets[0].representations[0];
    // 1 init + 1000 * 2 media segments = 2001 segments
    assert_eq!(rep.segments.len(), 2001);
    assert!(rep.segments[0].is_init);
    assert_eq!(rep.segments[1].uri, "chunk-000001.m4s");
    assert_eq!(rep.segments[2000].uri, "chunk-002000.m4s");
}

#[test]
fn test_stress_stream_verifier_packet_drop_injection() {
    let num_packets = 5000;
    let mut data = Vec::with_capacity(num_packets * 188);

    let mut cc = 0u8;
    let mut injected_drops = 0;

    for i in 0..num_packets {
        let mut pkt = [0u8; 188];
        pkt[0] = 0x47; // Sync byte
        pkt[1] = 0x01; // PID high byte
        pkt[2] = 0x00; // PID low byte (PID = 0x100)

        // Inject 12 continuity counter jumps
        if i % 400 == 0 && i > 0 {
            cc = (cc + 4) & 0x0F; // JUMP!
            injected_drops += 1;
        }

        pkt[3] = 0x10 | (cc & 0x0F); // Payload only + CC
        data.extend_from_slice(&pkt);

        cc = (cc + 1) & 0x0F;
    }

    let report = StreamVerifier::verify_mpeg_ts(&data);
    assert_eq!(report.total_packets_or_boxes, num_packets);
    assert_eq!(report.continuity_errors, injected_drops);
    assert!(
        !report.is_intact,
        "Packet drops must flag stream as corrupted"
    );
}

#[test]
fn test_stress_stream_verifier_pts_monotonicity_check() {
    let num_packets = 50;
    let mut data = Vec::with_capacity(num_packets * 188);

    for i in 0..num_packets {
        let mut pkt = [0u8; 188];
        pkt[0] = 0x47;
        pkt[1] = 0x41; // Payload unit start + PID 0x100
        pkt[2] = 0x00;
        pkt[3] = 0x10 | (i as u8 & 0x0F);

        // PES header at packet offset 4
        pkt[4] = 0x00;
        pkt[5] = 0x00;
        pkt[6] = 0x01; // PES prefix
        pkt[7] = 0xE0; // Video stream
        pkt[8] = 0x00;
        pkt[9] = 0x00;
        pkt[10] = 0x80;
        pkt[11] = 0x80; // PTS flag present
        pkt[12] = 0x05; // PES header data length

        // PTS timestamp encoding (monotonically increasing)
        let pts = (i as u64) * 3600;
        let b0 = ((pts >> 29) & 0x0E) as u8 | 0x21;
        let b1 = ((pts >> 22) & 0xFF) as u8;
        let b2 = ((pts >> 14) & 0xFE) as u8 | 0x01;
        let b3 = ((pts >> 7) & 0xFF) as u8;
        let b4 = ((pts << 1) & 0xFE) as u8 | 0x01;

        pkt[13] = b0;
        pkt[14] = b1;
        pkt[15] = b2;
        pkt[16] = b3;
        pkt[17] = b4;

        data.extend_from_slice(&pkt);
    }

    let pristine_report = StreamVerifier::verify_mpeg_ts(&data);
    assert!(pristine_report.pts_monotonic);
    assert!(pristine_report.is_intact);

    // Now corrupt timestamp in packet 25 to go backwards (time warp)
    let pkt25_offset = 25 * 188;
    data[pkt25_offset + 13] = 0x21;
    data[pkt25_offset + 14] = 0x00;
    data[pkt25_offset + 15] = 0x01;
    data[pkt25_offset + 16] = 0x00;
    data[pkt25_offset + 17] = 0x01; // PTS reset to near 0

    let warped_report = StreamVerifier::verify_mpeg_ts(&data);
    assert!(
        !warped_report.pts_monotonic,
        "PTS backwards warp must be caught"
    );
    assert!(!warped_report.is_intact);
}
