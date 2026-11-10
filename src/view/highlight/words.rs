//! Each programming language's comments and words for [`super::code`]. Keyword and type lists
//! are sorted byte-wise (a test checks).

use super::code::Spec;

pub(super) const C: Spec = Spec {
    line: &["//"],
    block: true,
    nest: false,
    keywords: C_KEYWORDS,
    types: C_TYPES,
    type_kw: &["class", "enum", "namespace", "new", "struct", "typename", "union"],
    fn_kw: &[],
    pascal: true,
    at_attr: false,
    ci: false,
    sync: &[
        "#define", "#if", "#ifdef", "#ifndef", "#include", "#pragma", "bool", "char", "class", "const", "double", "enum", "extern", "float", "inline", "int", "long",
        "namespace", "static", "struct", "template", "typedef", "union", "unsigned", "using", "void",
    ],
};

pub(super) const CSHARP: Spec = Spec {
    line: &["//"],
    block: true,
    nest: false,
    keywords: CSHARP_KEYWORDS,
    types: CSHARP_TYPES,
    type_kw: &["class", "enum", "interface", "namespace", "new", "record", "struct"],
    fn_kw: &[],
    pascal: true,
    at_attr: false,
    ci: false,
    sync: &["#region", "abstract", "class", "enum", "interface", "internal", "namespace", "partial", "public", "record", "sealed", "static", "struct", "using"],
};

pub(super) const JAVA: Spec = Spec {
    line: &["//"],
    block: true,
    nest: false,
    keywords: JAVA_KEYWORDS,
    types: JAVA_TYPES,
    type_kw: &["class", "enum", "extends", "implements", "interface", "new", "record"],
    fn_kw: &[],
    pascal: true,
    at_attr: true,
    ci: false,
    sync: &["@", "abstract", "class", "enum", "final", "import", "interface", "package", "public", "record"],
};

pub(super) const JS: Spec = Spec {
    line: &["//"],
    block: true,
    nest: false,
    keywords: JS_KEYWORDS,
    types: &[],
    type_kw: &["class", "extends", "new"],
    fn_kw: &["function"],
    pascal: true,
    at_attr: true,
    ci: false,
    sync: JS_SYNC,
};

pub(super) const TS: Spec = Spec {
    line: &["//"],
    block: true,
    nest: false,
    keywords: TS_KEYWORDS,
    types: TS_TYPES,
    type_kw: &["class", "enum", "extends", "implements", "interface", "namespace", "new", "type"],
    fn_kw: &["function"],
    pascal: true,
    at_attr: true,
    ci: false,
    sync: JS_SYNC,
};

pub(super) const GO: Spec = Spec {
    line: &["//"],
    block: true,
    nest: false,
    keywords: GO_KEYWORDS,
    types: GO_TYPES,
    type_kw: &["type"],
    fn_kw: &["func"],
    // Exported functions and variables are capitalised too.
    pascal: false,
    at_attr: false,
    ci: false,
    sync: &["const", "func", "import", "package", "type", "var"],
};

pub(super) const RUST: Spec = Spec {
    line: &["//"],
    block: true,
    nest: true,
    keywords: RUST_KEYWORDS,
    types: RUST_TYPES,
    type_kw: &["enum", "impl", "struct", "trait", "type"],
    fn_kw: &["fn"],
    pascal: true,
    at_attr: false,
    ci: false,
    sync: &["#[", "#![", "async", "const", "enum", "extern", "fn", "impl", "macro_rules!", "mod", "pub", "static", "struct", "trait", "type", "unsafe", "use"],
};

pub(super) const PHP: Spec = Spec {
    line: &["//", "#"],
    block: true,
    nest: false,
    keywords: PHP_KEYWORDS,
    types: PHP_TYPES,
    type_kw: &["class", "enum", "extends", "implements", "instanceof", "interface", "new", "trait"],
    fn_kw: &["fn", "function"],
    pascal: true,
    at_attr: false,
    ci: true,
    sync: &["<?php", "#[", "abstract", "class", "enum", "final", "function", "interface", "namespace", "trait", "use"],
};

/// PHP's keywords in lower case (they are matched ignoring case).
const PHP_KEYWORDS: &[&str] = &[
    "__class__", "__dir__", "__file__", "__function__", "__line__", "__method__", "__namespace__", "__trait__", "abstract", "and", "array", "as", "break",
    "callable", "case", "catch", "class", "clone", "const", "continue", "declare", "default", "do", "echo", "else", "elseif", "empty", "enddeclare", "endfor",
    "endforeach", "endif", "endswitch", "endwhile", "enum", "extends", "false", "final", "finally", "fn", "for", "foreach", "function", "global", "goto", "if",
    "implements", "include", "include_once", "instanceof", "insteadof", "interface", "isset", "list", "match", "namespace", "new", "null", "or", "parent", "print",
    "private", "protected", "public", "readonly", "require", "require_once", "return", "self", "static", "switch", "throw", "trait", "true", "try", "unset", "use",
    "var", "while", "xor", "yield",
];
const PHP_TYPES: &[&str] = &["bool", "float", "int", "iterable", "mixed", "never", "object", "string", "void"];

const JS_SYNC: &[&str] =&["@", "abstract", "async", "class", "const", "declare", "enum", "export", "function", "import", "interface", "let", "namespace", "type", "var"];

const C_KEYWORDS: &[&str] = &[
    "_Alignas", "_Alignof", "_Atomic", "_Generic", "_Noreturn", "_Static_assert", "_Thread_local", "alignas", "alignof", "and", "and_eq", "asm", "auto", "bitand",
    "bitor", "break", "case", "catch", "class", "co_await", "co_return", "co_yield", "compl", "concept", "const", "const_cast", "consteval", "constexpr", "constinit",
    "continue", "decltype", "default", "delete", "do", "dynamic_cast", "else", "enum", "explicit", "export", "extern", "false", "final", "for", "friend", "goto",
    "if", "inline", "mutable", "namespace", "new", "noexcept", "not", "not_eq", "nullptr", "operator", "or", "or_eq", "override", "private", "protected", "public",
    "register", "reinterpret_cast", "requires", "restrict", "return", "sizeof", "static", "static_assert", "static_cast", "struct", "switch", "template", "this",
    "thread_local", "throw", "true", "try", "typedef", "typeid", "typename", "union", "using", "virtual", "volatile", "while", "xor", "xor_eq",
];
const C_TYPES: &[&str] = &[
    "FILE", "_Bool", "_Complex", "array", "bool", "char", "char16_t", "char32_t", "char8_t", "double", "float", "int", "int16_t", "int32_t", "int64_t", "int8_t",
    "intptr_t", "long", "map", "optional", "pair", "ptrdiff_t", "set", "shared_ptr", "short", "signed", "size_t", "ssize_t", "string", "uint16_t", "uint32_t",
    "uint64_t", "uint8_t", "uintptr_t", "unique_ptr", "unordered_map", "unordered_set", "unsigned", "vector", "void", "wchar_t", "wstring",
];

const CSHARP_KEYWORDS: &[&str] = &[
    "abstract", "and", "as", "async", "await", "base", "break", "case", "catch", "checked", "class", "const", "continue", "default", "delegate", "do", "dynamic",
    "else", "enum", "event", "explicit", "extern", "false", "finally", "fixed", "for", "foreach", "get", "goto", "if", "implicit", "in", "init", "interface",
    "internal", "is", "lock", "nameof", "namespace", "new", "not", "null", "operator", "or", "out", "override", "params", "partial", "private", "protected",
    "public", "readonly", "record", "ref", "required", "return", "sealed", "set", "sizeof", "stackalloc", "static", "struct", "switch", "this", "throw", "true",
    "try", "typeof", "unchecked", "unsafe", "using", "var", "virtual", "volatile", "when", "where", "while", "with", "yield",
];
const CSHARP_TYPES: &[&str] = &["bool", "byte", "char", "decimal", "double", "float", "int", "long", "nint", "nuint", "object", "sbyte", "short", "string", "uint", "ulong", "ushort", "void"];

const JAVA_KEYWORDS: &[&str] = &[
    "abstract", "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else", "enum", "extends", "false", "final", "finally", "for",
    "goto", "if", "implements", "import", "instanceof", "interface", "native", "new", "null", "package", "permits", "private", "protected", "public", "record",
    "return", "sealed", "static", "strictfp", "super", "switch", "synchronized", "this", "throw", "throws", "transient", "true", "try", "var", "volatile", "when",
    "while", "yield",
];
const JAVA_TYPES: &[&str] = &["boolean", "byte", "char", "double", "float", "int", "long", "short", "void"];

const JS_KEYWORDS: &[&str] = &[
    "Infinity", "NaN", "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "export",
    "extends", "false", "finally", "for", "from", "function", "if", "import", "in", "instanceof", "let", "new", "null", "of", "return", "static", "super",
    "switch", "this", "throw", "true", "try", "typeof", "undefined", "var", "void", "while", "with", "yield",
];
const TS_KEYWORDS: &[&str] = &[
    "Infinity", "NaN", "abstract", "as", "asserts", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "declare", "default",
    "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for", "from", "function", "if", "implements", "import", "in", "infer", "instanceof",
    "interface", "is", "keyof", "let", "module", "namespace", "new", "null", "of", "override", "private", "protected", "public", "readonly", "return", "satisfies",
    "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "unique", "var", "while", "with", "yield",
];
const TS_TYPES: &[&str] = &["any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown", "void"];

const GO_KEYWORDS: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "false", "for", "func", "go", "goto", "if", "import", "interface",
    "iota", "map", "nil", "package", "range", "return", "select", "struct", "switch", "true", "type", "var",
];
const GO_TYPES: &[&str] = &[
    "any", "bool", "byte", "comparable", "complex128", "complex64", "error", "float32", "float64", "int", "int16", "int32", "int64", "int8", "rune", "string",
    "uint", "uint16", "uint32", "uint64", "uint8", "uintptr",
];

const RUST_KEYWORDS: &[&str] = &[
    "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let",
    "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
];
const RUST_TYPES: &[&str] = &["bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128", "u16", "u32", "u64", "u8", "usize"];

/// After these words a `/` starts a regular expression, not a division.
pub(super) const JS_BEFORE_REGEX: &[&str] = &["await", "case", "delete", "do", "else", "in", "instanceof", "new", "of", "return", "throw", "typeof", "void", "yield"];

#[cfg(test)]
pub(super) const ALL: &[&Spec] = &[&C, &CSHARP, &JAVA, &JS, &TS, &GO, &RUST, &PHP];
