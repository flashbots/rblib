use super::*;

#[derive(Default, Debug)]
pub struct BaseFeeFilter;

impl<P: Platform> OrderFilter<P> for BaseFeeFilter {}
