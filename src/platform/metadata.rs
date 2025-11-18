use {crate::prelude::*, core::fmt::Debug};

/// Constraints on user-defined checkpoint metadata.
pub trait CheckpointContext<P: Platform>:
	Debug + Default + Clone + PartialEq + Eq + Send + Sync
{
}

impl<P: Platform> CheckpointContext<P> for () {}
