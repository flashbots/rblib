use proc_macro::TokenStream;

mod metrics;
mod variants;

/// Generates `IntoPipeline` implementations for tuples of steps up to the
/// specified count.
///
/// # Usage
/// ```rust
/// impl_into_pipeline_steps!(10);
/// ```
///
/// This will generate implementations for tuples of size 1 through 10.
#[proc_macro]
pub fn impl_into_pipeline_steps(input: TokenStream) -> TokenStream {
	variants::impl_into_pipeline_steps(input)
}

/// Derive for `MetricsSet`.
///
/// Behavior:
/// - `Default` impl sets all supported metric fields (Counter, Gauge,
///   Histogram) to their noop variants.
/// - Generates `fn with_scope(scope: &str) -> Self` which:
///     * Builds metric names as `<scope>_<metric_name>` (or renamed value).
///     * Uses doc comments (joined with spaces) as default description unless
///       `#[metric(describe = "...")]` provided.
///     * Supports per-field `#[metric(rename = "...", describe = "...",
///       skip)]`.
///     * Describes metrics (if description non-empty) via `describe_*` macros
///       before registering.
/// - Adds a simple Debug impl (non-exhaustive) similar to metrics-derive style.
///
/// Metrics definition:
///
/// ```rust
/// #[derive(MetricsSet)]
/// struct MyMetrics {
/// 	pub counter_one: Counter,
/// 	pub gauge_two: Gauge,
/// 	pub histogram_three: Histogram,
/// }
/// ```
///
/// Usage:
///
/// This will initialize all metrics recorders to their noop variants and
/// no metrics will be recorded.
///
/// ```rust
/// let metrics = MyMetrics::default();
/// metrics.counter_one.increment(1); // noop
/// metrics.histogram_three.record(2.5); // noop
/// ```
///
/// This will initialize all metrics recorders with the current recorder set by
/// the `metrics` crate. All metrics will be named `{scope_name}_{field_name}`.
///
/// ```rust
/// let metrics = MyMetrics::with_scope("my_scope");
/// metrics.counter_one.increment(1); // will be recorded as "my_scope_counter_one"
/// metrics.histogram_three.record(2.5); // will be recorded as "my_scope_histogram_three"
/// ```
///
/// inspired by: <https://crates.io/crates/metrics-derive>
#[proc_macro_derive(MetricsSet)]
pub fn metrics_set_derive(input: TokenStream) -> TokenStream {
	metrics::metrics_set_derive(input)
}
