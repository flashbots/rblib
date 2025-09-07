use super::*;

#[derive(Default, Debug)]
pub struct SignerBalanceFilter;

impl<P: Platform> OrderFilter<P> for SignerBalanceFilter {}
