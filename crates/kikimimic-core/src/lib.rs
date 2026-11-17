//! Kikimimic core: engine-agnostic domain types and traits.
//!
//! UI shells and inference backends depend on this crate; it depends on
//! nothing platform-specific so it stays testable headless (ADR-120001).

pub mod annotations;
pub mod advisor;
pub mod asr;
pub mod capture;
pub mod catalog;
mod conductor;
pub mod diarization_evaluation;
pub mod domain;
pub mod engine;
pub mod event;
pub mod export;
pub mod glossary;
pub mod imports;

pub mod language;
pub mod library;
pub mod meeting_qa;

pub mod minutes;

pub mod meeting;

pub mod metrics;

mod local_agreement;
mod mix;
pub mod postprocess;
mod resample;
mod runtime;
mod scheduler;
pub mod search;
pub mod settings;
pub mod sim;
pub mod store;
pub mod word_timing;

pub use conductor::RecordingTarget;
pub use runtime::{CaptureBackend, CaptureHandle, Config, Events, Kikimimic, SessionMode};
