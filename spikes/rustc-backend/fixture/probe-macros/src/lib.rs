use proc_macro::TokenStream;

#[proc_macro]
pub fn generated_function(_input: TokenStream) -> TokenStream {
    "pub fn generated_by_proc(value: bool) -> usize { if value { 17 } else { 19 } }"
        .parse()
        .expect("valid generated Rust")
}

#[proc_macro_attribute]
pub fn generated_test(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let mut output: TokenStream = "#[test]".parse().expect("valid test attribute");
    output.extend(item);
    output
}
