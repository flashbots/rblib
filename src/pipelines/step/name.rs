use {crate::prelude::*, core::any::type_name, std::sync::OnceLock};

#[derive(Debug, Clone)]
pub(super) struct Name {
	pretty: String,
	metrics: OnceLock<String>,
}

impl Name {
	pub(super) fn new<S: Step<P>, P: Platform>() -> Self {
		Self {
			pretty: short_type_name(type_name::<S>()),
			metrics: OnceLock::new(),
		}
	}

	/// Returns a short type name of the step.
	///
	/// Strips away all type paths and leaves only the last component.
	pub(super) const fn pretty(&self) -> &str {
		self.pretty.as_str()
	}

	/// Returns the name of the metric scope for this step.
	///
	/// # Panics
	/// This value can only be used only after initializing the metric name
	/// through `init_metric`.
	pub(super) fn metric(&self) -> &str {
		self
			.metrics
			.get()
			.expect("metrics name not initialized")
			.as_str()
	}

	/// Initializes the metric name for this step.
	///
	/// Metric names are initialized by
	/// [`service::PipelineServiceBuilder`] through a call to `.into_service()`
	/// during reth node setup.
	///
	/// # Panics
	/// This function will panic if the metric name has already been initialized.
	pub(super) fn init_metrics(&self, name: impl Into<String>) {
		self
			.metrics
			.set(name.into())
			.expect("Metrics name already initialized");
	}

	/// Returns true if the metric name has been initialized.
	pub(super) fn has_metrics(&self) -> bool {
		self.metrics.get().is_some()
	}
}

impl PartialEq for Name {
	fn eq(&self, other: &Self) -> bool {
		self.pretty == other.pretty
	}
}

impl PartialEq<str> for Name {
	fn eq(&self, other: &str) -> bool {
		self.pretty == other
	}
}

impl PartialEq<String> for Name {
	fn eq(&self, other: &String) -> bool {
		self.pretty == other.as_str()
	}
}

fn short_type_name(full_name: &str) -> String {
	let mut short_name = String::new();

	{
		// A typename may be a composition of several other type names (e.g. generic
		// parameters) separated by the characters that we try to find below.
		// Then, each individual typename is shortened to its last path component.
		//
		// Note: Instead of `find`, `split_inclusive` would be nice, but it's still
		// unstable...
		let mut remainder = full_name;
		while let Some(index) =
			remainder.find(&['<', '>', '(', ')', '[', ']', ',', ';'][..])
		{
			let (path, new_remainder) = remainder.split_at(index);
			// Push the shortened path in front of the found character
			short_name.push_str(path.rsplit(':').next().unwrap());
			// Push the character that was found
			let character = new_remainder.chars().next().unwrap();
			short_name.push(character);
			// Advance the remainder
			if character == ',' || character == ';' {
				// A comma or semicolon is always followed by a space
				short_name.push(' ');
				remainder = &new_remainder[2..];
			} else {
				remainder = &new_remainder[1..];
			}
		}

		// The remainder will only be non-empty if there were no matches at all
		if !remainder.is_empty() {
			// Then, the full typename is a path that has to be shortened
			short_name.push_str(remainder.rsplit(':').next().unwrap());
		}
	}

	short_name
}
