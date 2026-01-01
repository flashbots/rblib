mod set;

pub(crate) use set::metrics_set_derive;

fn rblib_path() -> proc_macro2::TokenStream {
	use {
		proc_macro_crate::{FoundCrate, crate_name},
		quote::quote,
	};

	match crate_name("rblib-pipeline") {
		Ok(FoundCrate::Itself) => {
			// We are inside the rblib-pipeline crate itself.
			quote!(crate)
		}
		Ok(FoundCrate::Name(name)) => {
			// We are in an external crate; use the actual dependency name (could be
			// renamed).
			let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
			quote!(::#ident)
		}
		Err(_) => {
			// Fallback: assume the crate is available as `::rblib`.
			// Emit a helpful error if that also fails at type-check time.
			quote!(::rblib)
		}
	}
}
