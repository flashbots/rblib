use {
	proc_macro::TokenStream,
	proc_macro2::Span,
	quote::quote,
	syn::{
		Block,
		Ident,
		ItemFn,
		Token,
		Type,
		TypeParamBound,
		parse::{Parse, ParseStream},
		parse_macro_input,
		punctuated::Punctuated,
	},
};

struct PlatformList {
	platforms: Punctuated<Ident, Token![,]>,
}

impl Parse for PlatformList {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let platforms = input.parse_terminated(Ident::parse, Token![,])?;
		Ok(PlatformList { platforms })
	}
}

#[proc_macro_attribute]
pub fn rblib_test(args: TokenStream, input: TokenStream) -> TokenStream {
	let platform_list = parse_macro_input!(args as PlatformList);
	let original_fn = parse_macro_input!(input as ItemFn);
	let sig = &original_fn.sig;

	let name = &sig.ident;
	let attrs = &original_fn.attrs;
	let generics = &sig.generics;
	let where_clause = &generics.where_clause;
	let output = &sig.output;
	let block = &original_fn.block;

	// Determine if the original function returns `eyre::Result<()>`
	let returns_eyre_result_unit = match output {
		syn::ReturnType::Default => false,
		syn::ReturnType::Type(_, ty) => is_eyre_result_unit(ty),
	};

	// Preserve return type only when it is `eyre::Result<()>`
	let generic_fn_output = if returns_eyre_result_unit {
		let ty = match output {
			syn::ReturnType::Type(_, ty) => ty,
			_ => unreachable!(),
		};
		quote! { -> #ty }
	} else {
		quote! {}
	};

	// Generate test functions for each platform
	let test_functions = platform_list.platforms.iter().map(|platform| {
		let platform_lowercase = platform.to_string().to_lowercase();
		let test_fn_name =
			Ident::new(&format!("{name}_{platform_lowercase}"), name.span());

		if returns_eyre_result_unit {
			quote! {
					#[tokio::test]
					async fn #test_fn_name() {
							#name::<#platform>().await.unwrap();
					}
			}
		} else {
			quote! {
					#[tokio::test]
					async fn #test_fn_name() {
							#name::<#platform>().await
					}
			}
		}
	});

	// Generate the original function (made private and generic)
	let expanded = quote! {
			#(#attrs)*
			async fn #name #generics () #generic_fn_output
			#where_clause
			#block

			#(#test_functions)*
	};

	TokenStream::from(expanded)
}

#[proc_macro]
pub fn if_platform(input: TokenStream) -> TokenStream {
	let IfPlatformInput { platform, code, .. } =
		parse_macro_input!(input as IfPlatformInput);

	let expanded = quote! {
			{
				if std::any::TypeId::of::<P>() == std::any::TypeId::of::<#platform>() {
					// Shadow P with the concrete platform type
					type P = #platform;
					#code
				}
			}
	};

	TokenStream::from(expanded)
}

/// Ensures that a given trait is [dyn safe].
///
/// Generates a test case that fails at compile time if the given trait is not
/// dyn safe.
///
/// Usage examples:
///   assert_is_dyn_safe!(MyTrait);
///   assert_is_dyn_safe!(MyTrait<P>, P: SomeBound + AnotherBound);
///
/// [dyn safe]: https://doc.rust-lang.org/reference/items/traits.html#object-safety
#[proc_macro]
pub fn assert_is_dyn_safe(input: TokenStream) -> TokenStream {
	struct AssertIsDynSafeInput {
		trait_ty: Type,
		generics: Option<GenericParamList>,
	}

	impl Parse for AssertIsDynSafeInput {
		fn parse(input: ParseStream) -> syn::Result<Self> {
			let trait_ty: Type = input.parse()?;

			if input.is_empty() {
				return Ok(Self {
					trait_ty,
					generics: None,
				});
			}

			// Optional comma, then generic parameter declarations (e.g., P: Platform,
			// Q: Foo + Bar)
			let _comma: Token![,] = input.parse()?;
			if input.is_empty() {
				return Ok(Self {
					trait_ty,
					generics: None,
				});
			}

			let generics = input.parse::<GenericParamList>()?;
			Ok(Self {
				trait_ty,
				generics: Some(generics),
			})
		}
	}

	struct GenericParamList {
		params: Punctuated<TypeGenericParam, Token![,]>,
	}

	impl Parse for GenericParamList {
		fn parse(input: ParseStream) -> syn::Result<Self> {
			let params =
				Punctuated::<TypeGenericParam, Token![,]>::parse_terminated(input)?;
			Ok(Self { params })
		}
	}

	struct TypeGenericParam {
		ident: Ident,
		colon_token: Option<Token![:]>,
		bounds: Punctuated<TypeParamBound, Token![+]>,
	}

	impl Parse for TypeGenericParam {
		fn parse(input: ParseStream) -> syn::Result<Self> {
			let ident: Ident = input.parse()?;

			let mut colon_token = None;
			let mut bounds = Punctuated::new();

			if input.peek(Token![:]) {
				colon_token = Some(input.parse()?);
				// Parse non-empty bounds list separated by '+'
				bounds =
					Punctuated::<TypeParamBound, Token![+]>::parse_separated_nonempty(
						input,
					)?;
			}

			Ok(Self {
				ident,
				colon_token,
				bounds,
			})
		}
	}

	let AssertIsDynSafeInput { trait_ty, generics } =
		parse_macro_input!(input as AssertIsDynSafeInput);

	// Build the generic parameter declaration list for the synthesized struct.
	let generics_decl = if let Some(generics) = generics {
		let params = generics.params.iter().map(|p| {
			let ident = &p.ident;
			if let Some(_colon) = p.colon_token {
				let bounds = &p.bounds;
				quote! { #ident: #bounds }
			} else {
				quote! { #ident }
			}
		});
		quote! { < #( #params ),* > }
	} else {
		quote! {}
	};

	// Create a test name from the last path segment of the trait type.
	let test_name_ident = {
		let fallback = Ident::new("trait_is_dyn_safe", Span::call_site());
		let ident = last_ident_of_type(&trait_ty).unwrap_or(fallback.clone());
		let snake = to_snake_case(&ident.to_string());
		Ident::new(&format!("{}_is_dyn_safe", snake), ident.span())
	};

	// Create a private scope with a type that includes Box<dyn Trait<...>>.
	// If the trait is not object-safe, this will fail to compile.
	let expanded = quote! {
			#[test]
			fn #test_name_ident() {
				struct _AssertIsDynSafe #generics_decl ( ::std::boxed::Box<dyn #trait_ty> );
			}
	};

	TokenStream::from(expanded)
}

struct IfPlatformInput {
	platform: Type,
	_arrow: Token![=>],
	code: Block,
}

impl Parse for IfPlatformInput {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let platform = input.parse()?;
		let _arrow = input.parse()?;
		let code = input.parse()?;
		Ok(IfPlatformInput {
			platform,
			_arrow,
			code,
		})
	}
}

// Detect if a return type is exactly `eyre::Result<()>`
fn is_eyre_result_unit(ty: &Type) -> bool {
	use syn::{GenericArgument, PathArguments, Type as SynType};

	if let SynType::Path(tp) = ty {
		let segments = &tp.path.segments;
		if let Some(last) = segments.last()
			&& last.ident == "Result"
		{
			// Ensure the path contains `eyre` somewhere, e.g. `eyre::Result`
			let has_eyre = segments.iter().any(|s| s.ident == "eyre");
			if !has_eyre {
				return false;
			}
			if let PathArguments::AngleBracketed(ab) = &last.arguments
				&& let Some(GenericArgument::Type(SynType::Tuple(tup))) =
					ab.args.first()
			{
				return tup.elems.is_empty();
			}
		}
	}
	false
}

// Helper: last identifier of a type path (e.g., crate::mod::Trait<T> -> Trait)
fn last_ident_of_type(ty: &Type) -> Option<Ident> {
	if let Type::Path(tp) = ty {
		tp.path.segments.last().map(|s| s.ident.clone())
	} else {
		None
	}
}

// Helper: basic UpperCamel to snake_case conversion
fn to_snake_case(s: &str) -> String {
	let mut out = String::with_capacity(s.len() * 2);
	for (i, ch) in s.chars().enumerate() {
		if ch.is_ascii_uppercase() {
			if i != 0 {
				out.push('_');
			}
			out.push(ch.to_ascii_lowercase());
		} else {
			out.push(ch);
		}
	}
	out
}
