//! Metrics contains the metrics setup for the controller.

use opentelemetry::{
    global,
    metrics::{Counter, Meter},
};

/// Common metrics for the reconcilers.
#[derive(Clone)]
pub struct Metrics {
    /// Metrics collected from reconcilers.
    pub reconcile: ReconcileMetrics,
}

impl Default for Metrics {
    fn default() -> Self {
        let meter = global::meter("clair_ctrl_reconcile");
        let reconcile = ReconcileMetrics::from(meter);
        Self { reconcile }
    }
}

/// Metrics collected from reconcilers.
#[derive(Clone)]
pub struct ReconcileMetrics {
    /// Runs records the total number of calls to a reconciler.
    pub runs: Counter<u64>,
    /// Failures records the total number of reconciler calls that resulted in a failure.
    pub failures: Counter<u64>,
}

impl From<Meter> for ReconcileMetrics {
    fn from(meter: Meter) -> Self {
        Self {
            runs: meter
                .u64_counter("reconciles")
                .with_unit("{request}")
                .build(),
            failures: meter.u64_counter("failures").with_unit("{request}").build(),
        }
    }
}
