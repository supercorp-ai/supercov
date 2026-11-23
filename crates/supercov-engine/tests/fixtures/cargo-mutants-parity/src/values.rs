//! Return types, nested.

use std::collections::HashMap;

pub fn result_option() -> Result<Option<bool>, ()> {
    Ok(None)
}

pub fn option_vec() -> Option<Vec<u8>> {
    None
}

pub fn vec_of_tuples() -> Vec<(String, i8)> {
    Vec::new()
}

pub fn map_of_vecs() -> HashMap<u8, Vec<u8>> {
    HashMap::new()
}

pub fn boxed_slice() -> Box<[u8]> {
    Box::new([])
}

pub fn boxed_str() -> Box<str> {
    "x".into()
}

pub fn static_slice() -> &'static [&'static str] {
    &["a"]
}

pub fn nested_result() -> std::io::Result<String> {
    Ok(String::new())
}

pub fn paren() -> (u8) {
    1
}

pub fn unit_tuple() -> () {
    let x = 1;
}

pub fn self_type() -> Option<Self_> {
    None
}

pub struct Self_;

pub struct Holder {
    items: Vec<u8>,
}

impl Holder {
    pub fn items(&self) -> &Vec<u8> {
        &self.items
    }

    pub fn items_mut(&mut self) -> &mut Vec<u8> {
        &mut self.items
    }

    pub fn first(&self) -> Option<&u8> {
        self.items.first()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

impl Iterator for Holder {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        self.items.pop()
    }
}
