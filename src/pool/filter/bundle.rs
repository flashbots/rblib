use super::*;

#[derive(Default, Debug)]
pub struct BundleFilter;

impl<P: Platform> OrderFilter<P> for BundleFilter {}
