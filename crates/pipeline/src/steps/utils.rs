use crate::prelude::*;

pub struct BreakAfterDeadline;
impl<P: Platform> Step<P> for BreakAfterDeadline {
	async fn step(
		self: std::sync::Arc<Self>,
		payload: Checkpoint<P>,
		ctx: StepContext<P>,
	) -> ControlFlow<P> {
		if ctx.deadline_reached() {
			ControlFlow::Break(payload)
		} else {
			ControlFlow::Ok(payload)
		}
	}
}
