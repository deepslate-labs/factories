use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;

/// Two surface forms share the autoref machinery:
///
/// - **Deferred**: `Type -> ReturnType { pat => expr, ... }` resolves to a
///   `fn() -> ReturnType`. All arms share one return type. This is the original
///   form and is unchanged.
/// - **Inline**: `binding: &mut Type { pat : ArmType => expr, ... }` resolves
///   *in place* to the selected arm's value. Each arm names its own `ArmType`
///   (which may mention the pattern's type binding), and `binding` - an in-scope
///   value of the scrutinee type - is reborrowed and handed to every arm. This
///   is what lets selection return a *specialized* type (e.g. a driver) without
///   funnelling through one erased return type.
enum SpecializeInput {
    Deferred(DeferredInput),
    Inline(InlineInput),
}

struct DeferredInput {
    specialize_on: syn::Type,
    specialize_output: syn::Type,
    arms: Vec<DeferredArm>,
}

struct InlineInput {
    /// In-scope value AND the name each arm sees (reborrowed per-arm).
    binding: syn::Ident,
    /// Scrutinee type as written, e.g. `&mut Self`.
    scrutinee_ty: syn::Type,
    arms: Vec<InlineArm>,
}

impl Parse for SpecializeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // `ident :` (single colon, not `::`) marks the inline value-carrying
        // form. A deferred `Type -> ...` never starts that way (a bare-ident
        // type like `String` is followed by `->`, a path type by `::`).
        if input.peek(syn::Ident)
            && input.peek2(syn::Token![:])
            && !input.peek2(syn::Token![::])
        {
            let binding = input.parse()?;
            let _: syn::Token![:] = input.parse()?;
            let scrutinee_ty = input.parse()?;

            let content;
            syn::braced!(content in input);
            let arms = InlineArm::parse_multiple(&content)?;

            return Ok(Self::Inline(InlineInput {
                binding,
                scrutinee_ty,
                arms,
            }));
        }

        let specialize_on = input.parse()?;
        let _: syn::Token![->] = input.parse()?;
        let specialize_output = input.parse()?;

        let content;
        syn::braced!(content in input);
        let arms = DeferredArm::parse_multiple(&content)?;

        Ok(Self::Deferred(DeferredInput {
            specialize_on,
            specialize_output,
            arms,
        }))
    }
}

struct DeferredArm {
    pub pat: SpecializePat,
    pub body: Box<syn::Expr>,
}

impl DeferredArm {
    fn parse_multiple(input: ParseStream) -> syn::Result<Vec<Self>> {
        let mut arms = Vec::new();
        while !input.is_empty() {
            arms.push(input.parse()?);
        }
        Ok(arms)
    }
}

impl Parse for DeferredArm {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pat = SpecializePat::parse(input)?;
        let _: syn::Token![=>] = input.parse()?;
        let body = syn::Expr::parse_with_earlier_boundary_rule(input)?;
        consume_arm_comma(input, &body)?;
        Ok(Self {
            pat,
            body: Box::new(body),
        })
    }
}

struct InlineArm {
    pub pat: SpecializePat,
    pub result_ty: syn::Type,
    pub body: Box<syn::Expr>,
}

impl InlineArm {
    fn parse_multiple(input: ParseStream) -> syn::Result<Vec<Self>> {
        let mut arms = Vec::new();
        while !input.is_empty() {
            arms.push(input.parse()?);
        }
        Ok(arms)
    }
}

impl Parse for InlineArm {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // `pat : ArmType => expr` - the pattern's bound list stops at `:`,
        // the result type parses up to `=>`.
        let pat = SpecializePat::parse(input)?;
        let _: syn::Token![:] = input.parse()?;
        let result_ty = input.parse()?;
        let _: syn::Token![=>] = input.parse()?;
        let body = syn::Expr::parse_with_earlier_boundary_rule(input)?;
        consume_arm_comma(input, &body)?;
        Ok(Self {
            pat,
            result_ty,
            body: Box::new(body),
        })
    }
}

/// Block-bodied expressions (`if`, `match`, ...) need no trailing comma; others
/// require one unless they are the last arm.
fn consume_arm_comma(input: ParseStream, body: &syn::Expr) -> syn::Result<()> {
    let needs_comma = !matches!(
        body,
        syn::Expr::If(_)
            | syn::Expr::Match(_)
            | syn::Expr::Block(_)
            | syn::Expr::Unsafe(_)
            | syn::Expr::While(_)
            | syn::Expr::Loop(_)
            | syn::Expr::ForLoop(_)
            | syn::Expr::TryBlock(_)
            | syn::Expr::Const(_)
    );
    if needs_comma {
        let _: syn::Token![,] = input.parse()?;
    } else {
        let _: Option<syn::Token![,]> = input.parse()?;
    }
    Ok(())
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

/// Find and validate the type variable binding across all selectors in an arm.
fn resolve_type_var_binding<'a>(
    selectors: impl IntoIterator<Item = &'a SpecializePatSelector>,
) -> Option<syn::Ident> {
    let mut found: Option<&syn::Ident> = None;

    for selector in selectors {
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
    match input {
        SpecializeInput::Deferred(deferred) => codegen_deferred(&deferred),
        SpecializeInput::Inline(inline) => codegen_inline(&inline),
    }
    .into()
}

/// A resolved selector for the deferred form.
struct DeferredResolvedSelector<'a> {
    trait_name: syn::Ident,
    selector: &'a SpecializePatSelector,
    type_var_binding: Option<syn::Ident>,
    body: &'a syn::Expr,
    ref_count: usize,
}

fn resolve_deferred_selectors(input: &DeferredInput) -> Vec<DeferredResolvedSelector<'_>> {
    let total: usize = input.arms.iter().map(|a| a.pat.selectors.len()).sum();
    let mut out = Vec::with_capacity(total);

    for (arm_index, arm) in input.arms.iter().enumerate() {
        let type_var_binding = resolve_type_var_binding(&arm.pat.selectors);
        for (selector_index, selector) in arm.pat.selectors.iter().enumerate() {
            let flat_index = out.len();
            out.push(DeferredResolvedSelector {
                trait_name: quote::format_ident!(
                    "SpecializePattern{}Selector{}",
                    arm_index,
                    selector_index
                ),
                selector,
                type_var_binding: type_var_binding.clone(),
                body: &arm.body,
                ref_count: total - 1 - flat_index,
            });
        }
    }

    out
}

fn codegen_deferred(input: &DeferredInput) -> proc_macro2::TokenStream {
    let selectors = resolve_deferred_selectors(input);
    let return_type = &input.specialize_output;
    let test_type = &input.specialize_on;

    let resolvers = selectors
        .iter()
        .map(|s| gen_deferred_resolver(return_type, s));
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
}

fn gen_deferred_resolver(
    return_type: &syn::Type,
    resolved: &DeferredResolvedSelector<'_>,
) -> proc_macro2::TokenStream {
    let internal_type_param = syn::Ident::new("__SpecializeT", proc_macro2::Span::mixed_site());
    let trait_name = &resolved.trait_name;
    let body = resolved.body;
    let refs = std::iter::repeat_n(quote::quote! { & }, resolved.ref_count);

    let where_clause = selector_where_clause(resolved.selector, &internal_type_param);

    let (inner_fn, inner_call) = match (&resolved.type_var_binding, resolved.selector) {
        (Some(user_binding), SpecializePatSelector::Bounded { bounds, .. }) => (
            quote::quote! {
                fn do_specialize<#user_binding: #bounds>() -> #return_type {
                    #body
                }
            },
            quote::quote! { do_specialize::<#internal_type_param>() },
        ),
        _ => (
            quote::quote! {
                fn do_specialize() -> #return_type {
                    #body
                }
            },
            quote::quote! { do_specialize() },
        ),
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

struct InlineResolvedSelector<'a> {
    trait_name: syn::Ident,
    selector: &'a SpecializePatSelector,
    type_var_binding: Option<syn::Ident>,
    result_ty: &'a syn::Type,
    body: &'a syn::Expr,
    ref_count: usize,
}

fn resolve_inline_selectors(input: &InlineInput) -> Vec<InlineResolvedSelector<'_>> {
    let total: usize = input.arms.iter().map(|a| a.pat.selectors.len()).sum();
    let mut out = Vec::with_capacity(total);

    for (arm_index, arm) in input.arms.iter().enumerate() {
        let type_var_binding = resolve_type_var_binding(&arm.pat.selectors);
        for (selector_index, selector) in arm.pat.selectors.iter().enumerate() {
            let flat_index = out.len();
            out.push(InlineResolvedSelector {
                trait_name: quote::format_ident!(
                    "SpecializePattern{}Selector{}",
                    arm_index,
                    selector_index
                ),
                selector,
                type_var_binding: type_var_binding.clone(),
                result_ty: &arm.result_ty,
                body: &arm.body,
                ref_count: total - 1 - flat_index,
            });
        }
    }

    out
}

fn codegen_inline(input: &InlineInput) -> proc_macro2::TokenStream {
    let selectors = resolve_inline_selectors(input);
    let binding = &input.binding;

    // The scrutinee `&mut T` is tested as `T`; the checker holds `*mut T`.
    let (test_type, ptr_mutability) = match strip_reference(&input.scrutinee_ty) {
        Some(parts) => parts,
        None => {
            proc_macro_error::abort!(
                input.scrutinee_ty,
                "the inline form requires a reference scrutinee, e.g. `name: &mut Self`"
            );
        }
    };

    let resolvers = selectors
        .iter()
        .map(|s| gen_inline_resolver(binding, &ptr_mutability, s));
    let test_refs = std::iter::repeat_n(quote::quote! { & }, selectors.len());

    // The checker is generic in its pointee; the concrete pointer fixes it.
    let checker_param = syn::Ident::new("__C", proc_macro2::Span::mixed_site());
    let field_ty = ptr_mutability.ptr_to(&checker_param);
    let local_ptr_ty = ptr_mutability.ptr_to(&test_type);

    quote::quote! {
        {
            struct SpecializeChecker<#checker_param: ?::core::marker::Sized>(#field_ty);

            #(#resolvers)*

            let __specialize_ptr: #local_ptr_ty = #binding;
            (#(#test_refs)* SpecializeChecker(__specialize_ptr)).specialize()
        }
    }
}

fn gen_inline_resolver(
    value_binding: &syn::Ident,
    ptr_mutability: &PtrMutability,
    resolved: &InlineResolvedSelector<'_>,
) -> proc_macro2::TokenStream {
    let internal_type_param = syn::Ident::new("__SpecializeT", proc_macro2::Span::mixed_site());
    let trait_name = &resolved.trait_name;
    let result_ty = resolved.result_ty;
    let body = resolved.body;
    let refs = std::iter::repeat_n(quote::quote! { & }, resolved.ref_count);

    // Use the user's type binding (so `result_ty`/`body` can mention it) as the
    // impl's generic; fall back to an internal name otherwise.
    let type_param = match (&resolved.type_var_binding, resolved.selector) {
        (Some(user_binding), SpecializePatSelector::Bounded { .. }) => user_binding.clone(),
        _ => internal_type_param.clone(),
    };

    let bound_clause = match resolved.selector {
        SpecializePatSelector::Bounded { bounds, .. } if !bounds.is_empty() => quote::quote!(#bounds +),
        _ => quote::quote!(),
    };

    let value_ref = ptr_mutability.ref_to(&type_param);
    let deref = ptr_mutability.deref();

    quote::quote! {
        trait #trait_name {
            type Out;
            fn specialize(&self) -> Self::Out;
        }

        impl<#type_param: #bound_clause ?::core::marker::Sized> #trait_name
            for #(#refs)* SpecializeChecker<#type_param>
        {
            type Out = #result_ty;

            #[allow(unused_variables, unused_unsafe)]
            fn specialize(&self) -> Self::Out {
                // SAFETY: the scrutinee value (`#value_binding`) outlives this
                // selection; exactly one selector's `specialize` runs, so the
                // reborrow is unique. The pointer rides through the checker only
                // to keep `&self`-method autoref working.
                let #value_binding: #value_ref = unsafe { #deref self.0 };
                #body
            }
        }
    }
}

/// `*mut` / `*const` plus the matching reference shapes, derived from the
/// scrutinee's mutability.
struct PtrMutability {
    is_mut: bool,
}

impl PtrMutability {
    fn ptr_to(&self, ty: &impl quote::ToTokens) -> proc_macro2::TokenStream {
        if self.is_mut {
            quote::quote!(*mut #ty)
        } else {
            quote::quote!(*const #ty)
        }
    }

    fn ref_to(&self, ty: &impl quote::ToTokens) -> proc_macro2::TokenStream {
        if self.is_mut {
            quote::quote!(&mut #ty)
        } else {
            quote::quote!(&#ty)
        }
    }

    fn deref(&self) -> proc_macro2::TokenStream {
        if self.is_mut {
            quote::quote!(&mut *)
        } else {
            quote::quote!(&*)
        }
    }
}

/// Strip one reference layer: `&mut T` -> (`T`, mut), `&T` -> (`T`, shared).
fn strip_reference(ty: &syn::Type) -> Option<(syn::Type, PtrMutability)> {
    match ty {
        syn::Type::Reference(r) => Some((
            (*r.elem).clone(),
            PtrMutability {
                is_mut: r.mutability.is_some(),
            },
        )),
        _ => None,
    }
}

fn selector_where_clause(
    selector: &SpecializePatSelector,
    type_param: &syn::Ident,
) -> Option<syn::WhereClause> {
    match selector {
        SpecializePatSelector::Wildcard => None,
        SpecializePatSelector::Bounded { bounds, .. } => {
            let mut predicates = Punctuated::new();
            predicates.push_value(syn::WherePredicate::Type(syn::PredicateType {
                lifetimes: None,
                bounded_ty: syn::Type::Path(syn::TypePath {
                    qself: None,
                    path: type_param.clone().into(),
                }),
                colon_token: Default::default(),
                bounds: bounds.clone(),
            }));

            Some(syn::WhereClause {
                where_token: Default::default(),
                predicates,
            })
        }
    }
}
