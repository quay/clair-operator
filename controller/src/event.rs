//! Event holds helpers for working with kubernetes Events.
pub use kube::runtime::events::{Event, EventType};

/// Reason is a marker trait for types meant to be used as event reasons.
///
/// It's useful to keep track of them because they're _kind of_ API.
pub trait Reason: ToString {}

/// Action is a marker trait for types meant to be used as event actions.
///
/// It's useful to keep track of them because they're _kind of_ API.
pub trait Action: ToString {}

/// Common creation function.
pub fn new<R, A>(type_: EventType, reason: R, action: A) -> Event
where
    R: Reason,
    A: ToString,
{
    let reason = reason.to_string();
    let action = action.to_string();
    Event {
        type_,
        reason,
        action,
        note: None,
        secondary: None,
    }
}
