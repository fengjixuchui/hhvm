(*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *)

type decls = (string * Shallow_decl_defs.decl) list [@@deriving show]

type ns_map = (string * string) list

external parse_decls_and_mode_ffi :
  Relative_path.t -> string -> ns_map -> decls * FileInfo.mode option
  = "hh_parse_decls_and_mode_ffi"

let parse_decls_ffi (path : Relative_path.t) (text : string) (ns_map : ns_map) :
    decls =
  parse_decls_and_mode_ffi path text ns_map |> fst
