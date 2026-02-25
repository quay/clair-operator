use std::{any::type_name, fmt::Debug};

use k8s_openapi::{
    NamespaceResourceScope,
    apimachinery::pkg::apis::meta::v1::{Condition, Time},
    jiff::Timestamp,
};
use kube::{
    Api, Resource, ResourceExt,
    api::Patch,
    runtime::events::{Event, EventType},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use tracing::{Instrument, debug, debug_span, instrument, trace};

use crate::{
    CREATE_PARAMS, Context, PATCH_PARAMS, Result,
    condition::{ConditionTypeFor, Conditions},
};
use clair_templates::{Build, Error as TemplateError};

/// Check_owned_resource builds an owned resource (of type `R`) for T using the templater `B`.
#[instrument(skip(obj, ctx), err, fields(
    source.kind = T::kind(&()).as_ref(),
    source.uid = obj.meta().uid,
    resource = R::kind(&()).as_ref(),
    builder = type_name::<B>(),
))]
pub async fn check_owned_resource<T, R, B>(obj: &T, ctx: &Context) -> Result<Option<R>>
where
    T: Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Serialize
        + DeserializeOwned
        + Conditions
        + Clone
        + Debug,
    R: Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Serialize
        + DeserializeOwned
        + ConditionTypeFor
        + Clone
        + Debug,
    B: Build<Output = R> + for<'i> TryFrom<&'i T, Error = TemplateError>,
{
    let ty = R::CONDITION_TYPE;
    if obj
        .find_condition(ty)
        .is_some_and(|c| c.status == "True" && c.observed_generation == obj.meta().generation)
    {
        trace!("short-circuit");
        return Ok(None);
    }

    let kind = R::kind(&()).to_string();
    let ns = obj.namespace().expect("resource is namespaced");
    let api = Api::<R>::namespaced(ctx.client.clone(), &ns);

    let s: R = B::try_from(obj)?.build();
    let cur: Option<R> = api
        .get_opt(&s.name_any())
        .instrument(debug_span!("get_opt", kind))
        .await?;

    let mut cnd = obj
        .find_condition(ty)
        .cloned()
        .unwrap_or_else(|| Condition {
            type_: R::CONDITION_TYPE.to_string(),
            observed_generation: obj.meta().generation,
            last_transition_time: now(),
            status: "False".into(),
            message: "".into(),
            reason: "".into(),
        });
    let mut ev = normal_event(
        format!("{} requires {kind} \"{}\"", T::kind(&()), s.name_any()),
        "",
    );

    let (obj, changes) = match cur {
        Some(ref cur) => {
            trace!(kind, "patch");
            let next: R = api
                .patch(&cur.name_any(), &PATCH_PARAMS, &Patch::Apply(&s))
                .instrument(debug_span!("patch", kind))
                .await?;

            cnd.message = format!("patched {}", kind);
            cnd.reason = format!("{}Updated", kind);
            ev.action = format!("Updated{}", kind);
            ev.secondary = next.object_ref(&()).into();

            let changes = cur.meta().generation != next.meta().generation;
            (next, changes)
        }
        None => {
            trace!(kind, "create");
            let s: R = api
                .create(&CREATE_PARAMS, &s)
                .instrument(debug_span!("create", kind))
                .await?;

            cnd.message = format!("created {}", kind);
            cnd.reason = format!("{}Created", kind);
            ev.action = format!("Created{}", kind);
            ev.secondary = s.object_ref(&()).into();
            (s, true)
        }
    };

    if changes {
        debug!("updating status");
        let status = Patch::Apply(json!({
            "apiVersion": T::api_version(&()),
            "kind": T::kind(&()),
            "status": {
                "condition": [ cnd ],
            },
        }));
        let objapi = Api::<T>::namespaced(ctx.client.clone(), &ns);
        let name = obj.meta().name.as_ref().expect("resource has name");
        objapi
            .patch_status(name, &PATCH_PARAMS, &status)
            .instrument(debug_span!("patch_status", kind = T::kind(&()).to_string()))
            .await?;
        ctx.recorder.publish(&ev, &obj.object_ref(&())).await?;
    }

    Ok(Some(obj))
}

#[inline]
fn now() -> Time {
    Time(Timestamp::now())
}

fn normal_event<R: ToString, A: ToString>(reason: R, action: A) -> Event {
    let reason = reason.to_string();
    let action = action.to_string();
    Event {
        type_: EventType::Normal,
        note: None,
        secondary: None,
        reason,
        action,
    }
}
