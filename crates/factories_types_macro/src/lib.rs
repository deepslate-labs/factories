mod specialize;
mod sequential_trait;

#[proc_macro_error::proc_macro_error]
#[proc_macro]
pub fn match_specialize(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    specialize::proc_macro_specialize(input)
}

#[proc_macro_error::proc_macro_error]
#[proc_macro_attribute]
pub fn sequential_trait(attrs: proc_macro::TokenStream, input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    sequential_trait::proc_macro_sequential_trait(attrs, input)
}
