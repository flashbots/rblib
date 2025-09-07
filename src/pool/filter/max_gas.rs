use super::*;

#[derive(Debug)]
pub struct MaxGasPerSenderFilter(pub u128);

impl<P: Platform> OrderFilter<P> for MaxGasPerSenderFilter {}
