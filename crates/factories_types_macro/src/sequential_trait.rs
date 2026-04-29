use proc_macro2::Ident;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{GenericParam, LitInt, LitStr, Type};

struct SequentialTraitArgs {
    count: usize,
    description: Option<String>,
}

impl Parse for SequentialTraitArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();

        if input.is_empty() {
            proc_macro_error::emit_error!(
                input.span(),
                "Missing required parameter `count`";
                help = "You must specify the count of the sequential trait using `count = <number>` or just `<number>` syntax"
            );
            return Ok(Self {
                count: 0,
                description: None,
            });
        }

        if lookahead.peek(syn::LitInt) {
            let count = input.parse::<LitInt>()?.base10_parse()?;

            if !input.is_empty() {
                proc_macro_error::emit_error!(
                    input.span(),
                    "Unexpected token, expected )";
                    help = "If you wanted to add additional parameters, use count = {}, description = \"...\" syntax", count
                );
            }

            Ok(Self {
                count,
                description: None,
            })
        } else if lookahead.peek(syn::Ident) {
            use syn::parse::Parser;
            let mut count = None;
            let mut description = None;

            let parser = syn::meta::parser(|meta| {
                if meta.path.is_ident("count") {
                    let value = meta.value()?;
                    let c: LitInt = value.parse()?;
                    count = Some((c.base10_parse()?, c.span()));
                    Ok(())
                } else if meta.path.is_ident("description") {
                    let value = meta.value()?;
                    let s: LitStr = value.parse()?;
                    description = Some(s.value());
                    Ok(())
                } else {
                    Err(meta.error("unknown parameter"))
                }
            });

            parser.parse2(input.cursor().token_stream())?;

            let count = match count {
                Some((v, _)) if v > 0 => v,
                Some((v, span)) => {
                    proc_macro_error::emit_warning!(
                        span,
                        "The count must be a positive integer";
                        help = "Using a non-positive count doesn't do anything"
                    );
                    v
                }
                None => {
                    proc_macro_error::emit_error!(
                        input.span(),
                        "Missing required parameter `count`";
                        help = "You must specify the count of the sequential trait using `count = <number>`"
                    );
                    0
                }
            };

            Ok(Self { count, description })
        } else {
            Err(lookahead.error())
        }
    }
}

pub(super) fn proc_macro_sequential_trait(
    attrs: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let args = syn::parse_macro_input!(attrs as SequentialTraitArgs);
    if args.count == 0 {
        // Uhhh sure...
        return input;
    }

    let mut base_trait = syn::parse_macro_input!(input as syn::ItemTrait);
    let trait_name = base_trait.ident.clone();

    let sealed_mod_name = quote::format_ident!("sealed_{}", trait_name);
    let valid_trait_name = quote::format_ident!("Valid{}Index", trait_name);

    let on_unimplemented_label = args
        .description
        .as_ref()
        .map(|desc| format!("Only up to {} {} indices are allowed", args.count, desc))
        .unwrap_or_else(|| format!("Only up to {} indices are allowed", args.count));

    let valid_impls = (0..args.count).map(|n| {
        quote::quote! {
            impl<T> #valid_trait_name<#n> for T {}
        }
    });

    let mut const_generic_size_ident = None;

    for generic_base_param in &base_trait.generics.params {
        let GenericParam::Const(const_param) = generic_base_param else {
            continue;
        };

        if !matches!(&const_param.ty, Type::Path(v) if v.path.is_ident("usize")) {
            continue;
        }

        const_generic_size_ident = Some(const_param.ident.clone());
        break;
    }

    let const_generic_size_ident = match const_generic_size_ident {
        None => {
            proc_macro_error::emit_error!(
                base_trait.generics.span(),
                "The trait must have a const generic parameter of type usize";
                help = "Add a const generic parameter of type usize to the trait, e.g. `trait MyTrait<const N: usize>`"
            );
            Ident::new("N", base_trait.generics.span())
        }
        Some(v) => v,
    };

    base_trait.supertraits.push(syn::parse_quote_spanned! {
        base_trait.span() =>
        #sealed_mod_name::#valid_trait_name<#const_generic_size_ident>
    });

    quote::quote! {
        #base_trait

        mod #sealed_mod_name {
            #[diagnostic::on_unimplemented(
                message = "the hook index `{N}` is not valid",
                label = #on_unimplemented_label,
                // note = concat!("{Self} only supports up to `{N}` implementations of type ", stringify!(#trait_name))
            )]
            pub(crate) trait #valid_trait_name<const N: usize> {}

            #(#valid_impls)*
        }
    }
        .into()
}
