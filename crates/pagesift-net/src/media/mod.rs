//! Native multimedia stream ingestion, manifest parsing, concurrent fetching, and pure Rust container verification.

pub mod chunk_fetcher;
pub mod m3u8_parser;
pub mod mpd_parser;
pub mod stream_verifier;

pub use chunk_fetcher::{ChunkFetcher, FetchError, FetchedChunk};
pub use m3u8_parser::{
    HlsMasterPlaylist, HlsMediaPlaylist, HlsPlaylist, HlsSegment, HlsVariant, M3u8Error, parse_m3u8,
};
pub use mpd_parser::{
    DashAdaptationSet, DashError, DashManifest, DashRepresentation, DashSegment, parse_mpd,
};
pub use stream_verifier::{StreamVerifier, VerificationReport};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_m3u8_master_and_media_parsing() {
        let master = r#"#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=640x360,CODECS="avc1.4d401f,mp4a.40.2"
mid.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720,CODECS="avc1.4d401f,mp4a.40.2"
high.m3u8
"#;
        match parse_m3u8(master, None).unwrap() {
            HlsPlaylist::Master(m) => {
                assert_eq!(m.variants.len(), 2);
                assert_eq!(m.variants[0].bandwidth, 1280000);
                assert_eq!(m.variants[0].resolution, Some((640, 360)));
                assert_eq!(m.variants[0].uri, "mid.m3u8");
                assert_eq!(m.variants[1].bandwidth, 2560000);
            }
            _ => panic!("expected master playlist"),
        }

        let media = r#"#EXTM3U
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:9.009,
segment1.ts
#EXTINF:9.009,
segment2.ts
#EXT-X-ENDLIST
"#;
        match parse_m3u8(media, None).unwrap() {
            HlsPlaylist::Media(m) => {
                assert_eq!(m.target_duration_s, 10.0);
                assert_eq!(m.segments.len(), 2);
                assert_eq!(m.segments[0].uri, "segment1.ts");
                assert_eq!(m.segments[0].duration_s, 9.009);
                assert!(m.is_vod);
            }
            _ => panic!("expected media playlist"),
        }
    }

    #[test]
    fn test_stream_verifier_ts_packet_check() {
        // Construct 2 valid 188-byte TS packets
        let mut data = vec![0u8; 188 * 2];
        // Packet 0: sync byte 0x47, PID 0x100, CC = 0
        data[0] = 0x47;
        data[1] = 0x01;
        data[2] = 0x00;
        data[3] = 0x10; // payload only, CC = 0

        // Packet 1: sync byte 0x47, PID 0x100, CC = 1
        data[188] = 0x47;
        data[189] = 0x01;
        data[190] = 0x00;
        data[191] = 0x11; // payload only, CC = 1

        let report = StreamVerifier::verify_mpeg_ts(&data);
        assert_eq!(report.total_packets_or_boxes, 2);
        assert_eq!(report.continuity_errors, 0);
        assert!(report.is_intact);
    }
}
