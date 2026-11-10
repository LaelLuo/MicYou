/*
 * MicYou — Turns your Android device into a high-quality PC microphone.
 * Copyright (C) 2026 LanRhyme <https://github.com/MicYou-Dev/MicYou>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 */

pub mod micyou {
    include!(concat!(env!("OUT_DIR"), "/micyou.rs"));
}

pub const PACKET_MAGIC: i32 = 0x4D696359; // "MicY"
pub const UDP_PACKET_MAGIC: i32 = 0x4D696355; // "MicU"

/// Audio buffer codecs carried in `AudioPacketMessage.codec`.
pub const CODEC_PCM: i32 = 0;
pub const CODEC_OPUS: i32 = 1;
/// Negotiated desktop playback format; values match Android ENCODING_PCM_16BIT.
pub const DOWNLINK_SAMPLE_RATE: i32 = 48_000;
pub const DOWNLINK_CHANNEL_COUNT: i32 = 2;
pub const DOWNLINK_AUDIO_FORMAT: i32 = 2;
pub const DOWNLINK_FRAME_MS: usize = 20;
pub const DOWNLINK_FRAME_BYTES: usize = 3_840;
pub const PORT: u16 = 9123;
pub const UDP_PORT: u16 = 9124;
pub const MDNS_SERVICE_TYPE: &str = "_micyou._tcp.local.";
pub const MDNS_WEB_SERVICE_TYPE: &str = "_micyou-web._tcp.local.";
pub const HANDSHAKE_CLIENT_STR: &[u8] = b"MicYouCheck1";
pub const HANDSHAKE_SERVER_STR: &[u8] = b"MicYouCheck2";

#[cfg(test)]
mod tests {
    use super::*;
    use micyou::*;
    use prost::Message;

    fn fixture(name: &str) -> Vec<u8> {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/duplex-wire.json")).unwrap();
        let hex = fixtures[name].as_str().unwrap();
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[derive(Clone, PartialEq, Message)]
    struct OfficialConnect {
        #[prost(int64, tag = "1")]
        session_id: i64,
        #[prost(string, tag = "2")]
        platform: String,
    }

    #[derive(Clone, PartialEq, Message)]
    struct OfficialStats {
        #[prost(int32, tag = "1")]
        rtt_ms: i32,
        #[prost(float, tag = "2")]
        jitter_ms: f32,
        #[prost(float, tag = "3")]
        packet_loss: f32,
        #[prost(int32, tag = "4")]
        latency_ms: i32,
    }

    #[derive(Clone, PartialEq, Message)]
    struct OfficialWrapper {
        #[prost(message, optional, tag = "2")]
        connect: Option<OfficialConnect>,
        #[prost(message, optional, tag = "8")]
        stats: Option<OfficialStats>,
    }

    #[test]
    fn connect_fixtures_roundtrip_and_official_schema_keeps_session() {
        let old = MessageWrapper::decode(fixture("legacyConnect").as_slice()).unwrap();
        assert_eq!(old.connect.as_ref().unwrap().session_id, 42);
        assert!(!old.connect.as_ref().unwrap().supports_downlink);
        let new_bytes = fixture("duplexConnect");
        let new = MessageWrapper::decode(new_bytes.as_slice()).unwrap();
        assert!(new.connect.as_ref().unwrap().supports_downlink);
        assert_eq!(new.encode_to_vec(), new_bytes);
        let official = OfficialWrapper::decode(new_bytes.as_slice()).unwrap();
        assert_eq!(official.connect.unwrap().session_id, 42);
        assert!(OfficialWrapper::decode(fixture("activeState").as_slice())
            .unwrap()
            .connect
            .is_none());
    }

    #[test]
    fn official_platform_is_not_a_downlink_capability() {
        let official_bytes = OfficialWrapper {
            connect: Some(OfficialConnect {
                session_id: 42,
                platform: "android".into(),
            }),
            stats: None,
        }
        .encode_to_vec();
        let current = MessageWrapper::decode(official_bytes.as_slice())
            .unwrap()
            .connect
            .unwrap();
        assert_eq!(current.session_id, 42);
        assert_eq!(current.platform, "android");
        assert!(!current.supports_downlink);
        assert!(!current.supports_playback_routing);

        let duplex_bytes = MessageWrapper {
            connect: Some(ConnectMessage {
                session_id: 42,
                platform: "android".into(),
                supports_downlink: true,
                supports_playback_routing: true,
            }),
            ..Default::default()
        }
        .encode_to_vec();
        let official = OfficialWrapper::decode(duplex_bytes.as_slice())
            .unwrap()
            .connect
            .unwrap();
        assert_eq!(official.session_id, 42);
        assert_eq!(official.platform, "android");
    }

    #[test]
    fn official_stats_uses_field_8_and_is_not_a_downlink_request() {
        let official_bytes = OfficialWrapper {
            connect: None,
            stats: Some(OfficialStats {
                rtt_ms: 12,
                jitter_ms: 1.5,
                packet_loss: 0.25,
                latency_ms: 40,
            }),
        }
        .encode_to_vec();
        let current = MessageWrapper::decode(official_bytes.as_slice()).unwrap();
        let stats = current.stats.unwrap();
        assert_eq!(stats.rtt_ms, 12);
        assert_eq!(stats.latency_ms, 40);
        assert!(current.downlink_request.is_none());
        assert!(current.downlink_state.is_none());
        assert!(current.downlink_audio.is_none());

        let request_bytes = fixture("enableRequest");
        let official = OfficialWrapper::decode(request_bytes.as_slice()).unwrap();
        assert!(official.stats.is_none());
        assert!(MessageWrapper::decode(request_bytes.as_slice())
            .unwrap()
            .downlink_request
            .is_some());
    }

    #[test]
    fn shared_android_rust_wire_fixtures_preserve_tags_and_fields() {
        let request_bytes = fixture("enableRequest");
        let request = MessageWrapper::decode(request_bytes.as_slice()).unwrap();
        assert_eq!(
            request.downlink_request,
            Some(DownlinkRequestMessage {
                session_id: 42,
                request_id: 1,
                enabled: true,
                playback_error: String::new(),
            })
        );
        assert_eq!(request.encode_to_vec(), request_bytes);

        let state_bytes = fixture("activeState");
        let state = MessageWrapper::decode(state_bytes.as_slice()).unwrap();
        assert_eq!(
            state.downlink_state,
            Some(DownlinkStateMessage {
                session_id: 42,
                request_id: 1,
                stream_id: 2,
                supported: true,
                active: true,
                error: String::new(),
                sample_rate: DOWNLINK_SAMPLE_RATE,
                channel_count: DOWNLINK_CHANNEL_COUNT,
                audio_format: DOWNLINK_AUDIO_FORMAT,
                supports_playback_routing: false,
                route_state: String::new(),
                route_error: String::new(),
            })
        );
        assert_eq!(state.encode_to_vec(), state_bytes);

        // Deliberately tiny PCM fixture tests byte preservation, not frame acceptance.
        let audio_bytes = fixture("audioWireSample");
        let audio = MessageWrapper::decode(audio_bytes.as_slice()).unwrap();
        assert_eq!(
            audio.downlink_audio,
            Some(DownlinkAudioMessage {
                session_id: 42,
                stream_id: 2,
                sequence_number: 7,
                pcm: vec![0, 128, 255, 127, 0, 0, 0, 64],
            })
        );
        assert_eq!(audio.encode_to_vec(), audio_bytes);
    }

    #[test]
    fn truncated_length_delimited_payload_is_rejected() {
        let mut bytes = fixture("audioWireSample");
        bytes.pop();
        assert!(MessageWrapper::decode(bytes.as_slice()).is_err());
    }

    #[test]
    fn playback_routing_fixtures_roundtrip() {
        let connect_bytes = fixture("routingConnect");
        let connect = MessageWrapper::decode(connect_bytes.as_slice()).unwrap();
        assert_eq!(
            connect.connect,
            Some(ConnectMessage {
                session_id: 42,
                supports_downlink: true,
                supports_playback_routing: true,
                platform: String::new(),
            })
        );
        assert_eq!(connect.encode_to_vec(), connect_bytes);
        assert_eq!(
            OfficialWrapper::decode(connect_bytes.as_slice())
                .unwrap()
                .connect
                .unwrap()
                .session_id,
            42
        );

        let request_bytes = fixture("playbackErrorRequest");
        let request = MessageWrapper::decode(request_bytes.as_slice()).unwrap();
        assert_eq!(
            request.downlink_request,
            Some(DownlinkRequestMessage {
                session_id: 42,
                request_id: 3,
                enabled: false,
                playback_error: "playback_failed".into(),
            })
        );
        assert_eq!(request.encode_to_vec(), request_bytes);
        assert!(OfficialWrapper::decode(request_bytes.as_slice())
            .unwrap()
            .stats
            .is_none());

        let state_bytes = fixture("routingFailedState");
        let state = MessageWrapper::decode(state_bytes.as_slice()).unwrap();
        assert_eq!(
            state.downlink_state,
            Some(DownlinkStateMessage {
                session_id: 42,
                request_id: 3,
                stream_id: 2,
                supported: true,
                active: false,
                error: "playback_failed".into(),
                sample_rate: 48_000,
                channel_count: 2,
                audio_format: 2,
                supports_playback_routing: true,
                route_state: "restore_failed".into(),
                route_error: "route_restore_failed".into(),
            })
        );
        assert_eq!(state.encode_to_vec(), state_bytes);
    }
}
