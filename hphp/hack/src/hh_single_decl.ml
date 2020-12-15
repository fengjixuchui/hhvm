(*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 *)

open Hh_prelude
open Direct_decl_parser

let popt ~auto_namespace_map ~enable_xhp_class_modifier =
  let po = ParserOptions.default in
  let po = ParserOptions.with_disable_xhp_element_mangling po false in
  let po = ParserOptions.with_auto_namespace_map po auto_namespace_map in
  let po =
    ParserOptions.with_enable_xhp_class_modifier po enable_xhp_class_modifier
  in
  po

let init root popt : Provider_context.t =
  Relative_path.(set_path_prefix Root root);
  let (_handle : SharedMem.handle) =
    SharedMem.init ~num_workers:0 SharedMem.default_config
  in
  let tcopt =
    {
      TypecheckerOptions.default with
      GlobalOptions.tco_shallow_class_decl = true;
      tco_higher_kinded_types = true;
    }
  in
  (* TODO(hverr): Figure out 64-bit *)
  let ctx =
    Provider_context.empty_for_tool
      ~popt
      ~tcopt
      ~backend:Provider_backend.Shared_memory
      ~deps_mode:Typing_deps_mode.SQLiteMode
  in

  (* Push local stacks here so we don't include shared memory in our timing. *)
  File_provider.local_changes_push_sharedmem_stack ();
  Decl_provider.local_changes_push_sharedmem_stack ();
  Shallow_classes_provider.local_changes_push_sharedmem_stack ();
  Linearization_provider.local_changes_push_sharedmem_stack ();

  ctx

let rec shallow_declare_ast ctx decls prog =
  List.fold prog ~init:decls ~f:(fun decls def ->
      let open Aast in
      match def with
      | Namespace (_, prog) -> shallow_declare_ast ctx decls prog
      | NamespaceUse _ -> decls
      | SetNamespaceEnv _ -> decls
      | FileAttributes _ -> decls
      | Fun f ->
        let (name, decl) = Decl_nast.fun_naming_and_decl ctx f in
        (name, Shallow_decl_defs.Fun decl) :: decls
      | Class c ->
        let decl = Shallow_classes_provider.decl ctx c in
        let (_, name) = decl.Shallow_decl_defs.sc_name in
        (name, Shallow_decl_defs.Class decl) :: decls
      | RecordDef rd ->
        let (name, decl) = Decl_nast.record_def_naming_and_decl ctx rd in
        (name, Shallow_decl_defs.Record decl) :: decls
      | Typedef typedef ->
        let (name, decl) = Decl_nast.typedef_naming_and_decl ctx typedef in
        (name, Shallow_decl_defs.Typedef decl) :: decls
      | Stmt _ -> decls
      | Constant cst ->
        let (name, ty) = Decl_nast.const_naming_and_decl ctx cst in
        let decl = Typing_defs.{ cd_pos = fst cst.cst_name; cd_type = ty } in
        (name, Shallow_decl_defs.Const decl) :: decls)

let compare_decls ctx fn text =
  let (ctx, _entry) =
    Provider_context.(
      add_or_overwrite_entry_contents ~ctx ~path:fn ~contents:text)
  in
  let ast = Ast_provider.get_ast ctx fn in
  let legacy_decls = shallow_declare_ast ctx [] ast in
  let legacy_decls_str = show_decls (List.rev legacy_decls) ^ "\n" in
  let popt = Provider_context.get_popt ctx in
  let auto_namespace_map = ParserOptions.auto_namespace_map popt in
  let decls = parse_decls_ffi fn text auto_namespace_map in
  let decls_str = show_decls (List.rev decls) ^ "\n" in
  let matched = String.equal decls_str legacy_decls_str in
  if matched then
    Printf.eprintf "%s%!" decls_str
  else
    Tempfile.with_real_tempdir (fun dir ->
        let temp_dir = Path.to_string dir in
        let expected =
          Caml.Filename.temp_file ~temp_dir "expected_decls" ".txt"
        in
        let actual = Caml.Filename.temp_file ~temp_dir "actual_decls" ".txt" in
        Disk.write_file ~file:expected ~contents:legacy_decls_str;
        Disk.write_file ~file:actual ~contents:decls_str;
        Ppxlib_print_diff.print
          ~diff_command:"diff -U9999 --label legacy --label 'direct decl'"
          ~file1:expected
          ~file2:actual
          ());
  matched

type modes = CompareDirectDeclParser

let () =
  let usage =
    Printf.sprintf "Usage: %s [OPTIONS] mode filename\n" Sys.argv.(0)
  in
  let usage_and_exit () =
    prerr_endline usage;
    exit 1
  in
  let mode = ref None in
  let set_mode m () =
    match !mode with
    | None -> mode := Some m
    | Some _ -> usage_and_exit ()
  in
  let file = ref None in
  let set_file f =
    match !file with
    | None -> file := Some f
    | Some _ -> usage_and_exit ()
  in
  let skip_if_errors = ref false in
  let expect_extension = ref ".exp" in
  let set_expect_extension s = expect_extension := s in
  let auto_namespace_map = ref [] in
  let enable_xhp_class_modifier = ref false in
  Arg.parse
    [
      ( "--compare-direct-decl-parser",
        Arg.Unit (set_mode CompareDirectDeclParser),
        "(mode) Runs the direct decl parser against the FFP -> naming -> decl pipeline and compares their output"
      );
      ( "--skip-if-errors",
        Arg.Set skip_if_errors,
        "Skip comparison if the corresponding .exp file has errors" );
      ( "--expect-extension",
        Arg.String set_expect_extension,
        "The extension with which the output of the legacy pipeline should be written"
      );
      ( "--auto-namespace-map",
        Arg.String
          (fun m ->
            auto_namespace_map := ServerConfig.convert_auto_namespace_to_map m),
        "Namespace aliases" );
      ( "--enable-xhp-class-modifier",
        Arg.Set enable_xhp_class_modifier,
        "Enable the XHP class modifier, xhp class name {} will define an xhp class."
      );
    ]
    set_file
    usage;
  match !mode with
  | None -> usage_and_exit ()
  | Some CompareDirectDeclParser ->
    begin
      match !file with
      | None -> usage_and_exit ()
      | Some file ->
        let () =
          if
            !skip_if_errors
            && not
               @@ String_utils.is_substring
                    "No errors"
                    (RealDisk.cat (file ^ ".exp"))
          then begin
            print_endline "Skipping because input file has errors";
            exit 0
          end
        in
        let file = Path.make file in
        let auto_namespace_map = !auto_namespace_map in
        let enable_xhp_class_modifier = !enable_xhp_class_modifier in
        let popt = popt ~auto_namespace_map ~enable_xhp_class_modifier in
        let ctx = init (Path.dirname file) popt in
        let file = Relative_path.(create Root (Path.to_string file)) in
        let files = Multifile.file_to_file_list file in
        let num_files = List.length files in
        let (all_matched, _) =
          List.fold
            files
            ~init:(true, true)
            ~f:(fun (matched, is_first) (filename, contents) ->
              (* All output is printed to stderr because that's the output
                 channel Ppxlib_print_diff prints to. *)
              if not is_first then Printf.eprintf "\n%!";
              (* Multifile turns the path into an absolute path instead of a
                 relative one. Turn it back into a relative path. *)
              let filename =
                Relative_path.(create Root (Relative_path.to_absolute filename))
              in
              if num_files > 1 then
                Printf.eprintf
                  "File %s\n%!"
                  (Relative_path.storage_to_string filename);
              let matched =
                Provider_utils.respect_but_quarantine_unsaved_changes
                  ~ctx
                  ~f:(fun () -> compare_decls ctx filename contents)
                && matched
              in
              (matched, false))
        in
        if all_matched then
          Printf.eprintf "\nThey matched!\n%!"
        else
          exit 1
    end
