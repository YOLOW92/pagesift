//! Pure Safe Rust container & timestamp integrity verifier for MPEG-TS and Fragmented MP4 (fMP4).
//! Validates 188-byte TS packet sync, PID continuity counters, PES PTS/DTS monotonicity, and fMP4 box bounds.
//! Operates completely in pure Safe Rust without any FFmpeg or external C runtime.

use std::collections::HashMap;

/// Verification report detailing media stream structural and timestamp integrity.
#[derive(Clone, Debug, PartialEq)]
pub struct VerificationReport {
    pub format: String,
    pub total_packets_or_boxes: usize,
    pub continuity_errors: u32,
    pub pts_monotonic: bool,
    pub dts_monotonic: bool,
    pub is_intact: bool,
    pub first_pts: Option<u64>,
    pub last_pts: Option<u64>,
}

pub struct StreamVerifier;

impl StreamVerifier {
    /// Verify an MPEG-TS byte sequence.
    /// Checks 188-byte sync word (0x47), continuity counter transitions, and PES PTS/DTS order.
    pub fn verify_mpeg_ts(data: &[u8]) -> VerificationReport {
        if data.is_empty() || data.len() % 188 != 0 {
            return VerificationReport {
                format: "MPEG-TS".into(),
                total_packets_or_boxes: data.len() / 188,
                continuity_errors: 1,
                pts_monotonic: false,
                dts_monotonic: false,
                is_intact: false,
                first_pts: None,
                last_pts: None,
            };
        }

        let total_packets = data.len() / 188;
        let mut continuity_errors = 0;
        let mut last_cc_per_pid: HashMap<u16, u8> = HashMap::new();
        let mut pts_list = Vec::new();
        let mut dts_list = Vec::new();

        for i in 0..total_packets {
            let offset = i * 188;
            let packet = &data[offset..offset + 188];

            // 1. Sync byte check
            if packet[0] != 0x47 {
                continuity_errors += 1;
                continue;
            }

            // 2. Parse PID and continuity counter
            let pid = (((packet[1] & 0x1F) as u16) << 8) | (packet[2] as u16);
            let payload_unit_start = (packet[1] & 0x40) != 0;
            let adaptation_control = (packet[3] >> 4) & 0x03;
            let cc = packet[3] & 0x0F;

            // Skip null packets (PID 0x1FFF)
            if pid == 0x1FFF {
                continue;
            }

            // Check CC if packet has payload
            if (adaptation_control & 0x01) != 0 {
                if let Some(prev_cc) = last_cc_per_pid.get(&pid) {
                    let expected = (prev_cc + 1) & 0x0F;
                    if cc != expected && cc != *prev_cc {
                        continuity_errors += 1;
                    }
                }
                last_cc_per_pid.insert(pid, cc);
            }

            // 3. If payload unit start, parse PES header for PTS/DTS
            if payload_unit_start && (adaptation_control & 0x01) != 0 {
                let mut pes_offset = 4;
                if (adaptation_control & 0x02) != 0 {
                    let adapt_len = packet[4] as usize;
                    pes_offset += 1 + adapt_len;
                }

                if pes_offset + 9 <= 188 {
                    let pes = &packet[pes_offset..];
                    // PES start code prefix: 0x000001
                    if pes[0] == 0x00 && pes[1] == 0x00 && pes[2] == 0x01 {
                        let stream_id = pes[3];
                        // Audio: 0xC0..0xDF, Video: 0xE0..0xEF
                        if (0xC0..=0xEF).contains(&stream_id) && pes.len() >= 9 {
                            let pts_dts_flags = (pes[7] >> 6) & 0x03;
                            if pts_dts_flags & 0x02 != 0 && pes.len() >= 14 {
                                let pts = parse_ts_timestamp(&pes[9..14]);
                                pts_list.push(pts);
                                if pts_dts_flags & 0x01 != 0 && pes.len() >= 19 {
                                    let dts = parse_ts_timestamp(&pes[14..19]);
                                    dts_list.push(dts);
                                }
                            }
                        }
                    }
                }
            }
        }

        let pts_monotonic = is_strictly_monotonic(&pts_list);
        let dts_monotonic = is_strictly_monotonic(&dts_list);
        let is_intact = continuity_errors == 0 && (pts_list.is_empty() || pts_monotonic);

        VerificationReport {
            format: "MPEG-TS".into(),
            total_packets_or_boxes: total_packets,
            continuity_errors,
            pts_monotonic,
            dts_monotonic,
            is_intact,
            first_pts: pts_list.first().copied(),
            last_pts: pts_list.last().copied(),
        }
    }

    /// Verify an fMP4 (fragmented MP4) stream byte sequence.
    /// Traverses top-level boxes: ftyp, moov, moof, mdat, checking boundaries and decode time.
    pub fn verify_fmp4(data: &[u8]) -> VerificationReport {
        let mut offset = 0;
        let mut box_count = 0;
        let mut has_ftyp_or_moof = false;
        let mut decode_times = Vec::new();
        let mut malformed = false;

        while offset + 8 <= data.len() {
            let size = u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize;
            let tag = &data[offset + 4..offset + 8];

            let box_size = if size == 1 {
                // 64-bit large box size
                if offset + 16 > data.len() {
                    malformed = true;
                    break;
                }
                u64::from_be_bytes([
                    data[offset + 8], data[offset + 9], data[offset + 10], data[offset + 11],
                    data[offset + 12], data[offset + 13], data[offset + 14], data[offset + 15]
                ]) as usize
            } else if size == 0 {
                data.len() - offset
            } else {
                size
            };

            if box_size < 8 || offset + box_size > data.len() {
                malformed = true;
                break;
            }

            match tag {
                b"ftyp" | b"styp" | b"moov" => {
                    has_ftyp_or_moof = true;
                }
                b"moof" => {
                    has_ftyp_or_moof = true;
                    // Scan moof for tfdt (track fragment decode time)
                    if let Some(tfdt_time) = find_tfdt_time(&data[offset..offset + box_size]) {
                        decode_times.push(tfdt_time);
                    }
                }
                b"mdat" => {}
                _ => {}
            }

            box_count += 1;
            offset += box_size;
        }

        let times_monotonic = is_strictly_monotonic(&decode_times);
        let is_intact = !malformed && has_ftyp_or_moof && (decode_times.is_empty() || times_monotonic);

        VerificationReport {
            format: "fMP4".into(),
            total_packets_or_boxes: box_count,
            continuity_errors: if malformed { 1 } else { 0 },
            pts_monotonic: times_monotonic,
            dts_monotonic: times_monotonic,
            is_intact,
            first_pts: decode_times.first().copied(),
            last_pts: decode_times.last().copied(),
        }
    }
}

fn parse_ts_timestamp(bytes: &[u8]) -> u64 {
    let b0 = bytes[0] as u64;
    let b1 = bytes[1] as u64;
    let b2 = bytes[2] as u64;
    let b3 = bytes[3] as u64;
    let b4 = bytes[4] as u64;

    ((b0 & 0x0E) << 29)
        | ((b1 & 0xFF) << 22)
        | ((b2 & 0xFE) << 14)
        | ((b3 & 0xFF) << 7)
        | ((b4 & 0xFE) >> 1)
}

fn is_strictly_monotonic(values: &[u64]) -> bool {
    if values.len() <= 1 {
        return true;
    }
    for w in values.windows(2) {
        if w[1] <= w[0] {
            return false;
        }
    }
    true
}

fn find_tfdt_time(moof_data: &[u8]) -> Option<u64> {
    let mut i = 8;
    while i + 8 <= moof_data.len() {
        let size = u32::from_be_bytes([moof_data[i], moof_data[i + 1], moof_data[i + 2], moof_data[i + 3]]) as usize;
        let tag = &moof_data[i + 4..i + 8];
        if size < 8 || i + size > moof_data.len() {
            break;
        }
        if tag == b"traf" {
            // Search inside traf
            let mut j = i + 8;
            while j + 8 <= i + size {
                let sub_size = u32::from_be_bytes([moof_data[j], moof_data[j + 1], moof_data[j + 2], moof_data[j + 3]]) as usize;
                let sub_tag = &moof_data[j + 4..j + 8];
                if sub_size < 8 || j + sub_size > i + size {
                    break;
                }
                if sub_tag == b"tfdt" && j + 12 <= i + size {
                    let version = moof_data[j + 8];
                    if version == 1 && j + 20 <= i + size {
                        return Some(u64::from_be_bytes([
                            moof_data[j + 12], moof_data[j + 13], moof_data[j + 14], moof_data[j + 15],
                            moof_data[j + 16], moof_data[j + 17], moof_data[j + 18], moof_data[j + 19],
                        ]));
                    } else if version == 0 && j + 16 <= i + size {
                        return Some(u32::from_be_bytes([
                            moof_data[j + 12], moof_data[j + 13], moof_data[j + 14], moof_data[j + 15],
                        ]) as u64);
                    }
                }
                j += sub_size;
            }
        }
        i += size;
    }
    None
}
