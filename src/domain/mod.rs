//! Shared identities, clocks, vocabularies, and strict configuration.

pub mod config;
pub mod identity;
pub(crate) mod strict;
pub mod time;
pub mod vocabulary;

pub use config::{
    Assignment, AutoBindOutcome, Binding, BindingHealth, BindingKey, Brightness, ConfigError,
    ControlBindings, Direction, DpadAngles, LaunchHost, MAX_CONFIG_BYTES, RuntimeId, Settings,
    SlotMap, SlotMutationError, TerminalAnchor, ValidationError, WindowsSessionRoot,
    validate_custom_label,
};
pub use identity::{
    Generations, NativeProcessIdentity, OpaqueId, OpaqueIdError, RuntimeEpoch, SessionId, Slot,
    SlotError, TargetIdentity, UuidTextError,
};
pub use time::{ClockError, ClockSample, ClockSource, MonotonicStamp};
pub use vocabulary::{Action, RefusalReason, Status};
