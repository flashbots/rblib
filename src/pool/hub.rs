use {super::*, tokio::sync::broadcast};

/// The hub is responsible for receiving incoming orders and matching them to
/// their order groups. After matching the order to its group, the hub will
/// broadcast the order to all subscribers.
pub struct Hub<P: Platform> {
	fanout: broadcast::Sender<PooledOrder<P>>,
}

impl<P: Platform> Hub<P> {
	pub fn new() -> Self {
		let (fanout, _) = broadcast::channel(1024);

		Self { fanout }
	}
}

impl<P: Platform> Hub<P> {
	pub fn accept(&self, order: Order<P>) {
		todo!()
	}
}
