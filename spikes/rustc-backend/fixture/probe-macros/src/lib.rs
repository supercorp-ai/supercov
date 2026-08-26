use proc_macro::TokenStream;

#[proc_macro]
pub fn generated_function(_input: TokenStream) -> TokenStream {
    "pub fn generated_by_proc(value: bool) -> usize { if value { 17 } else { 19 } }"
        .parse()
        .expect("valid generated Rust")
}

#[proc_macro]
pub fn generated_match_function(_input: TokenStream) -> TokenStream {
    "pub fn generated_match_by_proc(value: bool) -> usize { match value { true => 31, false => 37 } }"
        .parse()
        .expect("valid generated Rust match")
}

#[proc_macro]
pub fn generated_guarded_match_function(_input: TokenStream) -> TokenStream {
    "pub fn generated_guarded_match_by_proc(value: Option<usize>, enabled: bool) -> usize { match value { Some(value) if value > 0 && enabled => value, Some(_) => 2, None => 0 } }"
        .parse()
        .expect("valid generated guarded Rust match")
}

#[proc_macro_attribute]
pub fn generated_test(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let mut output: TokenStream = "#[test]".parse().expect("valid test attribute");
    output.extend(item);
    output
}
