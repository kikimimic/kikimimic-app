//! Kikimimic core: engine-agnostic domain types and traits.
//!
//! UI shells and inference backends depend on this crate; it depends on
//! nothing platform-specific so it stays testable headless (ADR-120001).

pub mod asr;
pub mod capture;
pub mod catalog;
mod conductor;
pub mod domain;
pub mod engine;
pub mod event;
pub mod glossary;
pub mod language;
pub mod library;
mod local_agreement;
mod mix;
pub mod postprocess;
mod resample;
mod runtime;
mod scheduler;
pub mod settings;
pub mod sim;
pub mod store;

pub use conductor::RecordingTarget;
pub use runtime::{CaptureBackend, CaptureHandle, Config, Events, Kikimimic, SessionMode};
