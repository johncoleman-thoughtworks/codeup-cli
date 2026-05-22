//! Per-language import extractors — mirror TS `scanner/imports.ts`.
//!
//! Regex-based on purpose. Tree-sitter is a future upgrade; current
//! coverage handles Java/Kotlin/Scala, TS/JS, Python, Go, C#.

use regex::Regex;
use std::sync::OnceLock;

pub fn extract_imports(language: &str, text: &str) -> Vec<String> {
    match language {
        "java" | "kotlin" | "scala" => jvm_imports(text),
        "typescript" | "typescriptreact" | "javascript" | "javascriptreact" => js_imports(text),
        "python" => python_imports(text),
        "go" => go_imports(text),
        "csharp" => csharp_imports(text),
        _ => Vec::new(),
    }
}

// import com.example.Foo;        → "com.example.Foo"
// import com.example.*;          → "com.example.*"
// import static com.x.Y.method;  → "com.x.Y"   (drop tail member)
fn jvm_imports(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*import\s+(static\s+)?([a-zA-Z_][\w.]*\*?)\s*;?\s*$").unwrap()
    });
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        let is_static = cap.get(1).is_some();
        let mut imp = cap[2].to_string();
        if is_static && !imp.ends_with(".*") {
            if let Some(idx) = imp.rfind('.') {
                if idx > 0 {
                    imp.truncate(idx);
                }
            }
        }
        out.push(imp);
    }
    out
}

// import ... from 'x'  |  import 'x'  |  require('x')  |  import('x')
fn js_imports(text: &str) -> Vec<String> {
    static GENERAL: OnceLock<Regex> = OnceLock::new();
    static BARE: OnceLock<Regex> = OnceLock::new();
    let general = GENERAL.get_or_init(|| {
        Regex::new(r#"(?:from|require\(|import\()\s*['"]([^'"]+)['"]\)?"#).unwrap()
    });
    let bare = BARE.get_or_init(|| {
        Regex::new(r#"(?m)^\s*import\s+['"]([^'"]+)['"]\s*;?\s*$"#).unwrap()
    });
    let mut out = Vec::new();
    for cap in general.captures_iter(text) {
        out.push(cap[1].to_string());
    }
    for cap in bare.captures_iter(text) {
        out.push(cap[1].to_string());
    }
    out
}

// from a.b.c import x  →  "a.b.c"
// import a.b           →  "a.b"
// import a, b          →  "a", "b"
fn python_imports(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*(?:from\s+([\w.]+)\s+import\s+.+|import\s+([\w. ,]+))$").unwrap()
    });
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(from_mod) = cap.get(1) {
            out.push(from_mod.as_str().to_string());
        } else if let Some(import_list) = cap.get(2) {
            for part in import_list.as_str().split(',') {
                let name = part.split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() {
                    out.push(name);
                }
            }
        }
    }
    out
}

// import "github.com/x/y"   (single)
// import (   "a"   "b"   )  (block)
fn go_imports(text: &str) -> Vec<String> {
    static SINGLE: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static INSIDE: OnceLock<Regex> = OnceLock::new();
    let single = SINGLE.get_or_init(|| {
        Regex::new(r#"(?m)^\s*import\s+(?:\w+\s+)?"([^"]+)"\s*$"#).unwrap()
    });
    let block = BLOCK.get_or_init(|| {
        Regex::new(r"(?ms)^\s*import\s*\((.*?)\)").unwrap()
    });
    let inside = INSIDE.get_or_init(|| Regex::new(r#"(?:\w+\s+)?"([^"]+)""#).unwrap());

    let mut out = Vec::new();
    for cap in single.captures_iter(text) {
        out.push(cap[1].to_string());
    }
    for cap in block.captures_iter(text) {
        for inner_cap in inside.captures_iter(&cap[1]) {
            out.push(inner_cap[1].to_string());
        }
    }
    out
}

// using Foo.Bar;          →  "Foo.Bar"
// using static Foo.Bar;   →  "Foo.Bar"
fn csharp_imports(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*using\s+(?:static\s+)?([\w.]+)\s*;\s*$").unwrap()
    });
    re.captures_iter(text).map(|c| c[1].to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_extracts_dotted_and_static() {
        let src = r#"
package com.example;

import java.util.List;
import java.util.Map;
import static com.example.util.Strings.isBlank;
import com.example.other.*;

public class Foo {}
"#;
        let mut got = extract_imports("java", src);
        got.sort();
        assert_eq!(
            got,
            vec![
                "com.example.other.*",
                "com.example.util.Strings",
                "java.util.List",
                "java.util.Map",
            ]
        );
    }

    #[test]
    fn typescript_from_require_dynamic_and_bare() {
        let src = r#"
import { a } from './a';
import b from "b";
import 'side-effect';
const c = require('c');
async function f() { return await import('./d'); }
"#;
        let mut got = extract_imports("typescript", src);
        got.sort();
        assert_eq!(got, vec!["./a", "./d", "b", "c", "side-effect"]);
    }

    #[test]
    fn python_imports_both_forms() {
        let src = r#"
import os
import sys, json
from app.services import x
from collections.abc import Mapping
import numpy as np
"#;
        let mut got = extract_imports("python", src);
        got.sort();
        assert_eq!(
            got,
            vec!["app.services", "collections.abc", "json", "numpy", "os", "sys"]
        );
    }

    #[test]
    fn go_single_and_block() {
        let src = r#"
package main

import "fmt"
import (
  "os"
  alias "path/filepath"
)
"#;
        let mut got = extract_imports("go", src);
        got.sort();
        assert_eq!(got, vec!["fmt", "os", "path/filepath"]);
    }

    #[test]
    fn csharp_using_and_static_using() {
        let src = r#"
using System;
using System.Collections.Generic;
using static System.Math;
"#;
        let mut got = extract_imports("csharp", src);
        got.sort();
        assert_eq!(got, vec!["System", "System.Collections.Generic", "System.Math"]);
    }

    #[test]
    fn unsupported_language_returns_empty() {
        assert!(extract_imports("markdown", "whatever").is_empty());
    }
}
