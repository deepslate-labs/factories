use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::Brace;

struct SpecializeInput {
    specialize_on: syn::Type,
    _return_arrow: syn::Token![->],
    specialize_output: syn::Type,
    _brace_token: Brace,
    arms: Vec<SpecializeMatchArm>,
}

impl Parse for SpecializeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let specialize_on = input.parse()?;
        let return_arrow = input.parse()?;
        let specialize_output = input.parse()?;

        let content;
        let brace_token = syn::braced!(content in input);

        let arms = SpecializeMatchArm::parse_multiple(&content)?;

        Ok(Self {
            specialize_on,
            _return_arrow: return_arrow,
            specialize_output,
            _brace_token: brace_token,
            arms,
        })
    }
}

struct SpecializeMatchArm {
    pub pat: SpecializePat,
    pub _fat_arrow_token: syn::Token![=>],
    pub body: Box<syn::Expr>,
    pub _comma: Option<syn::Token![,]>,
}

impl SpecializeMatchArm {
    fn parse_multiple(input: ParseStream) -> syn::Result<Vec<Self>> {
        let mut arms = Vec::new();

        while !input.is_empty() {
            arms.push(input.parse()?);
        }

        Ok(arms)
    }

    fn expr_requires_comma(expr: &syn::Expr) -> bool {
        match expr {
            syn::Expr::If(_)
            | syn::Expr::Match(_)
            | syn::Expr::Block(_)
            | syn::Expr::Unsafe(_)
            | syn::Expr::While(_)
            | syn::Expr::Loop(_)
            | syn::Expr::ForLoop(_)
            | syn::Expr::TryBlock(_)
            | syn::Expr::Const(_) => false,
            _ => true,
        }
    }
}

impl Parse for SpecializeMatchArm {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pat = SpecializePat::parse(input)?;
        let fat_arrow_token = input.parse()?;
        let body = syn::Expr::parse_with_earlier_boundary_rule(input)?;

        let comma = if Self::expr_requires_comma(&body) {
            Some(input.parse()?)
        } else {
            input.parse()?
        };

        Ok(Self {
            pat,
            _fat_arrow_token: fat_arrow_token,
            body: Box::new(body),
            _comma: comma,
        })
    }
}

struct SpecializePat {
    _leading_vert: Option<syn::Token![|]>,
    selectors: Punctuated<SpecializePatSelector, syn::Token![|]>,
}

impl Parse for SpecializePat {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let leading_vert: Option<syn::Token![|]> = input.parse()?;

        let mut selectors = Punctuated::new();

        loop {
            selectors.push_value(SpecializePatSelector::parse(input)?);

            if !input.peek(syn::Token![|]) {
                break;
            }

            selectors.push_punct(input.parse()?);
        }

        Ok(Self {
            _leading_vert: leading_vert,
            selectors,
        })
    }
}

enum SpecializePatSelector {
    /// A wildcard pattern `_` that matches any type.
    Wildcard,
    /// A bounded pattern with optional type variable binding, e.g. `T@Clone + Debug` or just `Clone`.
    Bounded {
        binding: Option<(syn::Ident, syn::Token![@])>,
        bounds: Punctuated<syn::TypeParamBound, syn::Token![+]>,
    },
}

impl SpecializePatSelector {
    /// Returns the binding ident if this selector has one.
    fn binding(&self) -> Option<&syn::Ident> {
        match self {
            Self::Wildcard => None,
            Self::Bounded { binding, .. } => binding.as_ref().map(|(ident, _)| ident),
        }
    }
}

impl Parse for SpecializePatSelector {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Check for `_` wildcard - must not be followed by `@` (that would be a binding)
        if input.peek(syn::Token![_]) && !input.peek2(syn::Token![@]) {
            let _: syn::Token![_] = input.parse()?;
            return Ok(Self::Wildcard);
        }

        let binding = match input.peek2(syn::Token![@]) {
            true => Some((input.parse()?, input.parse()?)),
            false => None,
        };

        let mut bounds = Punctuated::new();

        loop {
            if input.is_empty()
                || input.peek(syn::token::Brace)
                || input.peek(syn::Token![,])
                || input.peek(syn::Token![;])
                || input.peek(syn::Token![:]) && !input.peek(syn::Token![::])
                || input.peek(syn::Token![=])
            {
                break;
            }

            bounds.push_value(syn::TypeParamBound::parse(input)?);
            if !input.peek(syn::Token![+]) {
                break;
            }

            let plus = input.parse()?;
            bounds.push_punct(plus);
        }

        Ok(Self::Bounded { binding, bounds })
    }
}

/// A resolved selector ready for code generation, with all metadata pre-computed.
struct ResolvedSelector<'a> {
    /// The trait name for this selector (e.g. `SpecializePattern0Selector1`).
    trait_name: syn::Ident,
    /// The selector pattern (bounds or wildcard).
    selector: &'a SpecializePatSelector,
    /// The user's type variable binding for the arm, if any.
    type_var_binding: Option<syn::Ident>,
    /// The arm's body expression.
    body: &'a syn::Expr,
    /// Number of `&` references on the impl target - determines autoref priority.
    /// Higher ref count = higher priority (tried first during method resolution).
    ref_count: usize,
}

/// Flatten all arms into a list of resolved selectors with pre-computed ref counts.
fn resolve_selectors(input: &SpecializeInput) -> Vec<ResolvedSelector<'_>> {
    let total: usize = input
        .arms
        .iter()
        .map(|arm| arm.pat.selectors.iter().len())
        .sum();

    let mut selectors = Vec::with_capacity(total);

    for (arm_index, arm) in input.arms.iter().enumerate() {
        // Validate that all selectors in this arm bind the same type variable.
        let type_var_binding = resolve_type_var_binding(arm);

        for (selector_index, selector) in arm.pat.selectors.iter().enumerate() {
            let flat_index = selectors.len();

            selectors.push(ResolvedSelector {
                trait_name: quote::format_ident!(
                    "SpecializePattern{}Selector{}",
                    arm_index,
                    selector_index
                ),
                selector,
                type_var_binding: type_var_binding.clone(),
                body: &arm.body,
                // First selector gets ref_count = total - 1 (highest priority),
                // last gets 0 (lowest priority / fallback).
                ref_count: total - 1 - flat_index,
            });
        }
    }

    selectors
}

/// Find and validate the type variable binding across all selectors in an arm.
fn resolve_type_var_binding(arm: &SpecializeMatchArm) -> Option<syn::Ident> {
    let mut found: Option<&syn::Ident> = None;

    for selector in &arm.pat.selectors {
        if let Some(ident) = selector.binding() {
            match found {
                None => found = Some(ident),
                Some(prev) if prev == ident => {}
                Some(prev) => {
                    proc_macro_error::emit_error!(
                        ident.span(),
                        "All patterns in the same arm must bind the same type variable (previously bound as `{}`)",
                        prev
                    );
                }
            }
        }
    }

    found.cloned()
}

pub(super) fn proc_macro_specialize(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as SpecializeInput);
    let selectors = resolve_selectors(&input);
    let return_type = &input.specialize_output;
    let test_type = &input.specialize_on;

    let resolvers = selectors.iter().map(|s| generate_resolver(return_type, s));
    let test_refs = std::iter::repeat_n(quote::quote! { & }, selectors.len());

    quote::quote! {
        {
            fn resolve() -> #return_type {
                struct SpecializeChecker<T: ?Sized>(::core::marker::PhantomData<T>);

                #(#resolvers)*

                (#(#test_refs)* SpecializeChecker::<#test_type>(::core::marker::PhantomData)).specialize()
            }

            resolve
        }
    }
    .into()
}

/// Generate a single trait + impl pair for one resolved selector.
fn generate_resolver(
    return_type: &syn::Type,
    resolved: &ResolvedSelector<'_>,
) -> proc_macro2::TokenStream {
    // The internal type parameter used in the trait impl - not exposed to user code.
    let internal_type_param =
        syn::Ident::new("__SpecializeT", proc_macro2::Span::mixed_site());

    let trait_name = &resolved.trait_name;
    let body = resolved.body;
    let refs = std::iter::repeat_n(quote::quote! { & }, resolved.ref_count);

    // Generate the where clause (empty for wildcards).
    let where_clause = match resolved.selector {
        SpecializePatSelector::Wildcard => None,
        SpecializePatSelector::Bounded { bounds, .. } => {
            let mut predicates = Punctuated::new();
            predicates.push_value(syn::WherePredicate::Type(syn::PredicateType {
                lifetimes: None,
                bounded_ty: syn::Type::Path(syn::TypePath {
                    qself: None,
                    path: internal_type_param.clone().into(),
                }),
                colon_token: Default::default(),
                bounds: bounds.clone(),
            }));

            Some(syn::WhereClause {
                where_token: Default::default(),
                predicates,
            })
        }
    };

    // Generate the inner function that wraps the body.
    // - With binding: fn do_specialize<T: Bounds>() -> RT { body }; do_specialize::<__SpecializeT>()
    // - Without binding: fn do_specialize() -> RT { body }; do_specialize()
    let (inner_fn, inner_call) = match (&resolved.type_var_binding, resolved.selector) {
        (Some(user_binding), SpecializePatSelector::Bounded { bounds, .. }) => {
            let inner_fn = quote::quote! {
                fn do_specialize<#user_binding: #bounds>() -> #return_type {
                    #body
                }
            };
            let inner_call = quote::quote! {
                do_specialize::<#internal_type_param>()
            };
            (inner_fn, inner_call)
        }
        _ => {
            // No binding or wildcard - body is not generic
            let inner_fn = quote::quote! {
                fn do_specialize() -> #return_type {
                    #body
                }
            };
            let inner_call = quote::quote! {
                do_specialize()
            };
            (inner_fn, inner_call)
        }
    };

    quote::quote! {
        trait #trait_name { fn specialize(&self) -> #return_type; }

        impl<#internal_type_param: ?Sized> #trait_name for #(#refs)* SpecializeChecker<#internal_type_param> #where_clause {
            fn specialize(&self) -> #return_type {
                #inner_fn
                #inner_call
            }
        }
    }
}
