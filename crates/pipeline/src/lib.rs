// Pipelines, steps, pools, and other orchestration utilities for rblib.
pub use rblib_core::{
	self as core,
	Variant,
	alloy,
	payload,
	platform,
	reth,
	revm,
	uuid,
};

#[doc(hidden)]
pub mod metrics_util {
	pub use rblib_core::metrics_util::*;
}

pub mod orderpool2;
pub mod pipelines;
pub mod pool;
pub mod steps;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

pub use {orderpool2::*, pipelines::*, pool::*, steps::*};

pub mod prelude {
	pub(crate) use rblib_core::Variant;
	pub use {
		super::{orderpool2::*, pipelines::*, pool::*, steps::*},
		metrics::{Counter, Gauge, Histogram},
		pipelines_macros::MetricsSet,
		rblib_core::prelude::*,
	};
}
