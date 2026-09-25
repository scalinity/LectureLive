//! Test doubles shared by the integration tests (spec §11): fake STT websocket and REST servers,
//! recorded fixtures and synthetic speech.
#![allow(dead_code)]
pub mod fake_rest;
pub mod fake_stt;
pub mod fixtures;
pub mod speech;
pub mod sources;
pub mod fake_sse;
