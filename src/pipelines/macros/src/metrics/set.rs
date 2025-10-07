use {
	proc_macro::TokenStream,
	quote::quote,
	syn::{
		Data,
		DeriveInput,
		Expr,
		ExprLit,
		Field,
		Fields,
		Lit,
		Meta,
		MetaNameValue,
		PathArguments,
		Token,
		Type,
		parse::ParseStream,
		parse_macro_input,
		token::Comma,
	},
};

/// Derive for `MetricsSet`.
///
/// Behavior:
/// - `Default` impl sets all supported metric fields (Counter, Gauge,
///   Histogram) to their noop variants.
/// - Generates `fn with_scope(scope: &str) -> Self` which:
///     * Builds metric names as `<scope>_<metric_name>` (or renamed value).
///     * Uses doc comments (joined with spaces) as default description unless
///       `#[metric(describe = "...")]` provided.
///     * Supports per-field `#[metric(rename = "...", describe = "...",
///       skip)]`.
///     * Describes metrics (if description non-empty) via `describe_*` macros
///       before registering.
/// - Adds a simple Debug impl (non-exhaustive) similar to metrics-derive style.
#[allow(clippy::too_many_lines)]
pub(crate) fn metrics_set_derive(input: TokenStream) -> TokenStream {
	let input = parse_macro_input!(input as DeriveInput);
	let ident = &input.ident;

	let Data::Struct(data_struct) = &input.data else {
		return syn::Error::new_spanned(
			&input,
			"MetricsSet can only be derived for structs.",
		)
		.to_compile_error()
		.into();
	};

	let Fields::Named(named) = &data_struct.fields else {
		return syn::Error::new_spanned(
			&input,
			"MetricsSet only supports structs with named fields.",
		)
		.to_compile_error()
		.into();
	};

	let mut metas = Vec::<FieldMeta>::new();

	for field in &named.named {
		let mut rename: Option<String> = None;
		let mut describe: Option<String> = None;

		for attr in &field.attrs {
			if attr.path().is_ident("metric")
				&& let Err(e) = parse_metric_attr(attr, &mut rename, &mut describe)
			{
				return e.to_compile_error().into();
			}
		}

		let doc_lines: Vec<String> =
			field.attrs.iter().filter_map(extract_doc).collect();
		let doc_joined = doc_lines.join(" ");

		let kind = classify(&field.ty);
		if matches!(kind, MetricKind::Other) {
			return syn::Error::new_spanned(
				&field.ty,
				"unsupported metric field type (expected Counter, Gauge, or Histogram)",
			)
			.to_compile_error()
			.into();
		}

		let final_name =
			rename.unwrap_or_else(|| field.ident.as_ref().unwrap().to_string());
		let description = describe.unwrap_or(doc_joined);
		metas.push(FieldMeta {
			field: field.clone(),
			kind,
			name: final_name,
			description,
		});
	}

	let mut default_inits = Vec::new();
	let mut init_inits = Vec::new();

	let rblib = super::rblib_path();

	for meta in &metas {
		let ident_field = meta.field.ident.as_ref().unwrap();
		let short_name = &meta.name;
		let desc = &meta.description;

		let (noop_expr, describe_stmt, register_stmt) = match meta.kind {
			MetricKind::Counter => (
				quote! { #rblib::metrics_util::Counter::noop() },
				if desc.is_empty() {
					quote! {}
				} else {
					quote! { #rblib::metrics_util::describe_counter!(full_name.clone(), #desc); }
				},
				quote! { #rblib::metrics_util::counter!(full_name) },
			),
			MetricKind::Gauge => (
				quote! { #rblib::metrics_util::Gauge::noop() },
				if desc.is_empty() {
					quote! {}
				} else {
					quote! { #rblib::metrics_util::describe_gauge!(full_name.clone(), #desc); }
				},
				quote! { #rblib::metrics_util::gauge!(full_name) },
			),
			MetricKind::Histogram => (
				quote! { #rblib::metrics_util::Histogram::noop() },
				if desc.is_empty() {
					quote! {}
				} else {
					quote! { #rblib::metrics_util::describe_histogram!(full_name.clone(), #desc); }
				},
				quote! { #rblib::metrics_util::histogram!(full_name) },
			),
			MetricKind::Other => unreachable!(),
		};

		default_inits.push(quote! { #ident_field: #noop_expr, });
		init_inits.push(quote! {
				#ident_field: {
						let full_name = format!("{}_{}", scope, #short_name);
						#describe_stmt
						#register_stmt
				},
		});
	}

	let expanded = quote! {
		const _: () = {
			impl ::core::default::Default for #ident {
					fn default() -> Self {
							Self { #(#default_inits)* }
					}
			}

			impl #ident {
					pub fn with_scope(scope: &str) -> Self {
							Self { #(#init_inits)* }
					}
			}

			impl ::core::fmt::Debug for #ident {
					fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
							f.debug_struct(stringify!(#ident)).finish_non_exhaustive()
					}
			}
		};
	};

	TokenStream::from(expanded)
}

struct FieldMeta {
	field: Field,
	kind: MetricKind,
	name: String,
	description: String,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum MetricKind {
	Counter,
	Gauge,
	Histogram,
	Other,
}

fn classify(ty: &Type) -> MetricKind {
	if let Type::Path(tp) = ty
		&& let Some(seg) = tp.path.segments.last()
		&& matches!(seg.arguments, PathArguments::None)
	{
		return match seg.ident.to_string().as_str() {
			"Counter" => MetricKind::Counter,
			"Gauge" => MetricKind::Gauge,
			"Histogram" => MetricKind::Histogram,
			_ => MetricKind::Other,
		};
	}
	MetricKind::Other
}

fn parse_metric_attr(
	attr: &syn::Attribute,
	rename: &mut Option<String>,
	describe: &mut Option<String>,
) -> syn::Result<()> {
	// Accept: #[metric(rename = "x", describe = "y")] with commas optional after
	// each pair.
	attr.parse_args_with(|input: ParseStream| {
		while !input.is_empty() {
			let path: syn::Path = input.parse()?;
			let Some(ident) = path.get_ident() else {
				return Err(input.error("expected identifier in #[metric(..)]"));
			};
			input.parse::<Token![=]>()?;
			let lit: Lit = input.parse()?;
			match ident.to_string().as_str() {
				"rename" => {
					if let Lit::Str(ls) = lit {
						*rename = Some(ls.value());
					} else {
						return Err(syn::Error::new_spanned(
							lit,
							"rename must be a string literal",
						));
					}
				}
				"describe" => {
					if let Lit::Str(ls) = lit {
						*describe = Some(ls.value());
					} else {
						return Err(syn::Error::new_spanned(
							lit,
							"describe must be a string literal",
						));
					}
				}
				_ => {
					return Err(syn::Error::new_spanned(
						path,
						"unsupported key in #[metric(..)]",
					));
				}
			}
			if input.peek(Comma) {
				let _ = input.parse::<Comma>();
			}
		}
		Ok(())
	})
}

fn extract_doc(attr: &syn::Attribute) -> Option<String> {
	if !attr.path().is_ident("doc") {
		return None;
	}
	if let Meta::NameValue(MetaNameValue {
		value: Expr::Lit(ExprLit {
			lit: Lit::Str(ls), ..
		}),
		..
	}) = &attr.meta
	{
		let line = ls.value().trim().to_string();
		return if line.is_empty() { None } else { Some(line) };
	}
	None
}
