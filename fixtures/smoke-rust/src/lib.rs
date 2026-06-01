pub fn helper() -> &'static str {
    "ready"
}

pub fn entry() -> &'static str {
    helper()
}

pub fn other() -> &'static str {
    entry()
}
