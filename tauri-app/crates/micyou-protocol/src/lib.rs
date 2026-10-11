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
    struct LegacyConnect {
        #[prost(int64, tag = "1")]
        session_id: i64,
    }

    #[derive(Clone, PartialEq, Message)]
    struct LegacyWrapper {
        #[prost(message, optional, tag = "2")]
        connect: Option<LegacyConnect>,
        #[prost(message, optional, tag = "8")]
        request: Option<LegacyRequest>,
        #[prost(message, optional, tag = "9")]
        state: Option<LegacyState>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct LegacyRequest {
        #[prost(int64, tag = "1")]
        session_id: i64,
        #[prost(int64, tag = "2")]
        request_id: i64,
        #[prost(bool, tag = "3")]
        enabled: bool,
    }

    #[derive(Clone, PartialEq, Message)]
    struct LegacyState {
        #[prost(int64, tag = "1")]
        session_id: i64,
        #[prost(int64, tag = "2")]
        request_id: i64,
        #[prost(int64, tag = "3")]
        stream_id: i64,
        #[prost(bool, tag = "4")]
        supported: bool,
        #[prost(bool, tag = "5")]
        active: bool,
        #[prost(string, tag = "6")]
        error: String,
    }

    #[test]
    fn old_and_new_connect_decode_without_changing_legacy_session() {
        let old = MessageWrapper::decode(fixture("legacyConnect").as_slice()).unwrap();
        assert_eq!(old.connect.as_ref().unwrap().session_id, 42);
        assert!(!old.connect.unwrap().supports_downlink);
        let new_bytes = fixture("duplexConnect");
        let new = MessageWrapper::decode(new_bytes.as_slice()).unwrap();
        assert!(new.connect.as_ref().unwrap().supports_downlink);
        assert_eq!(new.encode_to_vec(), new_bytes);
        let legacy = LegacyWrapper::decode(new_bytes.as_slice()).unwrap();
        assert_eq!(legacy.connect.unwrap().session_id, 42);
        // Unknown top-level tags also remain safely skippable by an old peer.
        assert!(LegacyWrapper::decode(fixture("activeState").as_slice())
            .unwrap()
            .connect
            .is_none());
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
    fn playback_routing_fixtures_roundtrip_and_old_peer_keeps_the_stop_semantics() {
        let connect_bytes = fixture("routingConnect");
        let connect = MessageWrapper::decode(connect_bytes.as_slice()).unwrap();
        assert_eq!(
            connect.connect,
            Some(ConnectMessage {
                session_id: 42,
                supports_downlink: true,
                supports_playback_routing: true,
            })
        );
        assert_eq!(connect.encode_to_vec(), connect_bytes);
        assert_eq!(
            LegacyWrapper::decode(connect_bytes.as_slice())
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
        let old = LegacyWrapper::decode(request_bytes.as_slice())
            .unwrap()
            .request
            .unwrap();
        assert_eq!(
            (old.session_id, old.request_id, old.enabled),
            (42, 3, false)
        );

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
        let old = LegacyWrapper::decode(state_bytes.as_slice())
            .unwrap()
            .state
            .unwrap();
        assert_eq!((old.session_id, old.request_id, old.stream_id), (42, 3, 2));
        assert!(old.supported && !old.active);
        assert_eq!(old.error, "playback_failed");
    }
}
