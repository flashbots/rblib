// core modules, partially re-exported through `rblib::prelude`
mod payload;
mod pipelines;
mod platform;

// RBLib Core Public API Prelude
pub mod prelude {
	pub use {
		super::{payload::*, pipelines::*, platform::*},
		metrics::{Counter, Gauge, Histogram},
		pipelines_macros::MetricsSet,
	};

	pub(crate) use super::Variant;
}

#[doc(hidden)]
pub mod metrics_util {
	pub use metrics::*;
}

/// Order Pool Public API
pub mod pool;

/// Common steps library
pub mod steps;

/// Orderpool utils
pub mod orderpool2;

/// Externally available test utilities
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

// Reexport reth version that is used by rblib as a convenience for downstream
// users. Those exports should be enough to get started with a simple node.
pub mod reth {
	pub use reth_origin::*;

	pub mod chainspec {
		pub use reth_chainspec::*;
	}

	pub mod cli {
		pub use {reth_cli::*, reth_origin::cli::*};

		pub mod commands {
			pub use reth_cli_commands::*;
		}
	}

	pub mod evm {
		pub use reth_evm::*;
	}

	pub mod errors {
		pub use reth_errors::*;
	}

	pub mod metrics {
		pub use reth_metrics::*;
	}

	pub mod payload {
		pub use ::reth_origin::payload::*;
		pub mod builder {
			pub use {
				reth_basic_payload_builder::*,
				reth_ethereum_payload_builder::*,
				reth_payload_builder::*,
			};
		}

		pub mod primitives {
			pub use reth_payload_primitives::*;
		}

		#[cfg(feature = "optimism")]
		pub mod util {
			pub use reth_payload_util::*;
		}
	}

	pub mod node {
		pub mod api {
			pub use reth_node_api::*;
		}

		pub mod builder {
			pub use reth_node_builder::*;
		}

		pub mod transaction_pool {
			pub use reth_transaction_pool::*;
		}
	}

	pub mod provider {
		pub use reth_provider::*;
	}

	pub mod rpc_eth_types {
		pub use reth_rpc_eth_types::*;
	}

	pub mod tracing_otlp {
		pub use reth_tracing_otlp::*;
	}

	pub mod db {
		pub use reth_db::*;
	}

	pub mod eth_wire_types {
		pub use reth_eth_wire_types::*;
	}

	pub mod network {
		pub use reth_network::*;
	}

	pub mod node_types {
		pub use reth_node_types::*;
	}

	pub mod ethereum {
		pub use reth_ethereum::*;
	}

	pub mod network_peers {
		pub use reth_network_peers::*;
	}

	#[cfg(feature = "optimism")]
	pub mod optimism {
		pub mod chainspec {
			pub use reth_optimism_chainspec::*;
		}
		pub mod node {
			pub use reth_optimism_node::*;
		}
		pub mod rpc {
			pub use reth_optimism_rpc::*;
		}
		pub mod forks {
			pub use reth_optimism_forks::*;
		}
		pub mod cli {
			pub use reth_optimism_cli::*;
		}
		pub mod primitives {
			pub use reth_optimism_primitives::*;
		}
		pub mod txpool {
			pub use reth_optimism_txpool::*;
		}
		pub mod consensus {
			pub use reth_optimism_consensus::*;
		}
		pub mod evm {
			pub use reth_optimism_evm::*;
		}
	}
}

pub mod revm {
	pub mod database {
		pub use revm_database::*;
	}
}

pub mod alloy {
	pub use alloy_origin::*;

	pub mod evm {
		pub use alloy_evm::*;
	}

	pub mod serde {
		pub use alloy_serde::*;
	}

	#[cfg(any(test, feature = "test-utils"))]
	pub mod genesis {
		pub use alloy_genesis::*;
	}

	#[cfg(feature = "optimism")]
	pub mod optimism {
		pub use op_alloy::*;

		pub mod rpc_types_engine {
			pub use op_alloy_rpc_types_engine::*;
		}
	}
}

pub use uuid;

/// Used internally as a sentinel type for generic parameters.
#[doc(hidden)]
pub enum Variant<const U: usize = 0> {}
