// Copyright (c) 2019, Facebook, Inc.
// All rights reserved.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.

use facts_rust as facts;
use hhbc_string_utils_rust::without_xhp_mangling;
use libc::c_char;
use ocamlrep::{bytes_from_ocamlrep, ptr::UnsafeOcamlPtr};
use ocamlrep_ocamlpool::ocaml_ffi;
use oxidized::relative_path::RelativePath;

use facts::facts_parser::*;

#[no_mangle]
extern "C" fn extract_as_json_cpp_ffi(
    flags: i32,
    filename: *const c_char,
    text_ptr: *const c_char,
    mangle_xhp: bool,
) -> *const c_char {
    let text = cpp_helper::cstr::to_u8(text_ptr);
    let filename = RelativePath::make(
        oxidized::relative_path::Prefix::Dummy,
        std::path::PathBuf::from(cpp_helper::cstr::to_str(filename)),
    );
    match extract_as_json_ffi0(
        ((1 << 0) & flags) != 0, // php5_compat_mode
        ((1 << 1) & flags) != 0, // hhvm_compat_mode
        ((1 << 2) & flags) != 0, // allow_new_attribute_syntax
        ((1 << 3) & flags) != 0, // enable_xhp_class_modifier
        ((1 << 4) & flags) != 0, // disable_xhp_element_mangling
        filename,
        text,
        mangle_xhp,
    ) {
        Some(s) => {
            let cs = std::ffi::CString::new(s)
                .expect("rust_facts_ffi: extract_as_json_cpp_ffi: String::new failed");
            cs.into_raw() as *const c_char
        }
        None => std::ptr::null(),
    }
}

// Return a result of `extract_as_json_cpp_ffi` to Rust.
#[no_mangle]
extern "C" fn extract_as_json_free_string_cpp_ffi(s: *mut c_char) {
    let _ = unsafe { std::ffi::CString::from_raw(s) };
}

ocaml_ffi! {
    fn extract_as_json_ffi(
        flags: i32,
        filename: RelativePath,
        text_ptr: UnsafeOcamlPtr,
        mangle_xhp: bool,
    ) -> Option<String> {
        // Safety: the OCaml garbage collector must not run as long as text_ptr
        // and text_value exist. We don't call into OCaml here, so it won't.
        let text_value = unsafe { text_ptr.as_value() };
        let text = bytes_from_ocamlrep(text_value).expect("expected string");
        extract_as_json_ffi0(
            ((1 << 0) & flags) != 0, // php5_compat_mode
            ((1 << 1) & flags) != 0, // hhvm_compat_mode
            ((1 << 2) & flags) != 0, // allow_new_attribute_syntax
            ((1 << 3) & flags) != 0, // enable_xhp_class_modifier
            ((1 << 4) & flags) != 0, // disable_xhp_element_mangling
            filename,
            text,
            mangle_xhp,
        )
    }
}

fn extract_as_json_ffi0(
    php5_compat_mode: bool,
    hhvm_compat_mode: bool,
    allow_new_attribute_syntax: bool,
    enable_xhp_class_modifier: bool,
    disable_xhp_element_mangling: bool,
    filename: RelativePath,
    text: &[u8],
    mangle_xhp: bool,
) -> Option<String> {
    let opts = ExtractAsJsonOpts {
        php5_compat_mode,
        hhvm_compat_mode,
        allow_new_attribute_syntax,
        enable_xhp_class_modifier,
        disable_xhp_element_mangling,
        filename,
    };
    if mangle_xhp {
        extract_as_json(text, opts)
    } else {
        without_xhp_mangling(|| extract_as_json(text, opts))
    }
}
