pub fn inherited_child_probe(enabled: bool) -> &'static str {
    if enabled { "inherited" } else { "disabled" }
}

pub fn background_child_probe(enabled: bool) -> &'static str {
    if enabled { "background" } else { "disabled" }
}

pub fn late_child_probe(enabled: bool) -> &'static str {
    if enabled { "late" } else { "disabled" }
}

