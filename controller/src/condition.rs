//! Module condition enumerates and contains helpers for kubernetes `Condition`s used by the
//! controller.
use std::str::FromStr;

use const_format::concatcp;
use k8s_openapi::{
    apimachinery::pkg::apis::meta::v1::{Condition, Time},
    jiff::Timestamp,
};
use kube::Resource;
use strum::{EnumString, IntoStaticStr};

use api::{
    GROUP,
    v1alpha1::{Clair, Indexer, Matcher},
};
use controller_macros::condition_types;

const PREFIX: &str = concatcp!(GROUP, "/");

/// Status enumerates the statuses for the kubernetes Conditions used by the controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, IntoStaticStr, EnumString)]
#[allow(missing_docs, reason = "pretty self-explanitory")]
pub enum Status {
    #[default]
    Unknown,
    True,
    False,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: &'static str = self.into();
        f.write_str(s)
    }
}

impl<T: AsRef<str>> PartialEq<T> for Status {
    fn eq(&self, other: &T) -> bool {
        let other = Self::from_str(other.as_ref()).unwrap_or_default();
        self == &other
    }
}

/// Type enumerates the type identifier for the kubernetes Conditions used by the controller.
#[derive(Clone, Copy, Debug, PartialEq, IntoStaticStr, EnumString)]
#[allow(missing_docs, reason = "most of these are self-explanitory")]
pub enum Type {
    ConfigReady,
    AdminPreJobDone,
    AdminPostJobDone,
    SpecOk,

    // The object types:
    ConfigMapCreated,
    SecretCreated,
    IndexerCreated,
    MatcherCreated,
    NotifierCreated,
    HorizontalPodAutoscalerCreated,
    ServiceCreated,
    DeploymentCreated,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let t: &'static str = self.into();
        write!(f, "{PREFIX}{t}")
    }
}

impl<T: AsRef<str>> PartialEq<T> for Type {
    fn eq(&self, other: &T) -> bool {
        other
            .as_ref()
            .strip_prefix(PREFIX)
            .and_then(|s| Self::from_str(s).ok())
            .is_some_and(|t| self == &t)
    }
}

/// ConditionTypeFor associates a [`Type`] with the implementor.
///
/// This trait is implemented on objects that are created by the controller and tracked in the
/// reconciled resource.
pub trait ConditionTypeFor {
    /// The associated [`Type`].
    const CONDITION_TYPE: Type;
}

condition_types!(
    ConfigMap,
    Deployment,
    Secret,
    Service,
    Indexer,
    Matcher,
    Notifier,
    HorizontalPodAutoscaler
);

/// Reason is a marker trait for types meant to be used as condition reasons.
///
/// It's useful to keep track of them because they're _kind of_ API.
pub trait Reason: ToString {}

/// ...
pub fn new<O, R, M>(obj: &O, type_: Type, status: Status, reason: R, message: M) -> Condition
where
    O: Resource,
    R: Reason,
    M: ToString,
{
    let mut b = ConditionBuilder::new()
        .type_(type_)
        .status(status)
        .reason(reason)
        .message(message);
    if let Some(n) = obj.meta().generation {
        b = b.generation(n);
    }

    b.build()
}

/// ConditionBuilder is a Builder for controller-produced Conditions.
#[derive(Default)]
pub struct ConditionBuilder {
    last_transition_time: Option<Time>,
    observed_generation: Option<i64>,
    type__: Option<Type>,
    status_: Option<Status>,
    reason_: Option<String>,
    message_: Option<String>,
}

impl ConditionBuilder {
    /// Create a ConditionBuilder.
    pub fn new() -> Self {
        Default::default()
    }

    /// Set the `last_transition_time`.
    ///
    /// Defaults to when [`Self::build()`] is called.
    pub fn time(self, t: Timestamp) -> Self {
        Self {
            last_transition_time: Some(Time(t)),
            ..self
        }
    }

    /// Set the `observed_generation`.
    ///
    /// Has no default.
    pub fn generation(self, n: i64) -> Self {
        Self {
            observed_generation: Some(n),
            ..self
        }
    }

    /// Set the `type`.
    ///
    /// Has no default.
    pub fn type_(self, t: Type) -> Self {
        Self {
            type__: Some(t),
            ..self
        }
    }

    /// Set the `status`.
    ///
    /// Defaults to [`Status::Unknown`].
    pub fn status(self, s: Status) -> Self {
        Self {
            status_: Some(s),
            ..self
        }
    }

    /// Set the `reason`.
    ///
    /// Defaults to the empty string.
    pub fn reason<R: Reason>(self, r: R) -> Self {
        Self {
            reason_: Some(r.to_string()),
            ..self
        }
    }

    /// Set the `message`.
    ///
    /// Defaults to the empty string.
    pub fn message<S: ToString>(self, s: S) -> Self {
        Self {
            message_: Some(s.to_string()),
            ..self
        }
    }

    /// Consume the ConditionBuilder and return the Condition.
    pub fn build(self) -> Condition {
        Condition {
            observed_generation: self.observed_generation,
            last_transition_time: self
                .last_transition_time
                .unwrap_or_else(|| Time(Timestamp::now())),
            type_: self.type__.map(|t| t.to_string()).unwrap_or_default(),
            status: self.status_.unwrap_or_default().to_string(),
            reason: self.reason_.unwrap_or_default(),
            message: self.message_.unwrap_or_default(),
        }
    }
}

/// Conditions is a helper trait for working with kubernetes [`Resource`]s that have a `conditions`
/// member in their status.
pub trait Conditions: Resource {
    /// Return a reference to the Conditions, if both the status and conditions are populated.
    fn get_conditions(&self) -> Option<&Vec<Condition>>;

    /// Return a mutable reference to the Conditions, if the status is populated.
    #[cfg(test)]
    fn get_conditions_mut(&mut self) -> Option<&mut Vec<Condition>>;

    /// Find the first Condition of the given Type.
    ///
    /// For objects from the API server, there should only be one.
    fn find_condition(&self, ty: Type) -> Option<&Condition> {
        self.get_conditions()
            .and_then(|cs| cs.iter().find(|&c| ty == c.type_))
    }
}

macro_rules! impl_conditions {
    ($($ty:ty),+) => {
        $(
        impl Conditions for $ty {
            fn get_conditions(&self) -> Option<&Vec<Condition>> {
                self.status.as_ref().and_then(|s| s.conditions.as_ref())
            }

            #[cfg(test)]
            fn get_conditions_mut(&mut self) -> Option<&mut Vec<Condition>> {
                self.status.as_mut().map(|s| s.conditions.get_or_insert_default())
            }
        }
        )+
    }
}

impl_conditions!(Clair, Indexer, Matcher);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_eq() {
        assert_eq!(Status::Unknown, "Unknown");
        assert_eq!(Status::Unknown, "other");
        assert_eq!(Status::True, "True");
        assert_eq!(Status::False, "False");
    }

    #[test]
    fn type_eq() {
        assert!(Type::SpecOk == "clairproject.org/SpecOk");
    }
}
