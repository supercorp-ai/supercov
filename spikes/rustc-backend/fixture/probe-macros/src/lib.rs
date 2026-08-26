use proc_macro::TokenStream;

#[proc_macro]
pub fn generated_function(_input: TokenStream) -> TokenStream {
    "pub fn generated_by_proc(value: bool) -> usize { if value { 17 } else { 19 } }"
        .parse()
        .expect("valid generated Rust")
}
