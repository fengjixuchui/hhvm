// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.

#![allow(unused_variables, dead_code)]

use ast_class_expr_rust::ClassExpr;
use ast_constant_folder_rust as ast_constant_folder;
use ast_scope_rust::Scope;
use emit_adata_rust as emit_adata;
use emit_fatal_rust as emit_fatal;
use emit_pos_rust::{emit_pos, emit_pos_then};
use emit_symbol_refs_rust as emit_symbol_refs;
use emit_type_constant_rust as emit_type_constant;
use env::{emitter::Emitter, local, Env, Flags as EnvFlags};
use hhas_symbol_refs_rust::IncludePath;
use hhbc_ast_rust::*;
use hhbc_id_rust::{class, function, method, prop, r#const, Id};
use hhbc_string_utils_rust as string_utils;
use instruction_sequence_rust::{
    unrecoverable,
    Error::{self, Unrecoverable},
    InstrSeq, Result,
};
use itertools::{Either, Itertools};
use label_rust::Label;
use naming_special_names_rust::{
    emitter_special_functions, fb, pseudo_consts, pseudo_functions, special_functions,
    special_idents, superglobals, user_attributes,
};
use ocaml_helper::int_of_str_opt;
use options::{CompilerFlags, HhvmFlags, LangFlags, Options};
use oxidized::{
    aast, aast_defs,
    aast_visitor::{visit, visit_mut, AstParams, Node, NodeMut, Visitor, VisitorMut},
    ast as tast, ast_defs, local_id,
    pos::Pos,
};
use runtime::TypedValue;
use scope_rust::scope;

use indexmap::IndexSet;
use std::{
    collections::{BTreeMap, HashSet},
    convert::TryInto,
    iter,
    result::Result as StdResult,
    str::FromStr,
};

#[derive(Debug)]
pub struct EmitJmpResult {
    // generated instruction sequence
    pub instrs: InstrSeq,
    // does instruction sequence fall through
    is_fallthrough: bool,
    // was label associated with emit operation used
    is_label_used: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LValOp {
    Set,
    SetOp(EqOp),
    IncDec(IncdecOp),
    Unset,
}

impl LValOp {
    fn is_incdec(&self) -> bool {
        if let Self::IncDec(_) = self {
            return true;
        };
        false
    }
}

pub fn is_local_this(env: &Env, lid: &local_id::LocalId) -> bool {
    local_id::get_name(lid) == special_idents::THIS
        && Scope::has_this(&env.scope)
        && !Scope::is_toplevel(&env.scope)
}

mod inout_locals {
    use crate::*;
    use oxidized::{aast_defs::Lid, aast_visitor, aast_visitor::Node, ast as tast, ast_defs};
    use std::{collections::HashMap, marker::PhantomData};

    pub(super) struct AliasInfo {
        first_inout: usize,
        last_write: usize,
        num_uses: usize,
    }

    impl Default for AliasInfo {
        fn default() -> Self {
            AliasInfo {
                first_inout: std::usize::MAX,
                last_write: std::usize::MIN,
                num_uses: 0,
            }
        }
    }

    impl AliasInfo {
        pub(super) fn add_inout(&mut self, i: usize) {
            if i < self.first_inout {
                self.first_inout = i;
            }
        }

        pub(super) fn add_write(&mut self, i: usize) {
            if i > self.last_write {
                self.last_write = i;
            }
        }

        pub(super) fn add_use(&mut self) {
            self.num_uses += 1
        }

        pub(super) fn in_range(&self, i: usize) -> bool {
            i > self.first_inout || i <= self.last_write
        }

        pub(super) fn has_single_ref(&self) -> bool {
            self.num_uses < 2
        }
    }

    pub(super) type AliasInfoMap = HashMap<String, AliasInfo>;

    fn add_write(name: String, i: usize, map: &mut AliasInfoMap) {
        map.entry(name).or_default().add_write(i);
    }

    fn add_inout(name: String, i: usize, map: &mut AliasInfoMap) {
        map.entry(name).or_default().add_inout(i);
    }

    fn add_use(name: String, map: &mut AliasInfoMap) {
        map.entry(name).or_default().add_use();
    }

    // determines if value of a local 'name' that appear in parameter 'i'
    // should be saved to local because it might be overwritten later
    pub(super) fn should_save_local_value(name: &str, i: usize, aliases: &AliasInfoMap) -> bool {
        aliases.get(name).map_or(false, |alias| alias.in_range(i))
    }

    pub(super) fn should_move_local_value(local: &local::Type, aliases: &AliasInfoMap) -> bool {
        match local {
            local::Type::Named(name) => aliases
                .get(&**name)
                .map_or(true, |alias| alias.has_single_ref()),
            local::Type::Unnamed(_) => false,
        }
    }

    pub(super) fn collect_written_variables(env: &Env, args: &[tast::Expr]) -> AliasInfoMap {
        let mut acc = HashMap::new();
        args.iter()
            .enumerate()
            .for_each(|(i, arg)| handle_arg(env, true, i, arg, &mut acc));
        acc
    }

    fn handle_arg(env: &Env, is_top: bool, i: usize, arg: &tast::Expr, acc: &mut AliasInfoMap) {
        use tast::{Expr, Expr_};

        let Expr(_, e) = arg;
        match e {
            Expr_::Callconv(x) => {
                if let (ast_defs::ParamKind::Pinout, Expr(_, Expr_::Lvar(lid))) = &**x {
                    let Lid(_, lid) = &**lid;
                    if !is_local_this(env, &lid) {
                        add_use(lid.1.to_string(), acc);
                        if is_top {
                            add_inout(lid.1.to_string(), i, acc);
                        } else {
                            add_write(lid.1.to_string(), i, acc);
                        }
                    }
                }
            }
            Expr_::Lvar(lid) => {
                let Lid(_, (_, id)) = &**lid;
                add_use(id.to_string(), acc);
            }
            _ => {
                // dive into argument value
                aast_visitor::visit(
                    &mut Visitor {
                        phantom: PhantomData,
                    },
                    &mut Ctx { state: acc, env, i },
                    arg,
                )
                .unwrap();
            }
        }
    }

    struct Visitor<'a> {
        phantom: PhantomData<&'a str>,
    }

    pub struct Ctx<'a> {
        state: &'a mut AliasInfoMap,
        env: &'a Env<'a>,
        i: usize,
    }

    impl<'a> aast_visitor::Visitor for Visitor<'a> {
        type P = aast_visitor::AstParams<Ctx<'a>, ()>;

        fn object(&mut self) -> &mut dyn aast_visitor::Visitor<P = Self::P> {
            self
        }

        fn visit_expr_(&mut self, c: &mut Ctx<'a>, p: &tast::Expr_) -> std::result::Result<(), ()> {
            p.recurse(c, self.object())?;
            Ok(match p {
                tast::Expr_::Binop(expr) => {
                    let (bop, left, _) = &**expr;
                    if let ast_defs::Bop::Eq(_) = bop {
                        collect_lvars_hs(c, left)
                    }
                }
                tast::Expr_::Unop(expr) => {
                    let (uop, e) = &**expr;
                    match uop {
                        ast_defs::Uop::Uincr | ast_defs::Uop::Udecr => collect_lvars_hs(c, e),
                        _ => (),
                    }
                }
                tast::Expr_::Lvar(expr) => {
                    let Lid(_, (_, id)) = &**expr;
                    add_use(id.to_string(), &mut c.state);
                }
                tast::Expr_::Call(expr) => {
                    let (_, _, _, args, uarg) = &**expr;
                    args.iter()
                        .for_each(|arg| handle_arg(&c.env, false, c.i, arg, &mut c.state));
                    uarg.as_ref()
                        .map(|arg| handle_arg(&c.env, false, c.i, arg, &mut c.state));
                }
                _ => (),
            })
        }
    }

    // collect lvars on the left hand side of '=' operator
    fn collect_lvars_hs(ctx: &mut Ctx, expr: &tast::Expr) {
        let tast::Expr(_, e) = expr;
        match &*e {
            tast::Expr_::Lvar(lid) => {
                let Lid(_, lid) = &**lid;
                if !is_local_this(&ctx.env, &lid) {
                    add_use(lid.1.to_string(), &mut ctx.state);
                    add_write(lid.1.to_string(), ctx.i, &mut ctx.state);
                }
            }
            tast::Expr_::List(exprs) => exprs.iter().for_each(|expr| collect_lvars_hs(ctx, expr)),
            _ => (),
        }
    }
}

pub fn get_type_structure_for_hint(
    e: &mut Emitter,
    tparams: &[&str],
    targ_map: &IndexSet<String>,
    hint: &aast::Hint,
) -> Result<InstrSeq> {
    let targ_map: BTreeMap<&str, i64> = targ_map
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i as i64))
        .collect();
    let tv = emit_type_constant::hint_to_type_constant(
        e.options(),
        tparams,
        &targ_map,
        &hint,
        false,
        false,
    )?;
    let i = emit_adata::get_array_identifier(e, &tv);
    Ok(if hack_arr_dv_arrs(e.options()) {
        InstrSeq::make_lit_const(InstructLitConst::Dict(i))
    } else {
        InstrSeq::make_lit_const(InstructLitConst::Array(i))
    })
}

pub struct Setrange {
    pub op: SetrangeOp,
    pub size: usize,
    pub vec: bool,
}

/// kind of value stored in local
#[derive(Debug, Clone, Copy)]
pub enum StoredValueKind {
    Local,
    Expr,
}

/// represents sequence of instructions interleaved with temp locals.
///    <(i, None) :: rest> - is emitted i :: <rest> (commonly used for final instructions in sequence)
///    <(i, Some(l, local_kind)) :: rest> is emitted as
///
///    i
///    .try {
///      setl/popl l; depending on local_kind
///      <rest>
///    } .catch {
///      unset l
///      throw
///    }
///    unsetl l
type InstrSeqWithLocals = Vec<(InstrSeq, Option<(local::Type, StoredValueKind)>)>;

/// result of emit_array_get
enum ArrayGetInstr {
    /// regular $a[..] that does not need to spill anything
    Regular(InstrSeq),
    /// subscript expression used as inout argument that need to spill intermediate values:
    Inout {
        /// instruction sequence with locals to load value
        load: InstrSeqWithLocals,
        /// instruction to set value back (can use locals defined in load part)
        store: InstrSeq,
    },
}

struct ArrayGetBaseData<T> {
    base_instrs: T,
    cls_instrs: InstrSeq,
    setup_instrs: InstrSeq,
    base_stack_size: StackIndex,
    cls_stack_size: StackIndex,
}

/// result of emit_base
enum ArrayGetBase {
    /// regular <base> part in <base>[..] that does not need to spill anything
    Regular(ArrayGetBaseData<InstrSeq>),
    /// base of subscript expression used as inout argument that need to spill
    /// intermediate values
    Inout {
        /// instructions to load base part
        load: ArrayGetBaseData<InstrSeqWithLocals>,
        /// instruction to load base part for setting inout argument back
        store: InstrSeq,
    },
}

pub fn emit_expr(emitter: &mut Emitter, env: &Env, expression: &tast::Expr) -> Result {
    use aast_defs::Lid;
    use tast::Expr_;
    let tast::Expr(pos, expr) = expression;
    match expr {
        Expr_::Float(_)
        | Expr_::String(_)
        | Expr_::Int(_)
        | Expr_::Null
        | Expr_::False
        | Expr_::True => {
            let v = ast_constant_folder::expr_to_typed_value(emitter, &env.namespace, expression)
                .map_err(|_| unrecoverable("expr_to_typed_value failed"))?;
            Ok(emit_pos_then(pos, InstrSeq::make_typedvalue(v)))
        }
        Expr_::PrefixedString(e) => emit_expr(emitter, env, &e.1),
        Expr_::ParenthesizedExpr(e) => emit_expr(emitter, env, e),
        Expr_::Lvar(e) => {
            let Lid(pos, _) = &**e;
            Ok(InstrSeq::gather(vec![
                emit_pos(pos),
                emit_local(emitter, env, BareThisOp::Notice, e)?,
            ]))
        }
        Expr_::ClassConst(e) => emit_class_const(emitter, env, pos, &e.0, &e.1),
        Expr_::Unop(e) => emit_unop(emitter, env, pos, e),
        Expr_::Binop(e) => emit_binop(emitter, env, pos, e),
        Expr_::Pipe(e) => emit_pipe(env, e),
        Expr_::Is(is_expr) => {
            let (e, h) = &**is_expr;
            Ok(InstrSeq::gather(vec![
                emit_expr(emitter, env, e)?,
                emit_is_hint(env, pos, h)?,
            ]))
        }
        Expr_::As(e) => emit_as(env, pos, e),
        Expr_::Cast(e) => emit_cast(env, pos, e),
        Expr_::Eif(e) => emit_conditional_expr(emitter, env, pos, &e.0, &e.1, &e.2),
        Expr_::ExprList(es) => Ok(InstrSeq::gather(
            es.iter()
                .map(|e| emit_expr(emitter, env, e))
                .collect::<Result<Vec<_>>>()?,
        )),
        Expr_::ArrayGet(e) => {
            let (base_expr, opt_elem_expr) = &**e;
            match (base_expr.lvar_name(), opt_elem_expr) {
                (Some(name), Some(e)) if name == superglobals::GLOBALS => {
                    Ok(InstrSeq::gather(vec![
                        emit_expr(emitter, env, e)?,
                        emit_pos(pos),
                        InstrSeq::make_cgetg(),
                    ]))
                }
                _ => Ok(emit_array_get(
                    emitter,
                    env,
                    pos,
                    None,
                    QueryOp::CGet,
                    base_expr,
                    opt_elem_expr.as_ref(),
                    false,
                    false,
                )?
                .0),
            }
        }
        Expr_::ObjGet(e) => {
            Ok(emit_obj_get(emitter, env, pos, QueryOp::CGet, &e.0, &e.1, &e.2, false)?.0)
        }
        Expr_::Call(c) => emit_call_expr(emitter, env, pos, None, c),
        Expr_::New(e) => emit_new(emitter, env, pos, e),
        Expr_::Record(e) => emit_record(env, pos, e),
        Expr_::Array(es) => Ok(emit_pos_then(
            pos,
            emit_collection(emitter, env, expression, es, None)?,
        )),
        Expr_::Darray(e) => Ok(emit_pos_then(
            pos,
            emit_collection(emitter, env, expression, &mk_afkvalues(&e.1), None)?,
        )),
        Expr_::Varray(e) => Ok(emit_pos_then(
            pos,
            emit_collection(emitter, env, expression, &mk_afvalues(&e.1), None)?,
        )),
        Expr_::Collection(e) => emit_named_collection_str(emitter, env, expression, e),
        Expr_::ValCollection(e) => {
            let (kind, _, es) = &**e;
            let fields = mk_afvalues(es);
            let collection_typ = match kind {
                aast_defs::VcKind::Vector => CollectionType::Vector,
                aast_defs::VcKind::ImmVector => CollectionType::ImmVector,
                aast_defs::VcKind::Set => CollectionType::Set,
                aast_defs::VcKind::ImmSet => CollectionType::ImmSet,
                _ => return emit_collection(emitter, env, expression, &fields, None),
            };
            emit_named_collection(emitter, env, pos, expression, &fields, collection_typ)
        }
        Expr_::Pair(e) => {
            let (e1, e2) = (**e).to_owned();
            let fields = mk_afvalues(&vec![e1, e2]);
            emit_named_collection(emitter, env, pos, expression, &fields, CollectionType::Pair)
        }
        Expr_::KeyValCollection(e) => {
            let (kind, _, fields) = &**e;
            let fields = mk_afkvalues(
                &fields
                    .to_owned()
                    .into_iter()
                    .map(|tast::Field(e1, e2)| (e1, e2))
                    .collect(),
            );
            let collection_typ = match kind {
                aast_defs::KvcKind::Map => CollectionType::Map,
                aast_defs::KvcKind::ImmMap => CollectionType::ImmMap,
                _ => return emit_collection(emitter, env, expression, &fields, None),
            };
            emit_named_collection(emitter, env, pos, expression, &fields, collection_typ)
        }
        Expr_::Clone(e) => Ok(emit_pos_then(pos, emit_clone(emitter, env, e)?)),
        Expr_::Shape(e) => Ok(emit_pos_then(pos, emit_shape(env, expression, e)?)),
        Expr_::Await(e) => emit_await(emitter, env, pos, e),
        Expr_::Yield(e) => emit_yield(emitter, env, pos, e),
        Expr_::Efun(e) => Ok(emit_pos_then(pos, emit_lambda(emitter, env, &e.0, &e.1)?)),
        Expr_::ClassGet(e) => emit_class_get(env, QueryOp::CGet, e),
        Expr_::String2(es) => emit_string2(emitter, env, pos, es),
        Expr_::BracedExpr(e) => emit_expr(emitter, env, e),
        Expr_::Id(e) => Ok(emit_pos_then(pos, emit_id(emitter, env, e)?)),
        Expr_::Xml(e) => emit_xhp(env, pos, e),
        Expr_::Callconv(e) => Err(Unrecoverable(
            "emit_callconv: This should have been caught at emit_arg".into(),
        )),
        Expr_::Import(e) => emit_import(emitter, env, pos, &e.0, &e.1),
        Expr_::Omitted => Ok(InstrSeq::Empty),
        Expr_::YieldBreak => Err(Unrecoverable(
            "yield break should be in statement position".into(),
        )),
        Expr_::YieldFrom(_) => Err(Unrecoverable("complex yield_from expression".into())),
        Expr_::Lfun(_) => Err(Unrecoverable(
            "expected Lfun to be converted to Efun during closure conversion emit_expr".into(),
        )),
        _ => unimplemented!("TODO(hrust)"),
    }
}

fn emit_exprs(e: &mut Emitter, env: &Env, exprs: &[tast::Expr]) -> Result {
    if exprs.is_empty() {
        Ok(InstrSeq::Empty)
    } else {
        Ok(InstrSeq::gather(
            exprs
                .iter()
                .map(|expr| emit_expr(e, env, expr))
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

fn emit_id(emitter: &mut Emitter, env: &Env, id: &tast::Sid) -> Result {
    use pseudo_consts::*;
    use InstructLitConst::*;

    let ast_defs::Id(p, s) = id;
    let res = match s.as_str() {
        G__FILE__ => InstrSeq::make_lit_const(File),
        G__DIR__ => InstrSeq::make_lit_const(Dir),
        G__METHOD__ => InstrSeq::make_lit_const(Method),
        G__FUNCTION_CREDENTIAL__ => InstrSeq::make_lit_const(FuncCred),
        G__CLASS__ => InstrSeq::gather(vec![InstrSeq::make_self(), InstrSeq::make_classname()]),
        G__COMPILER_FRONTEND__ => InstrSeq::make_string("hackc"),
        G__LINE__ => InstrSeq::make_int(p.info_pos_extended().1.try_into().map_err(|_| {
            emit_fatal::raise_fatal_parse(p, "error converting end of line from usize to isize")
        })?),
        G__NAMESPACE__ => InstrSeq::make_string(env.namespace.name.as_ref().map_or("", |s| &s[..])),
        EXIT | DIE => return emit_exit(emitter, env, None),
        _ => {
            //panic!("TODO: uncomment after D19350786 lands")
            //let cid: ConstId = r#const::Type::from_ast_name(&s);
            let cid: ConstId = string_utils::strip_global_ns(&s).to_string().into();
            emit_symbol_refs::State::add_constant(emitter, cid.clone());
            return Ok(emit_pos_then(p, InstrSeq::make_lit_const(CnsE(cid))));
        }
    };
    Ok(res)
}

fn emit_exit(emitter: &mut Emitter, env: &Env, expr_opt: Option<&tast::Expr>) -> Result {
    Ok(InstrSeq::gather(vec![
        expr_opt.map_or_else(|| Ok(InstrSeq::make_int(0)), |e| emit_expr(emitter, env, e))?,
        InstrSeq::make_exit(),
    ]))
}

fn emit_xhp(
    env: &Env,
    pos: &Pos,
    (id, attributes, children): &(tast::Sid, Vec<tast::XhpAttribute>, Vec<tast::Expr>),
) -> Result {
    unimplemented!("TODO(hrust)")
}

fn emit_yield(e: &mut Emitter, env: &Env, pos: &Pos, af: &tast::Afield) -> Result {
    Ok(match af {
        tast::Afield::AFvalue(v) => InstrSeq::gather(vec![
            emit_expr(e, env, v)?,
            emit_pos(pos),
            InstrSeq::make_yield(),
        ]),
        tast::Afield::AFkvalue(k, v) => InstrSeq::gather(vec![
            emit_expr(e, env, k)?,
            emit_expr(e, env, v)?,
            emit_pos(pos),
            InstrSeq::make_yieldk(),
        ]),
    })
}

fn parse_include(e: &tast::Expr) -> IncludePath {
    fn strip_backslash(s: &mut String) {
        if s.starts_with("/") {
            *s = s[1..].into()
        }
    }
    fn split_var_lit(e: &tast::Expr) -> (String, String) {
        match &e.1 {
            tast::Expr_::Binop(x) if x.0.is_dot() => {
                let (v, l) = split_var_lit(&x.2);
                if v.is_empty() {
                    let (var, lit) = split_var_lit(&x.1);
                    (var, format!("{}{}", lit, l))
                } else {
                    (v, String::new())
                }
            }
            tast::Expr_::String(lit) => (String::new(), lit.to_string()),
            _ => (text_of_expr(e), String::new()),
        }
    };
    let (mut var, mut lit) = split_var_lit(e);
    if var == pseudo_consts::G__DIR__ {
        var = String::new();
        strip_backslash(&mut lit);
    }
    if var.is_empty() {
        if std::path::Path::new(lit.as_str()).is_relative() {
            IncludePath::SearchPathRelative(lit)
        } else {
            IncludePath::Absolute(lit)
        }
    } else {
        strip_backslash(&mut lit);
        IncludePath::IncludeRootRelative(var, lit)
    }
}

fn text_of_expr(e: &tast::Expr) -> String {
    match &e.1 {
        tast::Expr_::String(s) => format!("\'{}\'", s),
        tast::Expr_::Id(id) => id.1.to_string(),
        tast::Expr_::Lvar(lid) => local_id::get_name(&lid.1).to_string(),
        tast::Expr_::ArrayGet(x) => match ((x.0).1.as_lvar(), x.1.as_ref()) {
            (Some(tast::Lid(_, id)), Some(e_)) => {
                format!("{}[{}]", local_id::get_name(&id), text_of_expr(e_))
            }
            _ => "unknown".into(),
        },
        _ => "unknown".into(),
    }
}

fn text_of_class_id(cid: &tast::ClassId) -> String {
    match &cid.1 {
        tast::ClassId_::CIparent => "parent".into(),
        tast::ClassId_::CIself => "self".into(),
        tast::ClassId_::CIstatic => "static".into(),
        tast::ClassId_::CIexpr(e) => text_of_expr(e),
        tast::ClassId_::CI(ast_defs::Id(_, id)) => id.into(),
    }
}

fn text_of_prop(prop: &tast::ClassGetExpr) -> String {
    match prop {
        tast::ClassGetExpr::CGstring((_, s)) => s.into(),
        tast::ClassGetExpr::CGexpr(e) => text_of_expr(e),
    }
}

fn emit_import(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    flavor: &tast::ImportFlavor,
    expr: &tast::Expr,
) -> Result {
    use tast::ImportFlavor;
    let inc = parse_include(expr);
    emit_symbol_refs::State::add_include(e, inc.clone());
    let (expr_instrs, import_op_instr) = match flavor {
        ImportFlavor::Include => (emit_expr(e, env, expr)?, InstrSeq::make_incl()),
        ImportFlavor::Require => (emit_expr(e, env, expr)?, InstrSeq::make_req()),
        ImportFlavor::IncludeOnce => (emit_expr(e, env, expr)?, InstrSeq::make_inclonce()),
        ImportFlavor::RequireOnce => {
            match inc.into_doc_root_relative(e.options().hhvm.include_roots.get()) {
                IncludePath::DocRootRelative(path) => {
                    let expr = tast::Expr(pos.clone(), tast::Expr_::String(path.to_owned()));
                    (emit_expr(e, env, &expr)?, InstrSeq::make_reqdoc())
                }
                _ => (emit_expr(e, env, expr)?, InstrSeq::make_reqonce()),
            }
        }
    };
    Ok(InstrSeq::gather(vec![
        expr_instrs,
        emit_pos(pos),
        import_op_instr,
    ]))
}

fn emit_string2(e: &mut Emitter, env: &Env, pos: &Pos, es: &Vec<tast::Expr>) -> Result {
    if es.is_empty() {
        Err(unrecoverable("String2 with zero araguments is impossible"))
    } else if es.len() == 1 {
        Ok(InstrSeq::gather(vec![
            emit_expr(e, env, &es[0])?,
            emit_pos(pos),
            InstrSeq::make_cast_string(),
        ]))
    } else {
        Ok(InstrSeq::gather(vec![
            emit_two_exprs(e, env, &es[0].0, &es[0], &es[1])?,
            emit_pos(pos),
            InstrSeq::make_concat(),
            InstrSeq::gather(
                (&es[2..])
                    .iter()
                    .map(|expr| {
                        Ok(InstrSeq::gather(vec![
                            emit_expr(e, env, expr)?,
                            emit_pos(pos),
                            InstrSeq::make_concat(),
                        ]))
                    })
                    .collect::<Result<_>>()?,
            ),
        ]))
    }
}

fn emit_clone(e: &mut Emitter, env: &Env, expr: &tast::Expr) -> Result {
    Ok(InstrSeq::gather(vec![
        emit_expr(e, env, expr)?,
        InstrSeq::make_clone(),
    ]))
}

fn emit_lambda(e: &mut Emitter, env: &Env, fndef: &tast::Fun_, ids: &[aast_defs::Lid]) -> Result {
    use global_state::LazyState;
    // Closure conversion puts the class number used for CreateCl in the "name"
    // of the function definition
    let fndef_name = &(fndef.name).1;
    let cls_num = fndef_name
        .parse::<isize>()
        .map_err(|err| Unrecoverable(err.to_string()))?;
    let explicit_use = e.emit_state().explicit_use_set.contains(fndef_name);
    let is_in_lambda = env.scope.is_in_lambda();
    Ok(InstrSeq::gather(vec![
        InstrSeq::gather(
            ids.iter()
                .map(|tast::Lid(pos, id)| {
                    match string_utils::reified::is_captured_generic(local_id::get_name(id)) {
                        Some((is_fun, i)) => {
                            if is_in_lambda {
                                Ok(InstrSeq::make_cgetl(local::Type::Named(
                                    string_utils::reified::reified_generic_captured_name(
                                        is_fun, i as usize,
                                    ),
                                )))
                            } else {
                                emit_reified_generic_instrs(
                                    e,
                                    &Pos::make_none(),
                                    is_fun,
                                    i as usize,
                                )
                            }
                        }
                        None => Ok({
                            let lid = get_local(e, env, pos, local_id::get_name(id))?;
                            if explicit_use {
                                InstrSeq::make_cgetl(lid)
                            } else {
                                InstrSeq::make_cugetl(lid)
                            }
                        }),
                    }
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        InstrSeq::make_createcl(ids.len(), cls_num),
    ]))
}

pub fn emit_await(emitter: &mut Emitter, env: &Env, pos: &Pos, expr: &tast::Expr) -> Result {
    let tast::Expr(_, e) = expr;
    let cant_inline_gen_functions = !emitter
        .options()
        .hhvm
        .flags
        .contains(HhvmFlags::JIT_ENABLE_RENAME_FUNCTION);
    match e.as_call() {
        Some((_, tast::Expr(_, tast::Expr_::Id(id)), _, args, None))
            if (cant_inline_gen_functions
                && args.len() == 1
                && string_utils::strip_global_ns(&(*id.1)) == "gena") =>
        {
            return inline_gena_call(emitter, env, &args[0])
        }
        _ => {
            let after_await = emitter.label_gen_mut().next_regular();
            let instrs = match e {
                tast::Expr_::Call(c) => {
                    emit_call_expr(emitter, env, pos, Some(after_await.clone()), &*c)?
                }
                _ => emit_expr(emitter, env, expr)?,
            };
            Ok(InstrSeq::gather(vec![
                instrs,
                emit_pos(pos),
                InstrSeq::make_dup(),
                InstrSeq::make_istypec(IstypeOp::OpNull),
                InstrSeq::make_jmpnz(after_await.clone()),
                InstrSeq::make_await(),
                InstrSeq::make_label(after_await),
            ]))
        }
    }
}

fn hack_arr_dv_arrs(opts: &Options) -> bool {
    opts.hhvm.flags.contains(HhvmFlags::HACK_ARR_DV_ARRS)
}

fn inline_gena_call(emitter: &mut Emitter, env: &Env, arg: &tast::Expr) -> Result {
    let load_arr = emit_expr(emitter, env, arg)?;
    let async_eager_label = emitter.label_gen_mut().next_regular();
    let hack_arr_dv_arrs = hack_arr_dv_arrs(emitter.options());

    scope::with_unnamed_local(emitter, |e, arr_local| {
        let before = InstrSeq::gather(vec![
            load_arr,
            if hack_arr_dv_arrs {
                InstrSeq::make_cast_dict()
            } else {
                InstrSeq::make_cast_darray()
            },
            InstrSeq::make_popl(arr_local.clone()),
        ]);

        let inner = InstrSeq::gather(vec![
            InstrSeq::make_nulluninit(),
            InstrSeq::make_nulluninit(),
            InstrSeq::make_nulluninit(),
            InstrSeq::make_cgetl(arr_local.clone()),
            InstrSeq::make_fcallclsmethodd(
                FcallArgs::new(
                    FcallFlags::default(),
                    1,
                    vec![],
                    Some(async_eager_label.clone()),
                    1,
                    None,
                ),
                method::from_raw_string(if hack_arr_dv_arrs {
                    "fromDict"
                } else {
                    "fromDArray"
                }),
                class::from_raw_string("HH\\AwaitAllWaitHandle"),
            ),
            InstrSeq::make_await(),
            InstrSeq::make_label(async_eager_label.clone()),
            InstrSeq::make_popc(),
            emit_iter(
                e,
                InstrSeq::make_cgetl(arr_local.clone()),
                |val_local, key_local| {
                    InstrSeq::gather(vec![
                        InstrSeq::make_cgetl(val_local),
                        InstrSeq::make_whresult(),
                        InstrSeq::make_basel(arr_local.clone(), MemberOpMode::Define),
                        InstrSeq::make_setm(0, MemberKey::EL(key_local)),
                        InstrSeq::make_popc(),
                    ])
                },
            )?,
        ]);

        let after = InstrSeq::make_pushl(arr_local);

        Ok((before, inner, after))
    })
}

fn emit_iter<F: FnOnce(local::Type, local::Type) -> InstrSeq>(
    e: &mut Emitter,
    collection: InstrSeq,
    f: F,
) -> Result {
    scope::with_unnamed_locals_and_iterators(e, |e| {
        let iter_id = e.iterator_mut().get();
        let val_id = e.local_gen_mut().get_unnamed();
        let key_id = e.local_gen_mut().get_unnamed();
        let loop_end = e.label_gen_mut().next_regular();
        let loop_next = e.label_gen_mut().next_regular();
        let iter_args = IterArgs {
            iter_id,
            key_id: Some(key_id.clone()),
            val_id: val_id.clone(),
        };
        let iter_init = InstrSeq::gather(vec![
            collection,
            InstrSeq::make_iterinit(iter_args.clone(), loop_end.clone()),
        ]);
        let iterate = InstrSeq::gather(vec![
            InstrSeq::make_label(loop_next.clone()),
            f(val_id.clone(), key_id.clone()),
            InstrSeq::make_iternext(iter_args, loop_next),
        ]);
        let iter_done = InstrSeq::gather(vec![
            InstrSeq::make_unsetl(val_id),
            InstrSeq::make_unsetl(key_id),
            InstrSeq::make_label(loop_end),
        ]);
        Ok((iter_init, iterate, iter_done))
    })
}

fn emit_shape(
    env: &Env,
    expr: &tast::Expr,
    fl: &Vec<(ast_defs::ShapeFieldName, tast::Expr)>,
) -> Result {
    unimplemented!("TODO(hrust)")
}

fn emit_vec_collection(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    fields: &Vec<tast::Afield>,
) -> Result {
    match ast_constant_folder::vec_to_typed_value(e, &env.namespace, pos, fields) {
        Ok(tv) => emit_static_collection(e, env, None, pos, tv),
        Err(_) => emit_value_only_collection(e, env, pos, fields, InstructLitConst::NewVecArray),
    }
}

fn emit_named_collection(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    expr: &tast::Expr,
    fields: &Vec<tast::Afield>,
    collection_type: CollectionType,
) -> Result {
    let emit_vector_like = |e: &mut Emitter, collection_type| {
        Ok(if fields.is_empty() {
            emit_pos_then(pos, InstrSeq::make_newcol(collection_type))
        } else {
            InstrSeq::gather(vec![
                emit_vec_collection(e, env, pos, fields)?,
                InstrSeq::make_colfromarray(collection_type),
            ])
        })
    };
    let emit_map_or_set = |e: &mut Emitter, collection_type| {
        if fields.is_empty() {
            Ok(emit_pos_then(pos, InstrSeq::make_newcol(collection_type)))
        } else {
            emit_collection(e, env, expr, fields, Some(collection_type))
        }
    };
    use CollectionType as C;
    match collection_type {
        C::Dict | C::Vec | C::Keyset => {
            let instr = emit_collection(e, env, expr, fields, None)?;
            Ok(emit_pos_then(pos, instr))
        }
        C::Vector | C::ImmVector => emit_vector_like(e, collection_type),
        C::Map | C::ImmMap | C::Set | C::ImmSet => emit_map_or_set(e, collection_type),
        C::Pair => Ok(InstrSeq::gather(vec![
            InstrSeq::gather(
                fields
                    .iter()
                    .map(|f| match f {
                        tast::Afield::AFvalue(v) => emit_expr(e, env, v),
                        _ => Err(unrecoverable("impossible Pair argument")),
                    })
                    .collect::<Result<_>>()?,
            ),
            InstrSeq::make_new_pair(),
        ])),
        _ => Err(unrecoverable("Unexpected named collection type")),
    }
}

fn emit_named_collection_str(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    (ast_defs::Id(pos, name), _, fields): &(
        tast::Sid,
        Option<tast::CollectionTarg>,
        Vec<tast::Afield>,
    ),
) -> Result {
    let name = string_utils::strip_ns(name);
    let name = string_utils::types::fix_casing(name.as_ref());
    use CollectionType::*;
    let ctype = match name {
        "dict" => Dict,
        "vec" => Vec,
        "keyset" => Keyset,
        "Vector" => Vector,
        "ImmVector" => ImmVector,
        "Map" => Map,
        "ImmMap" => ImmMap,
        "Set" => Set,
        "ImmSet" => ImmSet,
        "Pair" => Pair,
        _ => {
            return Err(unrecoverable(format!(
                "collection: {} does not exist",
                name
            )))
        }
    };
    emit_named_collection(e, env, pos, expr, fields, ctype)
}

fn mk_afkvalues(es: &Vec<(tast::Expr, tast::Expr)>) -> Vec<tast::Afield> {
    es.to_owned()
        .into_iter()
        .map(|(e1, e2)| tast::Afield::mk_afkvalue(e1, e2))
        .collect()
}

fn mk_afvalues(es: &Vec<tast::Expr>) -> Vec<tast::Afield> {
    es.to_owned()
        .into_iter()
        .map(|e| tast::Afield::mk_afvalue(e))
        .collect()
}

fn emit_collection(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    fields: &[tast::Afield],
    transform_to_collection: Option<CollectionType>,
) -> Result {
    let pos = &expr.0;
    match ast_constant_folder::expr_to_typed_value_(
        e,
        &env.namespace,
        expr,
        true, /*allow_map*/
    ) {
        Ok(tv) => emit_static_collection(e, env, transform_to_collection, pos, tv),
        Err(_) => emit_dynamic_collection(e, env, expr, fields),
    }
}

fn emit_static_collection(
    e: &mut Emitter,
    env: &Env,
    transform_to_collection: Option<CollectionType>,
    pos: &Pos,
    tv: TypedValue,
) -> Result {
    let arrprov_enabled = e.options().hhvm.flags.contains(HhvmFlags::ARRAY_PROVENANCE);
    let transform_instr = match transform_to_collection {
        Some(collection_type) => InstrSeq::make_colfromarray(collection_type),
        _ => InstrSeq::Empty,
    };
    Ok(
        if arrprov_enabled && env.scope.has_function_attribute("__ProvenanceSkipFrame") {
            InstrSeq::gather(vec![
                emit_pos(pos),
                InstrSeq::make_nulluninit(),
                InstrSeq::make_nulluninit(),
                InstrSeq::make_nulluninit(),
                InstrSeq::make_typedvalue(tv),
                InstrSeq::make_fcallfuncd(
                    FcallArgs::new(FcallFlags::default(), 0, vec![], None, 1, None),
                    function::from_raw_string("HH\\tag_provenance_here"),
                ),
                transform_instr,
            ])
        } else {
            InstrSeq::gather(vec![
                emit_pos(pos),
                InstrSeq::make_typedvalue(tv),
                transform_instr,
            ])
        },
    )
}

fn expr_and_new(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    instr_to_add_new: InstrSeq,
    instr_to_add: InstrSeq,
    field: &tast::Afield,
) -> Result {
    match field {
        tast::Afield::AFvalue(v) => Ok(InstrSeq::gather(vec![
            emit_expr(e, env, v)?,
            emit_pos(pos),
            instr_to_add_new,
        ])),
        tast::Afield::AFkvalue(k, v) => Ok(InstrSeq::gather(vec![
            emit_two_exprs(e, env, &k.0, k, v)?,
            instr_to_add,
        ])),
    }
}

fn emit_keyvalue_collection(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    fields: &[tast::Afield],
    ctype: CollectionType,
    constructor: InstructLitConst,
) -> Result {
    let (transform_instr, add_elem_instr) = match ctype {
        CollectionType::Dict | CollectionType::Array => {
            (InstrSeq::Empty, InstrSeq::make_add_new_elemc())
        }
        _ => (
            InstrSeq::make_colfromarray(ctype),
            InstrSeq::gather(vec![InstrSeq::make_dup(), InstrSeq::make_add_elemc()]),
        ),
    };
    let emitted_pos = emit_pos(pos);
    Ok(InstrSeq::gather(vec![
        emitted_pos.clone(),
        InstrSeq::make_lit_const(constructor),
        InstrSeq::gather(
            fields
                .iter()
                .map(|f| {
                    expr_and_new(
                        e,
                        env,
                        pos,
                        add_elem_instr.clone(),
                        InstrSeq::make_add_elemc(),
                        f,
                    )
                })
                .collect::<Result<_>>()?,
        ),
        emitted_pos,
        transform_instr,
    ]))
}

fn is_struct_init(
    e: &mut Emitter,
    env: &Env,
    fields: &[tast::Afield],
    allow_numerics: bool,
) -> bool {
    let mut are_all_keys_non_numeric_strings = true;
    let mut has_duplicate_keys = false;
    let mut uniq_keys = std::collections::HashSet::<String>::new();
    for f in fields.iter() {
        if let tast::Afield::AFkvalue(key, _) = f {
            // TODO(hrust): if key is String, don't clone and call fold_expr
            let mut key = key.clone();
            ast_constant_folder::fold_expr(&mut key, e, &env.namespace);
            if let tast::Expr(_, tast::Expr_::String(s)) = key {
                are_all_keys_non_numeric_strings = are_all_keys_non_numeric_strings
                    && !i64::from_str(&s).is_ok()
                    && !f64::from_str(&s).is_ok();
                has_duplicate_keys = has_duplicate_keys || !uniq_keys.insert(s);
            }
            if !are_all_keys_non_numeric_strings && has_duplicate_keys {
                break;
            }
        }
    }
    let num_keys = fields.len();
    let limit = *(e.options().max_array_elem_size_on_the_stack.get()) as usize;
    (allow_numerics || are_all_keys_non_numeric_strings)
        && !has_duplicate_keys
        && num_keys <= limit
        && num_keys != 0
}

fn emit_struct_array<C: FnOnce(&mut Emitter, Vec<String>) -> Result<InstrSeq>>(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    fields: &[tast::Afield],
    ctor: C,
) -> Result {
    use tast::{Expr as E, Expr_ as E_};
    let (keys, value_instrs) = fields
        .iter()
        .map(|f| match f {
            tast::Afield::AFkvalue(k, v) => match k {
                E(_, E_::String(s)) => Ok((s.clone(), emit_expr(e, env, v)?)),
                _ => {
                    let mut k = k.clone();
                    ast_constant_folder::fold_expr(&mut k, e, &env.namespace);
                    match k {
                        E(_, E_::String(s)) => Ok((s.clone(), emit_expr(e, env, v)?)),
                        _ => Err(unrecoverable("Key must be a string")),
                    }
                }
            },
            _ => Err(unrecoverable("impossible")),
        })
        .collect::<Result<Vec<(String, InstrSeq)>>>()?
        .into_iter()
        .unzip();
    Ok(InstrSeq::gather(vec![
        InstrSeq::gather(value_instrs),
        emit_pos(pos),
        ctor(e, keys)?,
    ]))
}

fn emit_dynamic_collection(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    fields: &[tast::Afield],
) -> Result {
    let pos = &expr.0;
    let count = fields.len();
    let emit_dict = |e: &mut Emitter| {
        if is_struct_init(e, env, fields, true) {
            emit_struct_array(e, env, pos, fields, |_, x| {
                Ok(InstrSeq::make_newstructdict(x))
            })
        } else {
            let ctor = InstructLitConst::NewDictArray(count as isize);
            emit_keyvalue_collection(e, env, pos, fields, CollectionType::Dict, ctor)
        }
    };
    let emit_collection_helper = |e: &mut Emitter, ctype| {
        if is_struct_init(e, env, fields, true) {
            Ok(InstrSeq::gather(vec![
                emit_struct_array(e, env, pos, fields, |_, x| {
                    Ok(InstrSeq::make_newstructdict(x))
                })?,
                emit_pos(pos),
                InstrSeq::make_colfromarray(ctype),
            ]))
        } else {
            let ctor = InstructLitConst::NewDictArray(count as isize);
            emit_keyvalue_collection(e, env, pos, fields, ctype, ctor)
        }
    };
    use tast::Expr_ as E_;
    match &expr.1 {
        E_::ValCollection(v) if v.0 == tast::VcKind::Vec => {
            emit_value_only_collection(e, env, pos, fields, InstructLitConst::NewVecArray)
        }
        E_::Collection(v) if (v.0).1 == "vec" => {
            emit_value_only_collection(e, env, pos, fields, InstructLitConst::NewVecArray)
        }
        E_::ValCollection(v) if v.0 == tast::VcKind::Keyset => {
            emit_value_only_collection(e, env, pos, fields, InstructLitConst::NewKeysetArray)
        }
        E_::Collection(v) if (v.0).1 == "keyset" => {
            emit_value_only_collection(e, env, pos, fields, InstructLitConst::NewKeysetArray)
        }
        E_::Collection(v) if (v.0).1 == "dict" => emit_dict(e),
        E_::KeyValCollection(v) if v.0 == tast::KvcKind::Dict => emit_dict(e),
        E_::Collection(v) if string_utils::strip_ns(&(v.0).1) == "Set" => {
            emit_collection_helper(e, CollectionType::Set)
        }
        E_::ValCollection(v) if v.0 == tast::VcKind::Set => {
            emit_collection_helper(e, CollectionType::Set)
        }
        E_::Collection(v) if string_utils::strip_ns(&(v.0).1) == "ImmSet" => {
            emit_collection_helper(e, CollectionType::ImmSet)
        }
        E_::ValCollection(v) if v.0 == tast::VcKind::ImmSet => {
            emit_collection_helper(e, CollectionType::ImmSet)
        }
        E_::Collection(v) if string_utils::strip_ns(&(v.0).1) == "Map" => {
            emit_collection_helper(e, CollectionType::Map)
        }
        E_::KeyValCollection(v) if v.0 == tast::KvcKind::Map => {
            emit_collection_helper(e, CollectionType::Map)
        }
        E_::Collection(v) if string_utils::strip_ns(&(v.0).1) == "ImmMap" => {
            emit_collection_helper(e, CollectionType::ImmMap)
        }
        E_::KeyValCollection(v) if v.0 == tast::KvcKind::ImmMap => {
            emit_collection_helper(e, CollectionType::ImmMap)
        }
        E_::Varray(_) => {
            let hack_arr_dv_arrs = hack_arr_dv_arrs(e.options());
            emit_value_only_collection(e, env, pos, fields, |n| {
                if hack_arr_dv_arrs {
                    InstructLitConst::NewVecArray(n)
                } else {
                    InstructLitConst::NewVArray(n)
                }
            })
        }
        E_::Darray(_) => {
            if is_struct_init(e, env, fields, false /* allow_numerics */) {
                let hack_arr_dv_arrs = hack_arr_dv_arrs(e.options());
                emit_struct_array(e, env, pos, fields, |e, arg| {
                    let instr = if hack_arr_dv_arrs {
                        InstrSeq::make_newstructdict(arg)
                    } else {
                        InstrSeq::make_newstructdarray(arg)
                    };
                    Ok(emit_pos_then(pos, instr))
                })
            } else {
                let constr = if hack_arr_dv_arrs(e.options()) {
                    InstructLitConst::NewDictArray(count as isize)
                } else {
                    InstructLitConst::NewDArray(count as isize)
                };
                emit_keyvalue_collection(e, env, pos, fields, CollectionType::Array, constr)
            }
        }
        _ => {
            if is_packed_init(e.options(), fields, true /* hack_arr_compat */) {
                emit_value_only_collection(e, env, pos, fields, InstructLitConst::NewPackedArray)
            } else if is_struct_init(e, env, fields, false /* allow_numerics */) {
                emit_struct_array(e, env, pos, fields, |_, x| {
                    Ok(InstrSeq::make_newstructarray(x))
                })
            } else if is_packed_init(e.options(), fields, false /* hack_arr_compat*/) {
                let constr = InstructLitConst::NewArray(count as isize);
                emit_keyvalue_collection(e, env, pos, fields, CollectionType::Array, constr)
            } else {
                let constr = InstructLitConst::NewMixedArray(count as isize);
                emit_keyvalue_collection(e, env, pos, fields, CollectionType::Array, constr)
            }
        }
    }
}

/// is_packed_init() returns true if this expression list looks like an
/// array with no keys and no ref values
fn is_packed_init(opts: &Options, es: &[tast::Afield], hack_arr_compat: bool) -> bool {
    let is_only_values = es.iter().all(|f| !f.is_afkvalue());
    let has_bool_keys = es.iter().any(|f| {
        f.as_afkvalue()
            .map(|(tast::Expr(_, k), _)| k.is_true() || k.is_false())
            .is_some()
    });
    let keys_are_zero_indexed_properly_formed = es.iter().enumerate().all(|(i, f)| {
        use tast::{Afield as A, Expr as E, Expr_ as E_};
        match f {
            A::AFkvalue(E(_, E_::Int(k)), _) => int_of_str_opt(k).unwrap() == i as i64, // already checked in lowerer
            // arrays with int-like string keys are still considered packed
            // and should be emitted via NewArray
            A::AFkvalue(E(_, E_::String(s)), _) => {
                int_of_str_opt(s).map_or(false, |s| s == i as i64)
            }
            A::AFkvalue(E(_, E_::True), _) => i == 1,
            A::AFkvalue(E(_, E_::False), _) => i == 0,
            A::AFvalue(_) => true,
            _ => false,
        }
    });
    (is_only_values || keys_are_zero_indexed_properly_formed)
        && (!(has_bool_keys
            && hack_arr_compat
            && opts.hhvm.flags.contains(HhvmFlags::HACK_ARR_COMPAT_NOTICES)))
        && !es.is_empty()
}

fn emit_value_only_collection<F: FnOnce(isize) -> InstructLitConst>(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    fields: &[tast::Afield],
    constructor: F,
) -> Result {
    let limit = *(e.options().max_array_elem_size_on_the_stack.get()) as usize;
    let inline = |e: &mut Emitter, exprs: &[tast::Afield]| -> Result {
        Ok(InstrSeq::gather(vec![
            InstrSeq::gather(
                exprs
                    .iter()
                    .map(|f| emit_expr(e, env, f.value()))
                    .collect::<Result<_>>()?,
            ),
            emit_pos(pos),
            InstrSeq::make_lit_const(constructor(exprs.len() as isize)),
        ]))
    };
    let outofline = |e: &mut Emitter, exprs: &[tast::Afield]| -> Result {
        Ok(InstrSeq::gather(
            exprs
                .iter()
                .map(|f| {
                    Ok(InstrSeq::gather(vec![
                        emit_expr(e, env, f.value())?,
                        InstrSeq::make_add_new_elemc(),
                    ]))
                })
                .collect::<Result<_>>()?,
        ))
    };
    let (x1, fields) = fields.split_at(std::cmp::min(fields.len(), limit));
    let (x2, _) = fields.split_at(std::cmp::min(fields.len(), limit));
    Ok(match (x1, x2) {
        ([], []) => InstrSeq::Empty,
        (_, []) => inline(e, x1)?,
        _ => InstrSeq::gather(vec![inline(e, x1)?, outofline(e, x2)?]),
    })
}

fn emit_record(
    env: &Env,
    pos: &Pos,
    (cid, is_array, es): &(tast::Sid, bool, Vec<(tast::Expr, tast::Expr)>),
) -> Result {
    let es = mk_afkvalues(es);
    unimplemented!("TODO(hrust)")
}

fn emit_call_isset_exprs(e: &mut Emitter, env: &Env, pos: &Pos, exprs: &[tast::Expr]) -> Result {
    unimplemented!()
}

fn emit_idx(e: &mut Emitter, env: &Env, pos: &Pos, es: &[tast::Expr]) -> Result {
    let default = if es.len() == 2 {
        InstrSeq::make_null()
    } else {
        InstrSeq::Empty
    };
    Ok(InstrSeq::gather(vec![
        emit_exprs(e, env, es)?,
        emit_pos(pos),
        default,
        InstrSeq::make_idx(),
    ]))
}

fn emit_call(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    expr: &tast::Expr,
    targs: &[tast::Targ],
    args: &[tast::Expr],
    uarg: Option<&tast::Expr>,
    async_eager_label: Option<Label>,
) -> Result {
    if let Some(ast_defs::Id(_, s)) = expr.as_id() {
        let fid = function::Type::from_ast_name(s);
        emit_symbol_refs::add_function(e, fid);
    }
    let fcall_args = get_fcall_args(args, uarg, async_eager_label, None, false);
    let FcallArgs(_, _, num_ret, _, _, _) = &fcall_args;
    let num_uninit = num_ret - 1;
    let default = scope::with_unnamed_locals(e, |e| {
        let (lhs, fcall) = emit_call_lhs_and_fcall(e, env, expr, fcall_args, targs)?;
        let (args, inout_setters) = emit_args_inout_setters(e, env, args)?;
        let uargs = uarg.map_or(Ok(InstrSeq::Empty), |uarg| emit_expr(e, env, uarg))?;
        Ok((
            InstrSeq::Empty,
            InstrSeq::gather(vec![
                InstrSeq::gather(
                    iter::repeat(InstrSeq::make_nulluninit())
                        .take(num_uninit)
                        .collect::<Vec<_>>(),
                ),
                lhs,
                args,
                uargs,
                emit_pos(pos),
                fcall,
                inout_setters,
            ]),
            InstrSeq::Empty,
        ))
    })?;
    expr.1
        .as_id()
        .and_then(|ast_defs::Id(_, id)| {
            emit_special_function(e, env, pos, &expr.0, &id, args, uarg, &default).transpose()
        })
        .unwrap_or(Ok(default))
}

fn emit_reified_targs(e: &mut Emitter, env: &Env, pos: &Pos, targs: &[&tast::Hint]) -> Result {
    unimplemented!()
}

fn get_erased_tparams<'a>(env: &'a Env<'a>) -> Vec<&'a str> {
    env.scope
        .get_tparams()
        .iter()
        .filter_map(|tparam| match tparam.reified {
            tast::ReifyKind::Erased => Some(tparam.name.1.as_str()),
            _ => None,
        })
        .collect()
}

fn has_non_tparam_generics_targs(env: &Env, targs: &[tast::Targ]) -> bool {
    let erased_tparams = get_erased_tparams(env);
    targs.iter().any(|targ| {
        (targ.1)
            .1
            .as_happly()
            .map_or(true, |(id, _)| !erased_tparams.contains(&id.1.as_str()))
    })
}

fn from_ast_null_flavor(nullflavor: tast::OgNullFlavor) -> ObjNullFlavor {
    match nullflavor {
        tast::OgNullFlavor::OGNullsafe => ObjNullFlavor::NullSafe,
        tast::OgNullFlavor::OGNullthrows => ObjNullFlavor::NullThrows,
    }
}

fn emit_object_expr(e: &mut Emitter, env: &Env, expr: &tast::Expr) -> Result {
    match &expr.1 {
        tast::Expr_::Lvar(x) if is_local_this(env, &x.1) => Ok(InstrSeq::make_this()),
        _ => emit_expr(e, env, expr),
    }
}

fn emit_call_lhs_and_fcall(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    mut fcall_args: FcallArgs,
    targs: &[tast::Targ],
) -> Result<(InstrSeq, InstrSeq)> {
    let tast::Expr(pos, expr_) = expr;
    use tast::{Expr as E, Expr_ as E_};

    let emit_generics = |e: &mut Emitter, env, fcall_args: &mut FcallArgs| {
        let does_not_have_non_tparam_generics = !has_non_tparam_generics_targs(env, targs);
        if does_not_have_non_tparam_generics {
            Ok(InstrSeq::Empty)
        } else {
            *(&mut fcall_args.0) = fcall_args.0 | FcallFlags::HAS_GENERICS;
            emit_reified_targs(
                e,
                env,
                pos,
                targs
                    .iter()
                    .map(|targ| &targ.1)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        }
    };

    match expr_ {
        E_::ObjGet(o) => {
            let emit_id =
                |e: &mut Emitter, obj, id, null_flavor: &tast::OgNullFlavor, mut fcall_args| {
                    // TODO(hrust): enable let name = method::Type::from_ast_name(id);
                    let name: method::Type = string_utils::strip_global_ns(id).to_string().into();
                    let obj = emit_object_expr(e, env, obj)?;
                    let generics = emit_generics(e, env, &mut fcall_args)?;
                    let null_flavor = from_ast_null_flavor(*null_flavor);
                    Ok((
                        InstrSeq::gather(vec![
                            obj,
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                        ]),
                        InstrSeq::gather(vec![
                            generics,
                            InstrSeq::make_fcallobjmethodd(fcall_args, name, null_flavor),
                        ]),
                    ))
                };
            match o.as_ref() {
                (obj, E(_, E_::String(id)), null_flavor) => {
                    emit_id(e, obj, id, null_flavor, fcall_args)
                }
                (obj, E(_, E_::Id(id)), null_flavor) => {
                    emit_id(e, obj, &id.1, null_flavor, fcall_args)
                }
                (obj, method_expr, null_flavor) => {
                    let obj = emit_object_expr(e, env, obj)?;
                    let tmp = e.local_gen_mut().get_unnamed();
                    let null_flavor = from_ast_null_flavor(*null_flavor);
                    Ok((
                        InstrSeq::gather(vec![
                            obj,
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            emit_expr(e, env, method_expr)?,
                            InstrSeq::make_popl(tmp.clone()),
                        ]),
                        InstrSeq::gather(vec![
                            InstrSeq::make_pushl(tmp),
                            InstrSeq::make_fcallobjmethod(fcall_args, null_flavor),
                        ]),
                    ))
                }
            }
        }
        E_::ClassConst(cls_const) => {
            let (cid, (_, id)) = &**cls_const;
            let mut cexpr = ClassExpr::class_id_to_class_expr(e, false, false, &env.scope, cid);
            if let ClassExpr::Id(ast_defs::Id(_, name)) = &cexpr {
                if let Some(reified_var_cexpr) = get_reified_var_cexpr(e, env, pos, &name)? {
                    cexpr = reified_var_cexpr;
                }
            }
            // TODO(hrust) enabel `let method_id = method::Type::from_ast_name(&id);`,
            // `from_ast_name` should be able to accpet Cow<str>
            let method_id: method::Type = string_utils::strip_global_ns(&id).to_string().into();
            Ok(match cexpr {
                // Statically known
                ClassExpr::Id(ast_defs::Id(_, cname)) => {
                    // TODO(hrust) enabel `let cid = class::Type::from_ast_name(&cname);`,
                    // `from_ast_name` should be able to accpet Cow<str>
                    let cid: class::Type = string_utils::strip_global_ns(&cname).to_string().into();
                    emit_symbol_refs::State::add_class(e, cid.clone());
                    let generics = emit_generics(e, env, &mut fcall_args)?;
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                        ]),
                        InstrSeq::gather(vec![
                            generics,
                            InstrSeq::make_fcallclsmethodd(fcall_args, method_id, cid),
                        ]),
                    )
                }
                ClassExpr::Special(clsref) => {
                    let generics = emit_generics(e, env, &mut fcall_args)?;
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                        ]),
                        InstrSeq::gather(vec![
                            generics,
                            InstrSeq::make_fcallclsmethodsd(fcall_args, clsref, method_id),
                        ]),
                    )
                }
                ClassExpr::Expr(expr) => {
                    let generics = emit_generics(e, env, &mut fcall_args)?;
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                        ]),
                        InstrSeq::gather(vec![
                            generics,
                            InstrSeq::make_string(method_id.to_raw_string()),
                            emit_expr(e, env, &expr)?,
                            InstrSeq::make_classgetc(),
                            InstrSeq::make_fcallclsmethod(
                                IsLogAsDynamicCallOp::DontLogAsDynamicCall,
                                fcall_args,
                            ),
                        ]),
                    )
                }
                ClassExpr::Reified(instrs) => {
                    let tmp = e.local_gen_mut().get_unnamed();
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            instrs,
                            InstrSeq::make_popl(tmp.clone()),
                        ]),
                        InstrSeq::gather(vec![
                            InstrSeq::make_string(method_id.to_raw_string()),
                            InstrSeq::make_pushl(tmp),
                            InstrSeq::make_classgetc(),
                            InstrSeq::make_fcallclsmethod(
                                IsLogAsDynamicCallOp::LogAsDynamicCall,
                                fcall_args,
                            ),
                        ]),
                    )
                }
            })
        }
        E_::ClassGet(class_get) => {
            let (cid, cls_get_expr) = &**class_get;
            let mut cexpr = ClassExpr::class_id_to_class_expr(e, false, false, &env.scope, cid);
            if let ClassExpr::Id(ast_defs::Id(_, name)) = &cexpr {
                if let Some(reified_var_cexpr) = get_reified_var_cexpr(e, env, pos, &name)? {
                    cexpr = reified_var_cexpr;
                }
            }
            let emit_meth_name = |e: &mut Emitter| match &cls_get_expr {
                tast::ClassGetExpr::CGstring((pos, id)) => Ok(emit_pos_then(
                    pos,
                    InstrSeq::make_cgetl(local::Type::Named(id.clone())),
                )),
                tast::ClassGetExpr::CGexpr(expr) => emit_expr(e, env, expr),
            };
            Ok(match cexpr {
                ClassExpr::Id(cid) => {
                    let tmp = e.local_gen_mut().get_unnamed();
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            emit_meth_name(e)?,
                            InstrSeq::make_popl(tmp.clone()),
                        ]),
                        InstrSeq::gather(vec![
                            InstrSeq::make_pushl(tmp),
                            emit_known_class_id(e, &cid),
                            InstrSeq::make_fcallclsmethod(
                                IsLogAsDynamicCallOp::LogAsDynamicCall,
                                fcall_args,
                            ),
                        ]),
                    )
                }
                ClassExpr::Special(clsref) => {
                    let tmp = e.local_gen_mut().get_unnamed();
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            emit_meth_name(e)?,
                            InstrSeq::make_popl(tmp.clone()),
                        ]),
                        InstrSeq::gather(vec![
                            InstrSeq::make_pushl(tmp),
                            InstrSeq::make_fcallclsmethods(fcall_args, clsref),
                        ]),
                    )
                }
                ClassExpr::Expr(expr) => {
                    let cls = e.local_gen_mut().get_unnamed();
                    let meth = e.local_gen_mut().get_unnamed();
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            emit_expr(e, env, &expr)?,
                            InstrSeq::make_popl(cls.clone()),
                            emit_meth_name(e)?,
                            InstrSeq::make_popl(meth.clone()),
                        ]),
                        InstrSeq::gather(vec![
                            InstrSeq::make_pushl(meth),
                            InstrSeq::make_pushl(cls),
                            InstrSeq::make_classgetc(),
                            InstrSeq::make_fcallclsmethod(
                                IsLogAsDynamicCallOp::LogAsDynamicCall,
                                fcall_args,
                            ),
                        ]),
                    )
                }
                ClassExpr::Reified(instrs) => {
                    let cls = e.local_gen_mut().get_unnamed();
                    let meth = e.local_gen_mut().get_unnamed();
                    (
                        InstrSeq::gather(vec![
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            InstrSeq::make_nulluninit(),
                            instrs,
                            InstrSeq::make_popl(cls.clone()),
                            emit_meth_name(e)?,
                            InstrSeq::make_popl(meth.clone()),
                        ]),
                        InstrSeq::gather(vec![
                            InstrSeq::make_pushl(meth),
                            InstrSeq::make_pushl(cls),
                            InstrSeq::make_classgetc(),
                            InstrSeq::make_fcallclsmethod(
                                IsLogAsDynamicCallOp::LogAsDynamicCall,
                                fcall_args,
                            ),
                        ]),
                    )
                }
            })
        }
        E_::Id(id) => {
            let FcallArgs(flags, num_args, _, _, _, _) = fcall_args;
            let fq_id = match string_utils::strip_global_ns(&id.1) {
                "min" if num_args == 2 && !flags.contains(FcallFlags::HAS_UNPACK) => {
                    function::Type::from_ast_name("__SystemLib\\min2")
                }
                "max" if num_args == 2 && !flags.contains(FcallFlags::HAS_UNPACK) => {
                    function::Type::from_ast_name("__SystemLib\\max2")
                }
                _ => {
                    //TODO(hrust): enable `function::Type::from_ast_name(&id.1)`
                    string_utils::strip_global_ns(&id.1).to_string().into()
                }
            };
            let generics = emit_generics(e, env, &mut fcall_args)?;
            Ok((
                InstrSeq::gather(vec![
                    InstrSeq::make_nulluninit(),
                    InstrSeq::make_nulluninit(),
                    InstrSeq::make_nulluninit(),
                ]),
                InstrSeq::gather(vec![generics, InstrSeq::make_fcallfuncd(fcall_args, fq_id)]),
            ))
        }
        E_::String(s) => unimplemented!(),
        _ => {
            let tmp = e.local_gen_mut().get_unnamed();
            Ok((
                InstrSeq::gather(vec![
                    InstrSeq::make_nulluninit(),
                    InstrSeq::make_nulluninit(),
                    InstrSeq::make_nulluninit(),
                    emit_expr(e, env, &expr)?,
                    InstrSeq::make_popl(tmp.clone()),
                ]),
                InstrSeq::gather(vec![
                    InstrSeq::make_pushl(tmp),
                    InstrSeq::make_fcallfunc(fcall_args),
                ]),
            ))
        }
    }
}

fn get_reified_var_cexpr(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    name: &str,
) -> Result<Option<ClassExpr>> {
    Ok(emit_reified_type_opt(e, env, pos, name)?.map(|instrs| {
        ClassExpr::Reified(InstrSeq::gather(vec![
            instrs,
            InstrSeq::make_basec(0, MemberOpMode::Warn),
            InstrSeq::make_querym(1, QueryOp::CGet, MemberKey::ET("classname".into())),
        ]))
    }))
}

fn emit_args_inout_setters(
    e: &mut Emitter,
    env: &Env,
    args: &[tast::Expr],
) -> Result<(InstrSeq, InstrSeq)> {
    let aliases = if has_inout_arg(args) {
        inout_locals::collect_written_variables(env, args)
    } else {
        inout_locals::AliasInfoMap::new()
    };
    fn emit_arg_and_inout_setter(
        e: &mut Emitter,
        env: &Env,
        i: usize,
        arg: &tast::Expr,
        aliases: &inout_locals::AliasInfoMap,
    ) -> Result<(InstrSeq, InstrSeq)> {
        use tast::Expr_ as E_;
        match &arg.1 {
            E_::Callconv(cc) if (cc.0).is_pinout() => {
                match &(cc.1).1 {
                    // inout $var
                    E_::Lvar(l) => {
                        let local = get_local(e, env, &l.0, local_id::get_name(&l.1))?;
                        let move_instrs = if !env.flags.contains(env::Flags::IN_TRY)
                            && inout_locals::should_move_local_value(&local, aliases)
                        {
                            InstrSeq::gather(vec![
                                InstrSeq::make_null(),
                                InstrSeq::make_popl(local.clone()),
                            ])
                        } else {
                            InstrSeq::Empty
                        };
                        Ok((
                            InstrSeq::gather(vec![
                                InstrSeq::make_cgetl(local.clone()),
                                move_instrs,
                            ]),
                            InstrSeq::make_popl(local),
                        ))
                    }
                    // inout $arr[...][...]
                    E_::ArrayGet(ag) => {
                        let array_get_result = emit_array_get_(
                            e,
                            env,
                            &(cc.1).0,
                            None,
                            QueryOp::InOut,
                            &ag.0,
                            ag.1.as_ref(),
                            false,
                            false,
                            Some((i, aliases)),
                        )?
                        .0;
                        Ok(match array_get_result {
                            ArrayGetInstr::Regular(instrs) => {
                                let setter_base = emit_array_get(
                                    e,
                                    env,
                                    &(cc.1).0,
                                    Some(MemberOpMode::Define),
                                    QueryOp::InOut,
                                    &ag.0,
                                    ag.1.as_ref(),
                                    true,
                                    false,
                                )?
                                .0;
                                let setter = InstrSeq::gather(vec![
                                    setter_base,
                                    InstrSeq::make_setm(
                                        0,
                                        get_elem_member_key(e, env, 0, ag.1.as_ref(), false)?,
                                    ),
                                    InstrSeq::make_popc(),
                                ]);
                                (instrs, setter)
                            }
                            ArrayGetInstr::Inout { load, store } => {
                                let (mut ld, mut st) = (vec![], vec![store]);
                                for (instr, local_kind_opt) in load.into_iter() {
                                    match local_kind_opt {
                                        None => ld.push(instr),
                                        Some((l, kind)) => {
                                            let unset = InstrSeq::make_unsetl(l.clone());
                                            let set = match kind {
                                                StoredValueKind::Expr => InstrSeq::make_setl(l),
                                                _ => InstrSeq::make_popl(l),
                                            };
                                            ld.push(instr);
                                            ld.push(set);
                                            st.push(unset);
                                        }
                                    }
                                }
                                (InstrSeq::gather(ld), InstrSeq::gather(st))
                            }
                        })
                    }
                    _ => Err(unrecoverable(
                        "emit_arg_and_inout_setter: Unexpected inout expression type",
                    )),
                }
            }
            _ => Ok((emit_expr(e, env, arg)?, InstrSeq::Empty)),
        }
    }
    let (instr_args, instr_setters): (Vec<InstrSeq>, Vec<InstrSeq>) = args
        .iter()
        .enumerate()
        .map(|(i, arg)| emit_arg_and_inout_setter(e, env, i, arg, &aliases))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .unzip();
    let instr_args = InstrSeq::gather(instr_args);
    let instr_setters = InstrSeq::gather(instr_setters);
    if has_inout_arg(args) {
        let retval = e.local_gen_mut().get_unnamed();
        Ok((
            instr_args,
            InstrSeq::gather(vec![
                InstrSeq::make_popl(retval.clone()),
                instr_setters,
                InstrSeq::make_pushl(retval),
            ]),
        ))
    } else {
        Ok((instr_args, InstrSeq::Empty))
    }
}

fn get_fcall_args(
    args: &[tast::Expr],
    uarg: Option<&tast::Expr>,
    async_eager_label: Option<Label>,
    context: Option<String>,
    lock_while_unwinding: bool,
) -> FcallArgs {
    let num_args = args.len();
    let num_rets = 1 + args.iter().filter(|x| is_inout_arg(*x)).count();
    let mut flags = FcallFlags::default();
    flags.set(FcallFlags::HAS_UNPACK, uarg.is_some());
    flags.set(FcallFlags::LOCK_WHILE_UNWINDING, lock_while_unwinding);
    let inouts: Vec<bool> = args.iter().map(is_inout_arg).collect();
    FcallArgs::new(
        flags,
        num_rets,
        inouts,
        async_eager_label,
        num_args,
        context,
    )
}

fn is_inout_arg(e: &tast::Expr) -> bool {
    e.1.as_callconv().map_or(false, |cc| cc.0.is_pinout())
}

fn has_inout_arg(es: &[tast::Expr]) -> bool {
    es.iter().any(is_inout_arg)
}

fn emit_special_function(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    annot: &Pos,
    id: &str,
    args: &[tast::Expr],
    uarg: Option<&tast::Expr>,
    default: &InstrSeq,
) -> Result<Option<InstrSeq>> {
    use tast::{Expr as E, Expr_ as E_};
    let nargs = args.len() + uarg.map_or(0, |_| 1);
    let fq = function::Type::from_ast_name(id);
    let lower_fq_name = fq.to_raw_string();
    match (lower_fq_name, args) {
        (id, _) if id == special_functions::ECHO => Ok(Some(InstrSeq::gather(
            args.iter()
                .enumerate()
                .map(|(i, arg)| {
                    Ok(InstrSeq::gather(vec![
                        emit_expr(e, env, arg)?,
                        emit_pos(pos),
                        InstrSeq::make_print(),
                        if i == nargs - 1 {
                            InstrSeq::Empty
                        } else {
                            InstrSeq::make_popc()
                        },
                    ]))
                })
                .collect::<Result<_>>()?,
        ))),
        ("HH\\invariant", args) if args.len() >= 2 => {
            let l = e.label_gen_mut().next_regular();
            let expr_id = tast::Expr(
                pos.clone(),
                tast::Expr_::mk_id(ast_defs::Id(
                    pos.clone(),
                    "\\hh\\invariant_violation".into(),
                )),
            );
            let call = tast::Expr(
                pos.clone(),
                tast::Expr_::mk_call(
                    tast::CallType::Cnormal,
                    expr_id,
                    vec![],
                    args[1..].to_owned(),
                    uarg.cloned(),
                ),
            );
            Ok(Some(InstrSeq::gather(vec![
                emit_expr(e, env, &args[0])?,
                InstrSeq::make_jmpnz(l.clone()),
                emit_ignored_expr(e, env, &Pos::make_none(), &call)?,
                emit_fatal::emit_fatal_runtime(pos, "invariant_violation"),
                InstrSeq::make_label(l),
                InstrSeq::make_null(),
            ])))
        }
        ("assert", _) => {
            let l0 = e.label_gen_mut().next_regular();
            let l1 = e.label_gen_mut().next_regular();
            Ok(Some(InstrSeq::gather(vec![
                InstrSeq::make_string("zend.assertions"),
                InstrSeq::make_fcallbuiltin(1, 1, 0, "ini_get"),
                InstrSeq::make_int(0),
                InstrSeq::make_gt(),
                InstrSeq::make_jmpz(l0.clone()),
                default.clone(),
                InstrSeq::make_jmp(l1.clone()),
                InstrSeq::make_label(l0),
                InstrSeq::make_true(),
                InstrSeq::make_label(l1),
            ])))
        }
        ("HH\\sequence", &[]) => Ok(Some(InstrSeq::make_null())),
        ("HH\\sequence", args) => Ok(Some(InstrSeq::gather(
            args.iter()
                .map(|arg| emit_expr(e, env, arg))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .intersperse(InstrSeq::make_popc())
                .collect::<Vec<_>>(),
        ))),
        ("class_exists", _) if nargs == 1 || nargs == 2 => unimplemented!(),
        ("trait_exists", _) if nargs == 1 || nargs == 2 => unimplemented!(),
        ("interface_exists", _) if nargs == 1 || nargs == 2 => unimplemented!(),
        ("exit", _) | ("die", _) if nargs == 0 || nargs == 1 => {
            Ok(Some(emit_exit(e, env, args.first())?))
        }
        ("HH\\fun", _) => unimplemented!(),
        ("__systemlib\\fun", _) => unimplemented!(),
        ("HH\\inst_meth", _) => unimplemented!(),
        ("HH\\class_meth", _) => unimplemented!(),
        ("HH\\global_set", _) => unimplemented!(),
        ("HH\\global_unset", _) => unimplemented!(),
        ("__hhvm_internal_whresult", &[E(_, E_::Lvar(ref _p))]) => unimplemented!(),
        ("__hhvm_internal_newlikearrayl", &[E(_, E_::Lvar(ref _p)), E(_, E_::Int(ref _n))]) => {
            unimplemented!()
        }
        _ => Ok(
            match (
                args,
                istype_op(e.options(), lower_fq_name),
                is_isexp_op(lower_fq_name),
            ) {
                (&[ref arg_expr], _, Some(ref h)) => Some(InstrSeq::gather(vec![
                    emit_expr(e, env, &arg_expr)?,
                    emit_is(e, env, pos, &h)?,
                ])),
                (&[E(_, E_::Lvar(ref arg_id))], Some(i), _)
                    if superglobals::is_any_global(arg_id.name()) =>
                {
                    Some(InstrSeq::gather(vec![
                        emit_local(e, env, BareThisOp::NoNotice, &arg_id)?,
                        emit_pos(pos),
                        InstrSeq::make_istypec(i),
                    ]))
                }
                (&[E(_, E_::Lvar(ref arg_id))], Some(i), _) if !is_local_this(env, &arg_id.1) => {
                    Some(InstrSeq::make_istypel(
                        get_local(e, env, &arg_id.0, &(arg_id.1).1)?,
                        i,
                    ))
                }
                (&[ref arg_expr], Some(i), _) => Some(InstrSeq::gather(vec![
                    emit_expr(e, env, &arg_expr)?,
                    emit_pos(pos),
                    InstrSeq::make_istypec(i),
                ])),
                _ => match get_call_builtin_func_info(e.options(), lower_fq_name) {
                    Some((nargs, i)) if nargs == args.len() => Some(InstrSeq::gather(vec![
                        emit_exprs(e, env, args)?,
                        emit_pos(pos),
                        InstrSeq::make_instr(i),
                    ])),
                    _ => None,
                },
            },
        ),
    }
}

fn get_call_builtin_func_info(opts: &Options, id: impl AsRef<str>) -> Option<(usize, Instruct)> {
    use {Instruct::*, InstructGet::*, InstructIsset::*, InstructMisc::*, InstructOperator::*};
    let hack_arr_dv_arrs = hack_arr_dv_arrs(opts);
    match id.as_ref() {
        "array_key_exists" => Some((2, IMisc(AKExists))),
        "hphp_array_idx" => Some((3, IMisc(ArrayIdx))),
        "intval" => Some((1, IOp(CastInt))),
        "boolval" => Some((1, IOp(CastBool))),
        "strval" => Some((1, IOp(CastString))),
        "floatval" | "doubleval" => Some((1, IOp(CastDouble))),
        "HH\\vec" => Some((1, IOp(CastVec))),
        "HH\\keyset" => Some((1, IOp(CastKeyset))),
        "HH\\dict" => Some((1, IOp(CastDict))),
        "HH\\varray" => Some((
            1,
            IOp(if hack_arr_dv_arrs {
                CastVec
            } else {
                CastVArray
            }),
        )),
        "HH\\darray" => Some((
            1,
            IOp(if hack_arr_dv_arrs {
                CastDict
            } else {
                CastDArray
            }),
        )),
        "HH\\global_get" => Some((1, IGet(CGetG))),
        "HH\\global_isset" => Some((1, IIsset(IssetG))),
        _ => None,
    }
}

fn emit_is(e: &mut Emitter, env: &Env, pos: &Pos, h: &tast::Hint) -> Result {
    unimplemented!()
}

fn istype_op(opts: &Options, id: impl AsRef<str>) -> Option<IstypeOp> {
    let widen_is_array = opts.hhvm.flags.contains(HhvmFlags::WIDEN_IS_ARRAY);
    let hack_arr_dv_arrs = hack_arr_dv_arrs(opts);
    use IstypeOp::*;
    match id.as_ref() {
        "is_int" | "is_integer" | "is_long" => Some(OpInt),
        "is_bool" => Some(OpBool),
        "is_float" | "is_real" | "is_double" => Some(OpDbl),
        "is_string" => Some(OpStr),
        "is_array" => Some(if widen_is_array { OpArrLike } else { OpArr }),
        "is_object" => Some(OpObj),
        "is_null" => Some(OpNull),
        "is_scalar" => Some(OpScalar),
        "HH\\is_keyset" => Some(OpKeyset),
        "HH\\is_dict" => Some(OpDict),
        "HH\\is_vec" => Some(OpVec),
        "HH\\is_varray" => Some(if hack_arr_dv_arrs { OpVec } else { OpVArray }),
        "HH\\is_darray" => Some(if hack_arr_dv_arrs { OpDict } else { OpDArray }),
        "HH\\is_any_array" => Some(OpArrLike),
        "HH\\is_class_meth" => Some(OpClsMeth),
        "HH\\is_fun" => Some(OpFunc),
        "HH\\is_php_array" => Some(OpPHPArr),
        _ => None,
    }
}

fn is_isexp_op(lower_fq_id: impl AsRef<str>) -> Option<tast::Hint> {
    let h = |s: &str| {
        Some(tast::Hint::new(
            Pos::make_none(),
            tast::Hint_::mk_happly(tast::Id(Pos::make_none(), s.into()), vec![]),
        ))
    };
    match lower_fq_id.as_ref() {
        "is_int" | "is_integer" | "is_long" => h("\\HH\\int"),
        "is_bool" => h("\\HH\\bool"),
        "is_float" | "is_real" | "is_double" => h("\\HH\\float"),
        "is_string" => h("\\HH\\string"),
        "is_null" => h("\\HH\\void"),
        "HH\\is_keyset" => h("\\HH\\keyset"),
        "HH\\is_dict" => h("\\HH\\dict"),
        "HH\\is_vec" => h("\\HH\\vec"),
        _ => None,
    }
}

fn emit_eval(e: &mut Emitter, env: &Env, pos: &Pos, expr: &tast::Expr) -> Result {
    Ok(InstrSeq::gather(vec![
        emit_expr(e, env, expr)?,
        emit_pos(pos),
        InstrSeq::make_eval(),
    ]))
}

fn emit_call_expr(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    async_eager_label: Option<Label>,
    (_, expr, targs, args, uarg): &(
        tast::CallType,
        tast::Expr,
        Vec<tast::Targ>,
        Vec<tast::Expr>,
        Option<tast::Expr>,
    ),
) -> Result {
    let jit_enable_rename_function = e
        .options()
        .hhvm
        .flags
        .contains(HhvmFlags::JIT_ENABLE_RENAME_FUNCTION);
    use {tast::Expr as E, tast::Expr_ as E_};
    match (&expr.1, &args[..], uarg) {
        (E_::Id(id), [E(_, E_::String(data))], None) if id.1 == special_functions::HHAS_ADATA => {
            let v = TypedValue::HhasAdata(data.into());
            Ok(emit_pos_then(pos, InstrSeq::make_typedvalue(v)))
        }
        (E_::Id(id), _, None) if id.1 == pseudo_functions::ISSET => {
            emit_call_isset_exprs(e, env, pos, args)
        }
        (E_::Id(id), args, None)
            if id.1 == fb::IDX
                && !jit_enable_rename_function
                && (args.len() == 2 || args.len() == 3) =>
        {
            emit_idx(e, env, pos, args)
        }
        (E_::Id(id), [arg1], None) if id.1 == emitter_special_functions::EVAL => {
            emit_eval(e, env, pos, arg1)
        }
        (E_::Id(id), [arg1], None) if id.1 == emitter_special_functions::SET_FRAME_METADATA => {
            Ok(InstrSeq::gather(vec![
                emit_expr(e, env, arg1)?,
                emit_pos(pos),
                InstrSeq::make_popl(local::Type::Named("$86metadata".into())),
                InstrSeq::make_null(),
            ]))
        }
        (E_::Id(id), [], None)
            if id.1 == pseudo_functions::EXIT || id.1 == pseudo_functions::DIE =>
        {
            let exit = emit_exit(e, env, None)?;
            Ok(emit_pos_then(pos, exit))
        }
        (E_::Id(id), [arg1], None)
            if id.1 == pseudo_functions::EXIT || id.1 == pseudo_functions::DIE =>
        {
            let exit = emit_exit(e, env, Some(arg1))?;
            Ok(emit_pos_then(pos, exit))
        }
        (_, _, _) => {
            let instrs = emit_call(
                e,
                env,
                pos,
                expr,
                targs,
                args,
                uarg.as_ref(),
                async_eager_label,
            )?;
            Ok(emit_pos_then(pos, instrs))
        }
    }
}

fn emit_reified_generic_instrs(e: &mut Emitter, pos: &Pos, is_fun: bool, index: usize) -> Result {
    let base = if is_fun {
        InstrSeq::make_basel(
            local::Type::Named(string_utils::reified::GENERICS_LOCAL_NAME.into()),
            MemberOpMode::Warn,
        )
    } else {
        InstrSeq::gather(vec![
            InstrSeq::make_checkthis(),
            InstrSeq::make_baseh(),
            InstrSeq::make_dim_warn_pt(prop::from_raw_string(string_utils::reified::PROP_NAME)),
        ])
    };
    Ok(emit_pos_then(
        pos,
        InstrSeq::gather(vec![
            base,
            InstrSeq::make_querym(0, QueryOp::CGet, MemberKey::EI(index.try_into().unwrap())),
        ]),
    ))
}

fn emit_reified_type(e: &mut Emitter, env: &Env, pos: &Pos, name: &str) -> Result<InstrSeq> {
    emit_reified_type_opt(e, env, pos, name)?
        .ok_or_else(|| emit_fatal::raise_fatal_runtime(&Pos::make_none(), "Invalid reified param"))
}

fn emit_reified_type_opt(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    name: &str,
) -> Result<Option<InstrSeq>> {
    let is_in_lambda = env.scope.is_in_lambda();
    let cget_instr = |is_fun, i| {
        InstrSeq::make_cgetl(local::Type::Named(
            string_utils::reified::reified_generic_captured_name(is_fun, i),
        ))
    };
    let check = |is_soft| -> Result<()> {
        if is_soft {
            Err(emit_fatal::raise_fatal_parse(pos, format!(
                "{} is annotated to be a soft reified generic, it cannot be used until the __Soft annotation is removed",
                name
            )))
        } else {
            Ok(())
        }
    };
    let mut emit = |(i, is_soft), is_fun| {
        check(is_soft)?;
        Ok(Some(if is_in_lambda {
            cget_instr(is_fun, i)
        } else {
            emit_reified_generic_instrs(e, pos, is_fun, i)?
        }))
    };
    match is_reified_tparam(env, true, name) {
        Some((i, is_soft)) => emit((i, is_soft), true),
        None => match is_reified_tparam(env, false, name) {
            Some((i, is_soft)) => emit((i, is_soft), false),
            None => Ok(None),
        },
    }
}

fn emit_known_class_id(e: &mut Emitter, id: &ast_defs::Id) -> InstrSeq {
    let cid = class::Type::from_ast_name(&id.1);
    emit_symbol_refs::State::add_class(e, cid.clone());
    InstrSeq::gather(vec![
        InstrSeq::make_string(cid.to_raw_string()),
        InstrSeq::make_classgetc(),
    ])
}

fn emit_load_class_ref(e: &mut Emitter, env: &Env, pos: &Pos, cexpr: ClassExpr) -> Result {
    let instrs = match cexpr {
        ClassExpr::Special(SpecialClsRef::Self_) => InstrSeq::make_self(),
        ClassExpr::Special(SpecialClsRef::Static) => InstrSeq::make_lateboundcls(),
        ClassExpr::Special(SpecialClsRef::Parent) => InstrSeq::make_parent(),
        ClassExpr::Id(id) => emit_known_class_id(e, &id),
        ClassExpr::Expr(expr) => InstrSeq::gather(vec![
            emit_pos(pos),
            emit_expr(e, env, &expr)?,
            InstrSeq::make_classgetc(),
        ]),
        ClassExpr::Reified(instrs) => {
            InstrSeq::gather(vec![emit_pos(pos), instrs, InstrSeq::make_classgetc()])
        }
    };
    Ok(emit_pos_then(pos, instrs))
}

fn emit_new(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    (cid, targs, args, uarg, _): &(
        tast::ClassId,
        Vec<tast::Targ>,
        Vec<tast::Expr>,
        Option<tast::Expr>,
        Pos,
    ),
) -> Result {
    if has_inout_arg(args) {
        return Err(unrecoverable("Unexpected inout arg in new expr"));
    }
    let resolve_self = true;
    use HasGenericsOp as H;
    let cexpr = ClassExpr::class_id_to_class_expr(e, false, resolve_self, &env.scope, cid);
    let (cexpr, has_generics) = match &cexpr {
        ClassExpr::Id(ast_defs::Id(_, name)) => match emit_reified_type_opt(e, env, pos, name)? {
            Some(instrs) => {
                if targs.is_empty() {
                    (ClassExpr::Reified(instrs), H::MaybeGenerics)
                } else {
                    return Err(emit_fatal::raise_fatal_parse(
                        pos,
                        "Cannot have higher kinded reified generics",
                    ));
                }
            }
            None if !has_non_tparam_generics_targs(env, targs) => (cexpr, H::NoGenerics),
            None => (cexpr, H::HasGenerics),
        },
        _ => (cexpr, H::NoGenerics),
    };
    let newobj_instrs = match cexpr {
        ClassExpr::Id(ast_defs::Id(_, cname)) => {
            // TODO(hrust) enabel `let id = class::Type::from_ast_name(&cname);`,
            // `from_ast_name` should be able to accpet Cow<str>
            let id: class::Type = string_utils::strip_global_ns(&cname).to_string().into();
            emit_symbol_refs::State::add_class(e, id.clone());
            match has_generics {
                H::NoGenerics => InstrSeq::gather(vec![emit_pos(pos), InstrSeq::make_newobjd(id)]),
                H::HasGenerics => InstrSeq::gather(vec![
                    emit_pos(pos),
                    emit_reified_targs(
                        e,
                        env,
                        pos,
                        &targs.iter().map(|t| &t.1).collect::<Vec<_>>(),
                    )?,
                    InstrSeq::make_newobjrd(id),
                ]),
                H::MaybeGenerics => {
                    return Err(unrecoverable(
                        "Internal error: This case should have been transformed",
                    ))
                }
            }
        }
        ClassExpr::Special(cls_ref) => {
            InstrSeq::gather(vec![emit_pos(pos), InstrSeq::make_newobjs(cls_ref)])
        }
        ClassExpr::Reified(instrs) if has_generics == H::MaybeGenerics => InstrSeq::gather(vec![
            instrs,
            InstrSeq::make_classgetts(),
            InstrSeq::make_newobjr(),
        ]),
        _ => InstrSeq::gather(vec![
            emit_load_class_ref(e, env, pos, cexpr)?,
            InstrSeq::make_newobj(),
        ]),
    };
    scope::with_unnamed_locals(e, |e| {
        let (instr_args, _) = emit_args_inout_setters(e, env, args)?;
        let instr_uargs = match uarg {
            None => InstrSeq::Empty,
            Some(uarg) => emit_expr(e, env, uarg)?,
        };
        Ok((
            InstrSeq::Empty,
            InstrSeq::gather(vec![
                newobj_instrs,
                InstrSeq::make_dup(),
                InstrSeq::make_nulluninit(),
                InstrSeq::make_nulluninit(),
                instr_args,
                instr_uargs,
                emit_pos(pos),
                InstrSeq::make_fcallctor(get_fcall_args(args, uarg.as_ref(), None, None, true)),
                InstrSeq::make_popc(),
                InstrSeq::make_lockobj(),
            ]),
            InstrSeq::Empty,
        ))
    })
}

fn emit_obj_get(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    query_op: QueryOp,
    expr: &tast::Expr,
    prop: &tast::Expr,
    nullflavor: &ast_defs::OgNullFlavor,
    null_coalesce_assignment: bool,
) -> Result<(InstrSeq, Option<StackIndex>)> {
    if let Some(tast::Lid(pos, id)) = expr.1.as_lvar() {
        if local_id::get_name(&id) == special_idents::THIS
            && nullflavor.eq(&ast_defs::OgNullFlavor::OGNullsafe)
        {
            return Err(emit_fatal::raise_fatal_parse(
                pos,
                "?-> is not allowed with $this",
            ));
        }
    }
    if let Some(ast_defs::Id(_, s)) = prop.1.as_id() {
        if string_utils::is_xhp(s) {
            return Ok((emit_xhp_obj_get(e, env, pos, &expr, s, nullflavor)?, None));
        }
    }
    let mode = if null_coalesce_assignment {
        MemberOpMode::Warn
    } else {
        get_querym_op_mode(&query_op)
    };
    let prop_stack_size = emit_prop_expr(e, env, nullflavor, 0, prop, null_coalesce_assignment)?.2;
    let (
        base_expr_instrs_begin,
        base_expr_instrs_end,
        base_setup_instrs,
        base_stack_size,
        cls_stack_size,
    ) = emit_base(
        e,
        env,
        expr,
        mode,
        true,
        null_coalesce_assignment,
        prop_stack_size,
        0,
    )?;
    let (mk, prop_instrs, _) = emit_prop_expr(
        e,
        env,
        nullflavor,
        cls_stack_size,
        prop,
        null_coalesce_assignment,
    )?;
    let total_stack_size = prop_stack_size + base_stack_size + cls_stack_size;
    let num_params = if null_coalesce_assignment {
        0
    } else {
        total_stack_size as usize
    };
    let final_instr = InstrSeq::make_querym(num_params, query_op, mk);
    let querym_n_unpopped = if null_coalesce_assignment {
        Some(total_stack_size)
    } else {
        None
    };
    let instr = InstrSeq::gather(vec![
        base_expr_instrs_begin,
        prop_instrs,
        base_expr_instrs_end,
        emit_pos(pos),
        base_setup_instrs,
        final_instr,
    ]);
    Ok((instr, querym_n_unpopped))
}

// Get the member key for a property, and return any instructions and
// the size of the stack in the case that the property cannot be
// placed inline in the instruction.
fn emit_prop_expr(
    e: &mut Emitter,
    env: &Env,
    nullflavor: &ast_defs::OgNullFlavor,
    stack_index: StackIndex,
    prop: &tast::Expr,
    null_coalesce_assignment: bool,
) -> Result<(MemberKey, InstrSeq, StackIndex)> {
    let mk = match &prop.1 {
        tast::Expr_::Id(id) => {
            let ast_defs::Id(pos, name) = &**id;
            if name.starts_with("$") {
                MemberKey::PL(get_local(e, env, pos, name)?)
            } else {
                // Special case for known property name

                // TODO(hrust) enable `let pid = prop::Type::from_ast_name(name);`,
                // `from_ast_name` should be able to accpet Cow<str>
                let pid: prop::Type = string_utils::strip_global_ns(&name).to_string().into();
                match nullflavor {
                    ast_defs::OgNullFlavor::OGNullthrows => MemberKey::PT(pid),
                    ast_defs::OgNullFlavor::OGNullsafe => MemberKey::QT(pid),
                }
            }
        }
        // Special case for known property name
        tast::Expr_::String(name) => {
            // TODO(hrust) enable `let pid = prop::Type::from_ast_name(name);`,
            // `from_ast_name` should be able to accpet Cow<str>
            let pid: prop::Type = string_utils::strip_global_ns(&name).to_string().into();
            match nullflavor {
                ast_defs::OgNullFlavor::OGNullthrows => MemberKey::PT(pid),
                ast_defs::OgNullFlavor::OGNullsafe => MemberKey::QT(pid),
            }
        }
        tast::Expr_::Lvar(lid) if !(is_local_this(env, &lid.1)) => {
            MemberKey::PL(get_local(e, env, &lid.0, local_id::get_name(&lid.1))?)
        }
        _ => {
            // General case
            MemberKey::PC(stack_index)
        }
    };
    // For nullsafe access, insist that property is known
    Ok(match mk {
        MemberKey::PL(_) | MemberKey::PC(_)
            if nullflavor.eq(&ast_defs::OgNullFlavor::OGNullsafe) =>
        {
            return Err(emit_fatal::raise_fatal_parse(
                &prop.0,
                "?-> can only be used with scalar property names",
            ))
        }
        MemberKey::PC(_) => (mk, emit_expr(e, env, prop)?, 1),
        MemberKey::PL(local) if null_coalesce_assignment => {
            (MemberKey::PC(stack_index), InstrSeq::make_cgetl(local), 1)
        }
        _ => (mk, InstrSeq::Empty, 0),
    })
}

fn emit_xhp_obj_get(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    expr: &tast::Expr,
    s: &str,
    nullflavor: &ast_defs::OgNullFlavor,
) -> Result {
    use tast::Expr as E;
    use tast::Expr_ as E_;
    let f = E(
        pos.clone(),
        E_::mk_obj_get(
            expr.clone(),
            E(
                pos.clone(),
                E_::mk_id(ast_defs::Id(pos.clone(), "getAttribute".into())),
            ),
            nullflavor.clone(),
        ),
    );
    let args = vec![E(pos.clone(), E_::mk_string(string_utils::clean(s).into()))];
    emit_call(e, env, pos, &f, &[], &args[..], None, None)
}

fn emit_array_get(
    e: &mut Emitter,
    env: &Env,
    outer_pos: &Pos,
    mode: Option<MemberOpMode>,
    query_op: QueryOp,
    base: &tast::Expr,
    elem: Option<&tast::Expr>,
    no_final: bool,
    null_coalesce_assignment: bool,
) -> Result<(InstrSeq, Option<usize>)> {
    let result = emit_array_get_(
        e,
        env,
        outer_pos,
        mode,
        query_op,
        base,
        elem,
        no_final,
        null_coalesce_assignment,
        None,
    )?;
    match result {
        (ArrayGetInstr::Regular(i), querym_n_unpopped) => Ok((i, querym_n_unpopped)),
        (ArrayGetInstr::Inout { load, store }, _) => Err(unrecoverable("unexpected inout")),
    }
}

fn emit_array_get_(
    e: &mut Emitter,
    env: &Env,
    outer_pos: &Pos,
    mode: Option<MemberOpMode>,
    query_op: QueryOp,
    base_expr: &tast::Expr,
    elem: Option<&tast::Expr>,
    no_final: bool,
    null_coalesce_assignment: bool,
    inout_param_info: Option<(usize, &inout_locals::AliasInfoMap)>,
) -> Result<(ArrayGetInstr, Option<usize>)> {
    use tast::{Expr as E, Expr_ as E_};
    match (base_expr, elem) {
        (E(pos, E_::Array(_)), None) => Err(emit_fatal::raise_fatal_parse(
            pos,
            "Can't use array() as base in write context",
        )),
        (E(pos, _), None) if !env.flags.contains(env::Flags::ALLOWS_ARRAY_APPEND) => Err(
            emit_fatal::raise_fatal_runtime(pos, "Can't use [] for reading"),
        ),
        _ => {
            let local_temp_kind = get_local_temp_kind(env, false, inout_param_info, elem);
            let mode = if null_coalesce_assignment {
                MemberOpMode::Warn
            } else {
                mode.unwrap_or(get_querym_op_mode(&query_op))
            };
            let (elem_instrs, elem_stack_size) =
                emit_elem(e, env, elem, local_temp_kind, null_coalesce_assignment)?;
            let base_result = emit_base_(
                e,
                env,
                base_expr,
                mode,
                false,
                null_coalesce_assignment,
                elem_stack_size,
                0,
                inout_param_info,
            )?;
            let cls_stack_size = match &base_result {
                ArrayGetBase::Regular(base) => base.cls_stack_size,
                ArrayGetBase::Inout { load, .. } => load.cls_stack_size,
            };
            let memberkey =
                get_elem_member_key(e, env, cls_stack_size, elem, null_coalesce_assignment)?;
            let mut querym_n_unpopped = None;
            let mut make_final = |total_stack_size: StackIndex| -> InstrSeq {
                if no_final {
                    InstrSeq::Empty
                } else if null_coalesce_assignment {
                    querym_n_unpopped = Some(total_stack_size as usize);
                    InstrSeq::make_querym(0, query_op, memberkey.clone())
                } else {
                    InstrSeq::make_querym(total_stack_size as usize, query_op, memberkey.clone())
                }
            };
            let instr = match (base_result, local_temp_kind) {
                (ArrayGetBase::Regular(base), None) =>
                // neither base nor expression needs to store anything
                {
                    ArrayGetInstr::Regular(InstrSeq::gather(vec![
                        base.base_instrs,
                        elem_instrs,
                        base.cls_instrs,
                        emit_pos(outer_pos),
                        base.setup_instrs,
                        make_final(base.base_stack_size + base.cls_stack_size + elem_stack_size),
                    ]))
                }
                (ArrayGetBase::Regular(base), Some(local_kind)) => {
                    // base does not need temp locals but index expression does
                    let local = e.local_gen_mut().get_unnamed();
                    // load base and indexer, value of indexer will be saved in local
                    let load = vec![
                        (
                            InstrSeq::gather(vec![base.base_instrs.clone(), elem_instrs]),
                            Some((local.clone(), local_kind)),
                        ),
                        (
                            InstrSeq::gather(vec![
                                base.base_instrs,
                                emit_pos(outer_pos),
                                base.setup_instrs,
                                make_final(
                                    base.base_stack_size + base.cls_stack_size + elem_stack_size,
                                ),
                            ]),
                            None,
                        ),
                    ];
                    let store = InstrSeq::gather(vec![
                        emit_store_for_simple_base(
                            e,
                            env,
                            outer_pos,
                            elem_stack_size,
                            base_expr,
                            local,
                            false,
                        )?,
                        InstrSeq::make_popc(),
                    ]);
                    ArrayGetInstr::Inout { load, store }
                }
                (
                    ArrayGetBase::Inout {
                        load:
                            ArrayGetBaseData {
                                mut base_instrs,
                                cls_instrs,
                                setup_instrs,
                                base_stack_size,
                                cls_stack_size,
                            },
                        store,
                    },
                    None,
                ) => {
                    // base needs temp locals, indexer - does not,
                    // simply concat two instruction sequences
                    base_instrs.push((
                        InstrSeq::gather(vec![
                            elem_instrs,
                            cls_instrs,
                            emit_pos(outer_pos),
                            setup_instrs,
                            make_final(base_stack_size + cls_stack_size + elem_stack_size),
                        ]),
                        None,
                    ));
                    let store = InstrSeq::gather(vec![
                        store,
                        InstrSeq::make_setm(0, memberkey),
                        InstrSeq::make_popc(),
                    ]);
                    ArrayGetInstr::Inout {
                        load: base_instrs,
                        store,
                    }
                }
                (
                    ArrayGetBase::Inout {
                        load:
                            ArrayGetBaseData {
                                mut base_instrs,
                                cls_instrs,
                                setup_instrs,
                                base_stack_size,
                                cls_stack_size,
                            },
                        store,
                    },
                    Some(local_kind),
                ) => {
                    // both base and index need temp locals,
                    // create local for index value
                    let local = e.local_gen_mut().get_unnamed();
                    base_instrs.push((elem_instrs, Some((local.clone(), local_kind))));
                    base_instrs.push((
                        InstrSeq::gather(vec![
                            cls_instrs,
                            emit_pos(outer_pos),
                            setup_instrs,
                            make_final(base_stack_size + cls_stack_size + elem_stack_size),
                        ]),
                        None,
                    ));
                    let store = InstrSeq::gather(vec![
                        store,
                        InstrSeq::make_setm(0, MemberKey::EL(local)),
                        InstrSeq::make_popc(),
                    ]);
                    ArrayGetInstr::Inout {
                        load: base_instrs,
                        store,
                    }
                }
            };
            Ok((instr, querym_n_unpopped))
        }
    }
}

fn is_special_class_constant_accessed_with_class_id(cname: &tast::ClassId_, id: &str) -> bool {
    let is_self_parent_or_static = match cname {
        tast::ClassId_::CIexpr(tast::Expr(_, tast::Expr_::Id(id))) => {
            string_utils::is_self(&id.1)
                || string_utils::is_parent(&id.1)
                || string_utils::is_static(&id.1)
        }
        _ => false,
    };
    string_utils::is_class(id) && !is_self_parent_or_static
}

fn emit_elem(
    e: &mut Emitter,
    env: &Env,
    elem: Option<&tast::Expr>,
    local_temp_kind: Option<StoredValueKind>,
    null_coalesce_assignment: bool,
) -> Result<(InstrSeq, StackIndex)> {
    Ok(match elem {
        None => (InstrSeq::Empty, 0),
        Some(expr) if expr.1.is_int() || expr.1.is_string() => (InstrSeq::Empty, 0),
        Some(expr) => match &expr.1 {
            tast::Expr_::Lvar(x) if !is_local_this(env, &x.1) => {
                if local_temp_kind.is_some() {
                    (
                        InstrSeq::make_cgetquietl(get_local(
                            e,
                            env,
                            &x.0,
                            local_id::get_name(&x.1),
                        )?),
                        0,
                    )
                } else if null_coalesce_assignment {
                    (
                        InstrSeq::make_cgetl(get_local(e, env, &x.0, local_id::get_name(&x.1))?),
                        1,
                    )
                } else {
                    (InstrSeq::Empty, 0)
                }
            }
            tast::Expr_::ClassConst(x)
                if is_special_class_constant_accessed_with_class_id(&(x.0).1, &(x.1).1) =>
            {
                (InstrSeq::Empty, 0)
            }
            _ => (emit_expr(e, env, expr)?, 1),
        },
    })
}

fn get_elem_member_key(
    e: &mut Emitter,
    env: &Env,
    stack_index: StackIndex,
    elem: Option<&tast::Expr>,
    null_coalesce_assignment: bool,
) -> Result<MemberKey> {
    use tast::ClassId_ as CI_;
    use tast::Expr as E;
    use tast::Expr_ as E_;
    match elem {
        // ELement missing (so it's array append)
        None => Ok(MemberKey::W),
        Some(elem_expr) => match &elem_expr.1 {
            // Special case for local
            E_::Lvar(x) if !is_local_this(env, &x.1) => Ok({
                if null_coalesce_assignment {
                    MemberKey::EC(stack_index)
                } else {
                    MemberKey::EL(get_local(e, env, &x.0, local_id::get_name(&x.1))?)
                }
            }),
            // Special case for literal integer
            E_::Int(s) => {
                match ast_constant_folder::expr_to_typed_value(e, &env.namespace, elem_expr) {
                    Ok(TypedValue::Int(i)) => Ok(MemberKey::EI(i)),
                    _ => Err(Unrecoverable(format!("{} is not a valid integer index", s))),
                }
            }
            // Special case for literal string
            E_::String(s) => Ok(MemberKey::ET(s.clone())),
            // Special case for class name
            E_::ClassConst(x)
                if is_special_class_constant_accessed_with_class_id(&(x.0).1, &(x.1).1) =>
            {
                let cname =
                    match (&(x.0).1, env.scope.get_class()) {
                        (CI_::CIself, Some(cd)) => string_utils::strip_global_ns(&(cd.name).1),
                        (CI_::CIexpr(E(_, E_::Id(id))), _) => string_utils::strip_global_ns(&id.1),
                        (CI_::CI(id), _) => string_utils::strip_global_ns(&id.1),
                        _ => return Err(Unrecoverable(
                            "Unreachable due to is_special_class_constant_accessed_with_class_id"
                                .into(),
                        )),
                    };
                let fq_id = class::Type::from_ast_name(&cname).to_raw_string().into();
                Ok(MemberKey::ET(fq_id))
            }
            _ => {
                // General case
                Ok(MemberKey::EC(stack_index))
            }
        },
    }
}

fn emit_store_for_simple_base(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    elem_stack_size: isize,
    base: &tast::Expr,
    local: local::Type,
    is_base: bool,
) -> Result {
    let (base_expr_instrs_begin, base_expr_instrs_end, base_setup_instrs, _, _) = emit_base(
        e,
        env,
        base,
        MemberOpMode::Define,
        false,
        false,
        elem_stack_size,
        0,
    )?;
    let memberkey = MemberKey::EL(local);
    Ok(InstrSeq::gather(vec![
        base_expr_instrs_begin,
        base_expr_instrs_end,
        emit_pos(pos),
        base_setup_instrs,
        if is_base {
            InstrSeq::make_dim(MemberOpMode::Define, memberkey)
        } else {
            InstrSeq::make_setm(0, memberkey)
        },
    ]))
}

fn get_querym_op_mode(query_op: &QueryOp) -> MemberOpMode {
    match query_op {
        QueryOp::InOut => MemberOpMode::InOut,
        QueryOp::CGet => MemberOpMode::Warn,
        _ => MemberOpMode::ModeNone,
    }
}

fn emit_class_get(
    env: &Env,
    query_op: QueryOp,
    (cid, cls_get_expr): &(tast::ClassId, tast::ClassGetExpr),
) -> Result {
    unimplemented!("TODO(hrust)")
}

fn emit_conditional_expr(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    etest: &tast::Expr,
    etrue: &Option<tast::Expr>,
    efalse: &tast::Expr,
) -> Result {
    Ok(match etrue.as_ref() {
        Some(etrue) => {
            let false_label = e.label_gen_mut().next_regular();
            let end_label = e.label_gen_mut().next_regular();
            let r = emit_jmpz(e, env, etest, &false_label)?;
            InstrSeq::gather(vec![
                r.instrs,
                if r.is_fallthrough {
                    InstrSeq::gather(vec![
                        emit_expr(e, env, etrue)?,
                        emit_pos(pos),
                        InstrSeq::make_jmp(end_label.clone()),
                    ])
                } else {
                    InstrSeq::Empty
                },
                if r.is_label_used {
                    InstrSeq::gather(vec![
                        InstrSeq::make_label(false_label),
                        emit_expr(e, env, efalse)?,
                    ])
                } else {
                    InstrSeq::Empty
                },
                if r.is_fallthrough {
                    InstrSeq::make_label(end_label)
                } else {
                    InstrSeq::Empty
                },
            ])
        }
        None => {
            let end_label = e.label_gen_mut().next_regular();
            InstrSeq::gather(vec![
                emit_expr(e, env, etest)?,
                InstrSeq::make_dup(),
                InstrSeq::make_jmpnz(end_label.clone()),
                InstrSeq::make_popc(),
                emit_expr(e, env, efalse)?,
                InstrSeq::make_label(end_label),
            ])
        }
    })
}

fn emit_local(e: &mut Emitter, env: &Env, notice: BareThisOp, lid: &aast_defs::Lid) -> Result {
    let tast::Lid(pos, id) = lid;
    let id_name = local_id::get_name(id);
    if superglobals::GLOBALS == id_name {
        Err(emit_fatal::raise_fatal_parse(
            pos,
            "Access $GLOBALS via wrappers",
        ))
    } else if superglobals::is_superglobal(id_name) {
        Ok(InstrSeq::gather(vec![
            InstrSeq::make_string(string_utils::locals::strip_dollar(id_name)),
            emit_pos(pos),
            InstrSeq::make_cgetg(),
        ]))
    } else {
        let local = get_local(e, env, pos, id_name)?;
        Ok(
            if is_local_this(env, id) && !env.flags.contains(EnvFlags::NEEDS_LOCAL_THIS) {
                emit_pos_then(pos, InstrSeq::make_barethis(notice))
            } else {
                InstrSeq::make_cgetl(local)
            },
        )
    }
}

fn emit_class_const(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    cid: &tast::ClassId,
    id: &ast_defs::Pstring,
) -> Result {
    let mut cexpr = ClassExpr::class_id_to_class_expr(e, false, true, &env.scope, cid);
    if let ClassExpr::Id(ast_defs::Id(_, name)) = &cexpr {
        if let Some(reified_var_cexpr) = get_reified_var_cexpr(e, env, pos, &name)? {
            cexpr = reified_var_cexpr;
        }
    }
    match cexpr {
        ClassExpr::Id(ast_defs::Id(pos, name)) => {
            // TODO(hrust) enabel `let cid = class::Type::from_ast_name(&cname);`,
            // `from_ast_name` should be able to accpet Cow<str>
            let cid: class::Type = string_utils::strip_global_ns(&name).to_string().into();
            let cname = cid.to_raw_string();
            Ok(if string_utils::is_class(&id.1) {
                emit_pos_then(&pos, InstrSeq::make_string(cname))
            } else {
                emit_symbol_refs::State::add_class(e, cid.clone());
                // TODO(hrust) enabel `let const_id = r#const::Type::from_ast_name(&id.1);`,
                // `from_ast_name` should be able to accpet Cow<str>
                let const_id: r#const::Type =
                    string_utils::strip_global_ns(&id.1).to_string().into();
                emit_pos_then(&pos, InstrSeq::make_clscnsd(const_id, cid))
            })
        }
        _ => {
            let load_const = if string_utils::is_class(&id.1) {
                InstrSeq::make_classname()
            } else {
                // TODO(hrust) enabel `let const_id = r#const::Type::from_ast_name(&id.1);`,
                // `from_ast_name` should be able to accpet Cow<str>
                let const_id: r#const::Type =
                    string_utils::strip_global_ns(&id.1).to_string().into();
                InstrSeq::make_clscns(const_id)
            };
            Ok(InstrSeq::gather(vec![
                emit_load_class_ref(e, env, pos, cexpr)?,
                load_const,
            ]))
        }
    }
}

fn emit_unop(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    (uop, expr): &(ast_defs::Uop, tast::Expr),
) -> Result {
    use ast_defs::Uop as U;
    match uop {
        U::Utild | U::Unot => Ok(InstrSeq::gather(vec![
            emit_expr(e, env, expr)?,
            emit_pos_then(pos, from_unop(e.options(), uop)?),
        ])),
        U::Uplus | U::Uminus => Ok(InstrSeq::gather(vec![
            emit_pos(pos),
            InstrSeq::make_int(0),
            emit_expr(e, env, expr)?,
            emit_pos_then(pos, from_unop(e.options(), uop)?),
        ])),
        U::Uincr | U::Udecr | U::Upincr | U::Updecr => emit_lval_op(
            e,
            env,
            pos,
            LValOp::IncDec(unop_to_incdec_op(e.options(), uop)?),
            expr,
            None,
            false,
        ),
        U::Usilence => e.local_scope(|e| {
            let temp_local = e.local_gen_mut().get_unnamed();
            Ok(InstrSeq::gather(vec![
                emit_pos(pos),
                InstrSeq::make_silence_start(temp_local.clone()),
                {
                    let try_instrs = emit_expr(e, env, expr)?;
                    let catch_instrs = InstrSeq::gather(vec![
                        emit_pos(pos),
                        InstrSeq::make_silence_end(temp_local.clone()),
                    ]);
                    InstrSeq::create_try_catch(
                        e.label_gen_mut(),
                        None,
                        false, /* skip_throw */
                        try_instrs,
                        catch_instrs,
                    )
                },
                emit_pos(pos),
                InstrSeq::make_silence_end(temp_local),
            ]))
        }),
    }
}

fn unop_to_incdec_op(opts: &Options, op: &ast_defs::Uop) -> Result<IncdecOp> {
    let check_int_overflow = opts
        .hhvm
        .hack_lang_flags
        .contains(LangFlags::CHECK_INT_OVERFLOW);
    let if_check_or = |op1, op2| Ok(if check_int_overflow { op1 } else { op2 });
    use {ast_defs::Uop as U, IncdecOp as I};
    match op {
        U::Uincr => if_check_or(I::PreIncO, I::PreInc),
        U::Udecr => if_check_or(I::PreDecO, I::PreDec),
        U::Upincr => if_check_or(I::PostIncO, I::PostInc),
        U::Updecr => if_check_or(I::PostDecO, I::PostDec),
        _ => Err(Unrecoverable("invalid incdec op".into())),
    }
}

fn from_unop(opts: &Options, op: &ast_defs::Uop) -> Result {
    let check_int_overflow = opts
        .hhvm
        .hack_lang_flags
        .contains(LangFlags::CHECK_INT_OVERFLOW);
    use ast_defs::Uop as U;
    Ok(match op {
        U::Utild => InstrSeq::make_bitnot(),
        U::Unot => InstrSeq::make_not(),
        U::Uplus => {
            if check_int_overflow {
                InstrSeq::make_addo()
            } else {
                InstrSeq::make_add()
            }
        }
        U::Uminus => {
            if check_int_overflow {
                InstrSeq::make_subo()
            } else {
                InstrSeq::make_sub()
            }
        }
        _ => {
            return Err(Unrecoverable(
                "this unary operation cannot be translated".into(),
            ))
        }
    })
}

fn binop_to_eqop(opts: &Options, op: &ast_defs::Bop) -> Option<EqOp> {
    use {ast_defs::Bop as B, EqOp::*};
    let check_int_overflow = opts
        .hhvm
        .hack_lang_flags
        .contains(LangFlags::CHECK_INT_OVERFLOW);
    match op {
        B::Plus => Some(if check_int_overflow {
            PlusEqualO
        } else {
            PlusEqual
        }),
        B::Minus => Some(if check_int_overflow {
            MinusEqualO
        } else {
            MinusEqual
        }),
        B::Star => Some(if check_int_overflow {
            MulEqualO
        } else {
            MulEqual
        }),
        B::Slash => Some(DivEqual),
        B::Starstar => Some(PowEqual),
        B::Amp => Some(AndEqual),
        B::Bar => Some(OrEqual),
        B::Xor => Some(XorEqual),
        B::Ltlt => Some(SlEqual),
        B::Gtgt => Some(SrEqual),
        B::Percent => Some(ModEqual),
        B::Dot => Some(ConcatEqual),
        _ => None,
    }
}

fn optimize_null_checks(e: &Emitter) -> bool {
    e.options()
        .hack_compiler_flags
        .contains(CompilerFlags::OPTIMIZE_NULL_CHECKS)
}

fn from_binop(opts: &Options, op: &ast_defs::Bop) -> Result {
    let check_int_overflow = opts
        .hhvm
        .hack_lang_flags
        .contains(LangFlags::CHECK_INT_OVERFLOW);
    use ast_defs::Bop as B;
    Ok(match op {
        B::Plus => {
            if check_int_overflow {
                InstrSeq::make_addo()
            } else {
                InstrSeq::make_add()
            }
        }
        B::Minus => {
            if check_int_overflow {
                InstrSeq::make_subo()
            } else {
                InstrSeq::make_sub()
            }
        }
        B::Star => {
            if check_int_overflow {
                InstrSeq::make_mulo()
            } else {
                InstrSeq::make_mul()
            }
        }
        B::Slash => InstrSeq::make_div(),
        B::Eqeq => InstrSeq::make_eq(),
        B::Eqeqeq => InstrSeq::make_same(),
        B::Starstar => InstrSeq::make_pow(),
        B::Diff => InstrSeq::make_neq(),
        B::Diff2 => InstrSeq::make_nsame(),
        B::Lt => InstrSeq::make_lt(),
        B::Lte => InstrSeq::make_lte(),
        B::Gt => InstrSeq::make_gt(),
        B::Gte => InstrSeq::make_gte(),
        B::Dot => InstrSeq::make_concat(),
        B::Amp => InstrSeq::make_bitand(),
        B::Bar => InstrSeq::make_bitor(),
        B::Ltlt => InstrSeq::make_shl(),
        B::Gtgt => InstrSeq::make_shr(),
        B::Cmp => InstrSeq::make_cmp(),
        B::Percent => InstrSeq::make_mod(),
        B::Xor => InstrSeq::make_bitxor(),
        B::LogXor => InstrSeq::make_xor(),
        B::Eq(_) => return Err(Unrecoverable("assignment is emitted differently".into())),
        B::QuestionQuestion => {
            return Err(Unrecoverable(
                "null coalescence is emitted differently".into(),
            ))
        }
        B::Barbar | B::Ampamp => {
            return Err(Unrecoverable(
                "short-circuiting operator cannot be generated as a simple binop".into(),
            ))
        }
    })
}

fn emit_first_expr(e: &mut Emitter, env: &Env, expr: &tast::Expr) -> Result<(InstrSeq, bool)> {
    Ok(match &expr.1 {
        tast::Expr_::Lvar(l)
            if !((is_local_this(env, &l.1) && !env.flags.contains(EnvFlags::NEEDS_LOCAL_THIS))
                || superglobals::is_any_global(local_id::get_name(&l.1))) =>
        {
            (
                InstrSeq::make_cgetl2(get_local(e, env, &l.0, local_id::get_name(&l.1))?),
                true,
            )
        }
        _ => (emit_expr(e, env, expr)?, false),
    })
}

pub fn emit_two_exprs(
    e: &mut Emitter,
    env: &Env,
    outer_pos: &Pos,
    e1: &tast::Expr,
    e2: &tast::Expr,
) -> Result {
    let (instrs1, is_under_top) = emit_first_expr(e, env, e1)?;
    let instrs2 = emit_expr(e, env, e2)?;
    let instrs2_is_var = e2.1.is_lvar();
    Ok(InstrSeq::gather(if is_under_top {
        if instrs2_is_var {
            vec![emit_pos(outer_pos), instrs2, instrs1]
        } else {
            vec![instrs2, emit_pos(outer_pos), instrs1]
        }
    } else if instrs2_is_var {
        vec![instrs1, emit_pos(outer_pos), instrs2]
    } else {
        vec![instrs1, instrs2, emit_pos(outer_pos)]
    }))
}

fn emit_quiet_expr(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    expr: &tast::Expr,
    null_coalesce_assignment: bool,
) -> Result<(InstrSeq, Option<NumParams>)> {
    unimplemented!()
}

fn emit_null_coalesce_assignment(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    e1: &tast::Expr,
    e2: &tast::Expr,
) -> Result {
    unimplemented!()
}

fn emit_binop(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    (op, e1, e2): &(ast_defs::Bop, tast::Expr, tast::Expr),
) -> Result {
    use ast_defs::Bop as B;
    match op {
        B::Ampamp | B::Barbar => unimplemented!("TODO(hrust)"),
        B::Eq(None) => emit_lval_op(e, env, pos, LValOp::Set, e1, Some(e2), false),
        B::Eq(Some(eop)) if eop.is_question_question() => {
            emit_null_coalesce_assignment(e, env, pos, e1, e2)
        }
        B::Eq(Some(eop)) => match binop_to_eqop(e.options(), eop) {
            None => Err(Unrecoverable("illegal eq op".into())),
            Some(op) => emit_lval_op(e, env, pos, LValOp::SetOp(op), e1, Some(e2), false),
        },
        B::QuestionQuestion => {
            let end_label = e.label_gen_mut().next_regular();
            Ok(InstrSeq::gather(vec![
                emit_quiet_expr(e, env, pos, e1, false)?.0,
                InstrSeq::make_dup(),
                InstrSeq::make_istypec(IstypeOp::OpNull),
                InstrSeq::make_not(),
                InstrSeq::make_jmpnz(end_label.clone()),
                InstrSeq::make_popc(),
                emit_expr(e, env, e2)?,
                InstrSeq::make_label(end_label),
            ]))
        }
        _ => {
            let default = |e: &mut Emitter| {
                Ok(InstrSeq::gather(vec![
                    emit_two_exprs(e, env, pos, e1, e2)?,
                    from_binop(e.options(), op)?,
                ]))
            };
            if optimize_null_checks(e) {
                match op {
                    B::Eqeqeq if e2.1.is_null() => emit_is_null(e, env, e1),
                    B::Eqeqeq if e1.1.is_null() => emit_is_null(e, env, e2),
                    B::Diff2 if e2.1.is_null() => Ok(InstrSeq::gather(vec![
                        emit_is_null(e, env, e1)?,
                        InstrSeq::make_not(),
                    ])),
                    B::Diff2 if e1.1.is_null() => Ok(InstrSeq::gather(vec![
                        emit_is_null(e, env, e2)?,
                        InstrSeq::make_not(),
                    ])),
                    _ => default(e),
                }
            } else {
                default(e)
            }
        }
    }
}

fn emit_pipe(env: &Env, (_, e1, e2): &(aast_defs::Lid, tast::Expr, tast::Expr)) -> Result {
    unimplemented!("TODO(hrust)")
}

fn emit_is_hint(env: &Env, pos: &Pos, h: &aast_defs::Hint) -> Result {
    unimplemented!("TODO(hrust)")
}

fn emit_as(
    env: &Env,
    pos: &Pos,
    (e, h, is_nullable): &(tast::Expr, aast_defs::Hint, bool),
) -> Result {
    unimplemented!("TODO(hrust)")
}

fn emit_cast(env: &Env, pos: &Pos, (h, e): &(aast_defs::Hint, tast::Expr)) -> Result {
    unimplemented!("TODO(hrust)")
}

pub fn emit_unset_expr(e: &mut Emitter, env: &Env, expr: &tast::Expr) -> Result {
    emit_lval_op_nonlist(
        e,
        env,
        &expr.0,
        LValOp::Unset,
        expr,
        InstrSeq::Empty,
        0,
        false,
    )
}

pub fn emit_set_range_expr(
    e: &mut Emitter,
    env: &mut Env,
    pos: &Pos,
    name: &str,
    kind: Setrange,
    args: &[&tast::Expr],
) -> Result {
    let raise_fatal = |msg: &str| {
        Err(emit_fatal::raise_fatal_parse(
            pos,
            format!("{} {}", name, msg),
        ))
    };

    let (base, offset, src, args) = if args.len() >= 3 {
        (&args[0], &args[1], &args[2], &args[3..])
    } else {
        return raise_fatal("expects at least 3 arguments");
    };

    let count_instrs = match (args, kind.vec) {
        ([c], true) => emit_expr(e, env, c)?,
        ([], _) => InstrSeq::make_int(-1),
        (_, false) => return raise_fatal("expects no more than 3 arguments"),
        (_, true) => return raise_fatal("expects no more than 4 arguments"),
    };

    let (base_expr, cls_expr, base_setup, base_stack, cls_stack) = emit_base(
        e,
        env,
        base,
        MemberOpMode::Define,
        false, /* is_object */
        false, /*null_coalesce_assignment*/
        3,     /* base_offset */
        3,     /* rhs_stack_size */
    )?;
    Ok(InstrSeq::gather(vec![
        base_expr,
        cls_expr,
        emit_expr(e, env, offset)?,
        emit_expr(e, env, src)?,
        count_instrs,
        base_setup,
        InstrSeq::make_instr(Instruct::IFinal(InstructFinal::SetRangeM(
            (base_stack + cls_stack)
                .try_into()
                .expect("StackIndex overflow"),
            kind.op,
            kind.size.try_into().expect("Setrange size overflow"),
        ))),
    ]))
}

pub fn is_reified_tparam(env: &Env, is_fun: bool, name: &str) -> Option<(usize, bool)> {
    let is = |tparams: &[tast::Tparam]| {
        let is_soft = |ual: &Vec<tast::UserAttribute>| {
            ual.iter().any(|ua| &ua.name.1 == user_attributes::SOFT)
        };
        use tast::ReifyKind::*;
        tparams.iter().enumerate().find_map(|(i, tp)| {
            if (tp.reified == Reified || tp.reified == SoftReified) && tp.name.1 == name {
                Some((i, is_soft(&tp.user_attributes)))
            } else {
                None
            }
        })
    };
    if is_fun {
        is(env.scope.get_fun_tparams())
    } else {
        is(&env.scope.get_class_tparams().list[..])
    }
}

/// Emit code for a base expression `expr` that forms part of
/// an element access `expr[elem]` or field access `expr->fld`.
/// The instructions are divided into three sections:
///   1. base and element/property expression instructions:
///      push non-trivial base and key values on the stack
///   2. base selector instructions: a sequence of Base/Dim instructions that
///      actually constructs the base address from "member keys" that are inlined
///      in the instructions, or pulled from the key values that
///      were pushed on the stack in section 1.
///   3. (constructed by the caller) a final accessor e.g. QueryM or setter
///      e.g. SetOpM instruction that has the final key inlined in the
///      instruction, or pulled from the key values that were pushed on the
///      stack in section 1.
/// The function returns a triple (base_instrs, base_setup_instrs, stack_size)
/// where base_instrs is section 1 above, base_setup_instrs is section 2, and
/// stack_size is the number of values pushed onto the stack by section 1.
///
/// For example, the r-value expression $arr[3][$ix+2]
/// will compile to
///   # Section 1, pushing the value of $ix+2 on the stack
///   Int 2
///   CGetL2 $ix
///   AddO
///   # Section 2, constructing the base address of $arr[3]
///   BaseL $arr Warn
///   Dim Warn EI:3
///   # Section 3, indexing the array using the value at stack position 0 (EC:0)
///   QueryM 1 CGet EC:0
///)
fn emit_base(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    mode: MemberOpMode,
    is_object: bool,
    null_coalesce_assignment: bool,
    base_offset: StackIndex,
    rhs_stack_size: StackIndex,
) -> Result<(InstrSeq, InstrSeq, InstrSeq, StackIndex, StackIndex)> {
    let result = emit_base_(
        e,
        env,
        expr,
        mode,
        is_object,
        null_coalesce_assignment,
        base_offset,
        rhs_stack_size,
        None,
    )?;
    match result {
        ArrayGetBase::Regular(i) => Ok((
            i.base_instrs,
            i.cls_instrs,
            i.setup_instrs,
            i.base_stack_size as isize,
            i.cls_stack_size as isize,
        )),
        ArrayGetBase::Inout { load, store } => Err(unrecoverable("unexpected input")),
    }
}

fn is_trivial(env: &Env, is_base: bool, expr: &tast::Expr) -> bool {
    use tast::Expr_ as E_;
    match &expr.1 {
        E_::Int(_) | E_::String(_) => true,
        E_::Lvar(x) => !is_local_this(env, &x.1) || env.flags.contains(EnvFlags::NEEDS_LOCAL_THIS),
        E_::ArrayGet(_) if !is_base => false,
        E_::ArrayGet(x) => {
            is_trivial(env, is_base, &x.0)
                && (x.1)
                    .as_ref()
                    .map_or(true, |e| is_trivial(env, is_base, &e))
        }
        _ => false,
    }
}

fn get_local_temp_kind(
    env: &Env,
    is_base: bool,
    inout_param_info: Option<(usize, &inout_locals::AliasInfoMap)>,
    expr: Option<&tast::Expr>,
) -> Option<StoredValueKind> {
    match (expr, inout_param_info) {
        (_, None) => None,
        (Some(tast::Expr(_, tast::Expr_::Lvar(id))), Some((i, aliases)))
            if inout_locals::should_save_local_value(id.name(), i, aliases) =>
        {
            Some(StoredValueKind::Local)
        }
        (Some(e), _) => {
            if is_trivial(env, is_base, e) {
                None
            } else {
                Some(StoredValueKind::Expr)
            }
        }
        (None, _) => None,
    }
}

// TODO(hrust): emit_base_ as emit_base_worker in Ocaml, remove this TODO after ported
fn emit_base_(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    mode: MemberOpMode,
    is_object: bool,
    null_coalesce_assignment: bool,
    base_offset: StackIndex,
    rhs_stack_size: StackIndex,
    inout_param_info: Option<(usize, &inout_locals::AliasInfoMap)>,
) -> Result<ArrayGetBase> {
    let pos = &expr.0;
    let expr_ = &expr.1;
    let base_mode = if mode == MemberOpMode::InOut {
        MemberOpMode::Warn
    } else {
        mode
    };
    let local_temp_kind = get_local_temp_kind(env, true, inout_param_info, Some(expr));
    let emit_default = |e: &mut Emitter,
                        base_instrs,
                        cls_instrs,
                        setup_instrs,
                        base_stack_size,
                        cls_stack_size| {
        match local_temp_kind {
            Some(local_temp) => {
                let local = e.local_gen_mut().get_unnamed();
                ArrayGetBase::Inout {
                    load: ArrayGetBaseData {
                        base_instrs: vec![(base_instrs, Some((local.clone(), local_temp)))],
                        cls_instrs,
                        setup_instrs,
                        base_stack_size,
                        cls_stack_size,
                    },
                    store: InstrSeq::make_basel(local, MemberOpMode::Define),
                }
            }
            _ => ArrayGetBase::Regular(ArrayGetBaseData {
                base_instrs,
                cls_instrs,
                setup_instrs,
                base_stack_size,
                cls_stack_size,
            }),
        }
    };
    use tast::Expr_ as E_;
    match expr_ {
        E_::Lvar(x) if &(x.1).1 == superglobals::GLOBALS => Err(emit_fatal::raise_fatal_runtime(
            pos,
            "Cannot use [] with $GLOBALS",
        )),
        E_::Lvar(x) if superglobals::is_superglobal(&(x.1).1) => {
            let base_instrs = emit_pos_then(
                &x.0,
                InstrSeq::make_string(string_utils::locals::strip_dollar(&(x.1).1)),
            );

            Ok(emit_default(
                e,
                base_instrs,
                InstrSeq::Empty,
                InstrSeq::make_basegc(base_offset, base_mode),
                1,
                0,
            ))
        }
        E_::Lvar(x) if is_object && &(x.1).1 == special_idents::THIS => {
            let base_instrs = emit_pos_then(&x.0, InstrSeq::make_checkthis());
            Ok(emit_default(
                e,
                base_instrs,
                InstrSeq::Empty,
                InstrSeq::make_baseh(),
                0,
                0,
            ))
        }
        E_::Lvar(x)
            if !is_local_this(env, &x.1) || env.flags.contains(EnvFlags::NEEDS_LOCAL_THIS) =>
        {
            let v = get_local(e, env, &x.0, &(x.1).1)?;
            let base_instr = if local_temp_kind.is_some() {
                InstrSeq::make_cgetquietl(v.clone())
            } else {
                InstrSeq::Empty
            };
            Ok(emit_default(
                e,
                base_instr,
                InstrSeq::Empty,
                InstrSeq::make_basel(v, base_mode),
                0,
                0,
            ))
        }
        E_::ArrayGet(x) => match (&(x.0).1, x.1.as_ref()) {
            (E_::Lvar(v), Some(expr)) if local_id::get_name(&v.1) == superglobals::GLOBALS => {
                Ok(match expr.1.as_lvar() {
                    Some(tast::Lid(pos, id)) => {
                        let v = get_local(e, env, pos, local_id::get_name(id))?;
                        emit_default(
                            e,
                            InstrSeq::Empty,
                            InstrSeq::Empty,
                            InstrSeq::make_basegl(v, base_mode),
                            0,
                            0,
                        )
                    }
                    _ => {
                        let elem_instrs = emit_expr(e, env, expr)?;
                        emit_default(
                            e,
                            elem_instrs,
                            InstrSeq::Empty,
                            InstrSeq::make_basegc(base_offset, base_mode),
                            1,
                            0,
                        )
                    }
                })
            }
            // $a[] can not be used as the base of an array get unless as an lval
            (_, None) if !env.flags.contains(env::Flags::ALLOWS_ARRAY_APPEND) => {
                return Err(emit_fatal::raise_fatal_runtime(
                    pos,
                    "Can't use [] for reading",
                ))
            }
            // base is in turn array_get - do a specific handling for inout params
            // if necessary
            (_, opt_elem_expr) => {
                let base_expr = &x.0;
                let local_temp_kind =
                    get_local_temp_kind(env, false, inout_param_info, opt_elem_expr);
                let (elem_instrs, elem_stack_size) = emit_elem(
                    e,
                    env,
                    opt_elem_expr,
                    local_temp_kind,
                    null_coalesce_assignment,
                )?;
                let base_result = emit_base_(
                    e,
                    env,
                    base_expr,
                    mode,
                    false,
                    null_coalesce_assignment,
                    base_offset + elem_stack_size,
                    rhs_stack_size,
                    inout_param_info,
                )?;
                let cls_stack_size = match &base_result {
                    ArrayGetBase::Regular(base) => base.cls_stack_size,
                    ArrayGetBase::Inout { load, .. } => load.cls_stack_size,
                };
                let mk = get_elem_member_key(
                    e,
                    env,
                    base_offset + cls_stack_size,
                    opt_elem_expr,
                    null_coalesce_assignment,
                )?;
                let make_setup_instrs = |base_setup_instrs: InstrSeq| {
                    InstrSeq::gather(vec![
                        base_setup_instrs,
                        InstrSeq::make_dim(mode, mk.clone()),
                    ])
                };
                Ok(match (base_result, local_temp_kind) {
                    // both base and index don't use temps - fallback to default handler
                    (ArrayGetBase::Regular(base), None) => emit_default(
                        e,
                        InstrSeq::gather(vec![base.base_instrs, elem_instrs]),
                        base.cls_instrs,
                        make_setup_instrs(base.setup_instrs),
                        base.base_stack_size + elem_stack_size,
                        base.cls_stack_size,
                    ),
                    // base does not need temps but index does
                    (ArrayGetBase::Regular(base), Some(local_temp)) => {
                        let local = e.local_gen_mut().get_unnamed();
                        let base_instrs = InstrSeq::gather(vec![base.base_instrs, elem_instrs]);
                        ArrayGetBase::Inout {
                            load: ArrayGetBaseData {
                                // store result of instr_begin to temp
                                base_instrs: vec![(base_instrs, Some((local.clone(), local_temp)))],
                                cls_instrs: base.cls_instrs,
                                setup_instrs: make_setup_instrs(base.setup_instrs),
                                base_stack_size: base.base_stack_size + elem_stack_size,
                                cls_stack_size: base.cls_stack_size,
                            },
                            store: emit_store_for_simple_base(
                                e,
                                env,
                                pos,
                                elem_stack_size,
                                base_expr,
                                local,
                                true,
                            )?,
                        }
                    }
                    // base needs temps, index - does not
                    (
                        ArrayGetBase::Inout {
                            load:
                                ArrayGetBaseData {
                                    mut base_instrs,
                                    cls_instrs,
                                    setup_instrs,
                                    base_stack_size,
                                    cls_stack_size,
                                },
                            store,
                        },
                        None,
                    ) => {
                        base_instrs.push((elem_instrs, None));
                        ArrayGetBase::Inout {
                            load: ArrayGetBaseData {
                                base_instrs,
                                cls_instrs,
                                setup_instrs: make_setup_instrs(setup_instrs),
                                base_stack_size: base_stack_size + elem_stack_size,
                                cls_stack_size,
                            },
                            store: InstrSeq::gather(vec![
                                store,
                                InstrSeq::make_dim(MemberOpMode::Define, mk),
                            ]),
                        }
                    }
                    // both base and index needs locals
                    (
                        ArrayGetBase::Inout {
                            load:
                                ArrayGetBaseData {
                                    mut base_instrs,
                                    cls_instrs,
                                    setup_instrs,
                                    base_stack_size,
                                    cls_stack_size,
                                },
                            store,
                        },
                        Some(local_kind),
                    ) => {
                        let local = e.local_gen_mut().get_unnamed();
                        base_instrs.push((elem_instrs, Some((local.clone(), local_kind))));
                        ArrayGetBase::Inout {
                            load: ArrayGetBaseData {
                                base_instrs,
                                cls_instrs,
                                setup_instrs: make_setup_instrs(setup_instrs),
                                base_stack_size: base_stack_size + elem_stack_size,
                                cls_stack_size,
                            },
                            store: InstrSeq::gather(vec![
                                store,
                                InstrSeq::make_dim(MemberOpMode::Define, MemberKey::EL(local)),
                            ]),
                        }
                    }
                })
            }
        },
        E_::ObjGet(x) => {
            let (base_expr, prop_expr, null_flavor) = &**x;
            Ok(match prop_expr.1.as_id() {
                Some(ast_defs::Id(_, s)) if string_utils::is_xhp(&s) => {
                    let base_instrs = emit_xhp_obj_get(e, env, pos, base_expr, &s, null_flavor)?;
                    emit_default(
                        e,
                        base_instrs,
                        InstrSeq::Empty,
                        InstrSeq::make_basec(base_offset, base_mode),
                        1,
                        0,
                    )
                }
                _ => {
                    let prop_stack_size = emit_prop_expr(
                        e,
                        env,
                        null_flavor,
                        0,
                        prop_expr,
                        null_coalesce_assignment,
                    )?
                    .2;
                    let (
                        base_expr_instrs_begin,
                        base_expr_instrs_end,
                        base_setup_instrs,
                        base_stack_size,
                        cls_stack_size,
                    ) = emit_base(
                        e,
                        env,
                        base_expr,
                        mode,
                        true,
                        null_coalesce_assignment,
                        base_offset + prop_stack_size,
                        rhs_stack_size,
                    )?;
                    let (mk, prop_instrs, _) = emit_prop_expr(
                        e,
                        env,
                        null_flavor,
                        base_offset + cls_stack_size,
                        prop_expr,
                        null_coalesce_assignment,
                    )?;
                    let total_stack_size = prop_stack_size + base_stack_size;
                    let final_instr = InstrSeq::make_dim(mode, mk);
                    emit_default(
                        e,
                        InstrSeq::gather(vec![base_expr_instrs_begin, prop_instrs]),
                        base_expr_instrs_end,
                        InstrSeq::gather(vec![base_setup_instrs, final_instr]),
                        total_stack_size,
                        cls_stack_size,
                    )
                }
            })
        }
        E_::ClassGet(x) => {
            let (cid, prop) = &**x;
            let cexpr = ClassExpr::class_id_to_class_expr(e, false, false, &env.scope, cid);
            let (cexpr_begin, cexpr_end) = emit_class_expr(e, env, cexpr, prop)?;
            Ok(emit_default(
                e,
                cexpr_begin,
                cexpr_end,
                InstrSeq::make_basesc(base_offset + 1, rhs_stack_size, base_mode),
                1,
                1,
            ))
        }
        _ => {
            let base_expr_instrs = emit_expr(e, env, expr)?;
            Ok(emit_default(
                e,
                base_expr_instrs,
                InstrSeq::Empty,
                emit_pos_then(pos, InstrSeq::make_basec(base_offset, base_mode)),
                1,
                0,
            ))
        }
    }
}

// TODO(hrust): change pos from &Pos to Option<&Pos>, since Pos::make_none() still allocate mem.
pub fn emit_ignored_expr(emitter: &mut Emitter, env: &Env, pos: &Pos, expr: &tast::Expr) -> Result {
    if let Some(es) = expr.1.as_expr_list() {
        Ok(InstrSeq::gather(
            es.iter()
                .map(|e| emit_ignored_expr(emitter, env, pos, e))
                .collect::<Result<Vec<_>>>()?,
        ))
    } else {
        Ok(InstrSeq::gather(vec![
            emit_expr(emitter, env, expr)?,
            emit_pos_then(pos, InstrSeq::make_popc()),
        ]))
    }
}

pub fn emit_lval_op(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    op: LValOp,
    expr1: &tast::Expr,
    expr2: Option<&tast::Expr>,
    null_coalesce_assignment: bool,
) -> Result {
    match (op, &expr1.1, expr2) {
        (LValOp::Set, tast::Expr_::List(l), Some(expr2)) => {
            let instr_rhs = emit_expr(e, env, expr2)?;
            let has_elements = l.iter().any(|e| !e.1.is_omitted());
            if !has_elements {
                Ok(instr_rhs)
            } else {
                scope::with_unnamed_local(e, |e, local| {
                    let loc = if can_use_as_rhs_in_list_assignment(&expr2.1)? {
                        Some(local.clone())
                    } else {
                        None
                    };
                    let (instr_lhs, instr_assign) =
                        emit_lval_op_list(e, env, pos, loc, &[], expr1, false)?;
                    Ok((
                        InstrSeq::gather(vec![
                            instr_lhs,
                            instr_rhs,
                            InstrSeq::make_popl(local.clone()),
                        ]),
                        instr_assign,
                        InstrSeq::make_pushl(local),
                    ))
                })
            }
        }
        _ => e.local_scope(|e| {
            let (rhs_instrs, rhs_stack_size) = match expr2 {
                None => (InstrSeq::Empty, 0),
                Some(tast::Expr(_, tast::Expr_::Yield(af))) => {
                    let temp = e.local_gen_mut().get_unnamed();
                    (
                        InstrSeq::gather(vec![
                            emit_yield(e, env, pos, af)?,
                            InstrSeq::make_setl(temp.clone()),
                            InstrSeq::make_popc(),
                            InstrSeq::make_pushl(temp),
                        ]),
                        1,
                    )
                }
                Some(expr) => (emit_expr(e, env, expr)?, 1),
            };
            emit_lval_op_nonlist(
                e,
                env,
                pos,
                op,
                expr1,
                rhs_instrs,
                rhs_stack_size,
                null_coalesce_assignment,
            )
        }),
    }
}

fn can_use_as_rhs_in_list_assignment(expr: &tast::Expr_) -> Result<bool> {
    use aast::Expr_ as E_;
    Ok(match expr {
        E_::Call(c)
            if ((c.1).1)
                .as_id()
                .map_or(false, |id| id.1 == special_functions::ECHO) =>
        {
            false
        }
        E_::Lvar(_)
        | E_::ArrayGet(_)
        | E_::ObjGet(_)
        | E_::ClassGet(_)
        | E_::PUAtom(_)
        | E_::Call(_)
        | E_::FunctionPointer(_)
        | E_::New(_)
        | E_::Record(_)
        | E_::ExprList(_)
        | E_::Yield(_)
        | E_::Cast(_)
        | E_::Eif(_)
        | E_::Array(_)
        | E_::Varray(_)
        | E_::Darray(_)
        | E_::Collection(_)
        | E_::Clone(_)
        | E_::Unop(_)
        | E_::As(_)
        | E_::Await(_) => true,
        E_::Pipe(p) => can_use_as_rhs_in_list_assignment(&(p.2).1)?,
        E_::Binop(b) if b.0.is_eq() => can_use_as_rhs_in_list_assignment(&(b.2).1)?,
        E_::Binop(b) => b.0.is_plus() || b.0.is_question_question() || b.0.is_any_eq(),
        E_::PUIdentifier(_) => {
            return Err(Unrecoverable(
                "TODO(T35357243): Pocket Universes syntax must be erased by now".into(),
            ))
        }
        _ => false,
    })
}

pub fn emit_lval_op_list(
    e: &mut Emitter,
    env: &Env,
    outer_pos: &Pos,
    local: Option<local::Type>,
    indices: &[isize],
    expr: &tast::Expr,
    last_usage: bool,
) -> Result<(InstrSeq, InstrSeq)> {
    unimplemented!()
}

pub fn emit_lval_op_nonlist(
    e: &mut Emitter,
    env: &Env,
    outer_pos: &Pos,
    op: LValOp,
    expr: &tast::Expr,
    rhs_instrs: InstrSeq,
    rhs_stack_size: isize,
    null_coalesce_assignment: bool,
) -> Result {
    emit_lval_op_nonlist_steps(
        e,
        env,
        outer_pos,
        op,
        expr,
        rhs_instrs,
        rhs_stack_size,
        null_coalesce_assignment,
    )
    .map(|(lhs, rhs, setop)| InstrSeq::gather(vec![lhs, rhs, setop]))
}

pub fn emit_final_global_op(pos: &Pos, op: LValOp) -> InstrSeq {
    use LValOp as L;
    match op {
        L::Set => emit_pos_then(pos, InstrSeq::make_setg()),
        L::SetOp(op) => InstrSeq::make_setopg(op),
        L::IncDec(op) => InstrSeq::make_incdecg(op),
        L::Unset => emit_pos_then(pos, InstrSeq::make_unsetg()),
    }
}

pub fn emit_final_local_op(pos: &Pos, op: LValOp, lid: local::Type) -> InstrSeq {
    use LValOp as L;
    emit_pos_then(
        pos,
        match op {
            L::Set => InstrSeq::make_setl(lid),
            L::SetOp(op) => InstrSeq::make_setopl(lid, op),
            L::IncDec(op) => InstrSeq::make_incdecl(lid, op),
            L::Unset => InstrSeq::make_unsetl(lid),
        },
    )
}

fn emit_final_member_op(stack_size: usize, op: LValOp, mk: MemberKey) -> InstrSeq {
    use LValOp as L;
    match op {
        L::Set => InstrSeq::make_setm(stack_size, mk),
        L::SetOp(op) => InstrSeq::make_setopm(stack_size, op, mk),
        L::IncDec(op) => InstrSeq::make_incdecm(stack_size, op, mk),
        L::Unset => InstrSeq::make_unsetm(stack_size, mk),
    }
}

fn emit_final_static_op(
    e: &mut Emitter,
    cid: &tast::ClassId,
    prop: &tast::ClassGetExpr,
    op: LValOp,
) -> Result {
    use LValOp as L;
    Ok(match op {
        L::Set => InstrSeq::make_sets(),
        L::SetOp(op) => InstrSeq::make_setops(op),
        L::IncDec(op) => InstrSeq::make_incdecs(op),
        L::Unset => {
            let pos = match prop {
                tast::ClassGetExpr::CGstring((pos, _))
                | tast::ClassGetExpr::CGexpr(tast::Expr(pos, _)) => pos,
            };
            let cid = text_of_class_id(cid);
            let id = text_of_prop(prop);
            emit_fatal::emit_fatal_runtime(
                pos,
                format!(
                    "Attempt to unset static property {}::{}",
                    string_utils::strip_ns(&cid),
                    id,
                ),
            )
        }
    })
}

pub fn emit_lval_op_nonlist_steps(
    e: &mut Emitter,
    env: &Env,
    outer_pos: &Pos,
    op: LValOp,
    expr: &tast::Expr,
    rhs_instrs: InstrSeq,
    rhs_stack_size: isize,
    null_coalesce_assignment: bool,
) -> Result<(InstrSeq, InstrSeq, InstrSeq)> {
    let f = |env: &mut Env| {
        use tast::Expr_ as E_;
        let pos = &expr.0;
        Ok(match &expr.1 {
            E_::Lvar(v) if superglobals::is_any_global(local_id::get_name(&v.1)) => (
                emit_pos_then(
                    &v.0,
                    InstrSeq::make_string(string_utils::lstrip(local_id::get_name(&v.1), "$")),
                ),
                rhs_instrs,
                emit_final_global_op(outer_pos, op),
            ),
            E_::Lvar(v) if is_local_this(env, &v.1) && op.is_incdec() => (
                emit_local(e, env, BareThisOp::Notice, v)?,
                rhs_instrs,
                InstrSeq::Empty,
            ),
            E_::Lvar(v) if !is_local_this(env, &v.1) || op == LValOp::Unset => {
                (InstrSeq::Empty, rhs_instrs, {
                    let lid = get_local(e, env, &v.0, &(v.1).1)?;
                    emit_final_local_op(outer_pos, op, lid)
                })
            }
            E_::ArrayGet(x) => match (&(x.0).1, x.1.as_ref()) {
                (E_::Lvar(v), Some(expr)) if local_id::get_name(&v.1) == superglobals::GLOBALS => {
                    let final_global_op_instrs = emit_final_global_op(pos, op);
                    if rhs_stack_size == 0 {
                        (
                            emit_expr(e, env, expr)?,
                            InstrSeq::Empty,
                            final_global_op_instrs,
                        )
                    } else {
                        let (index_instrs, under_top) = emit_first_expr(e, env, expr)?;
                        if under_top {
                            (
                                InstrSeq::Empty,
                                InstrSeq::gather(vec![rhs_instrs, index_instrs]),
                                final_global_op_instrs,
                            )
                        } else {
                            (index_instrs, rhs_instrs, final_global_op_instrs)
                        }
                    }
                }
                (_, None) if !env.flags.contains(env::Flags::ALLOWS_ARRAY_APPEND) => {
                    return Err(emit_fatal::raise_fatal_runtime(
                        pos,
                        "Can't use [] for reading",
                    ))
                }
                (_, opt_elem_expr) => {
                    let mode = match op {
                        LValOp::Unset => MemberOpMode::Unset,
                        _ => MemberOpMode::Define,
                    };
                    let (mut elem_instrs, elem_stack_size) =
                        emit_elem(e, env, opt_elem_expr, None, null_coalesce_assignment)?;
                    if null_coalesce_assignment {
                        elem_instrs = InstrSeq::Empty;
                    }
                    let base_offset = elem_stack_size + rhs_stack_size;
                    let (
                        base_expr_instrs_begin,
                        base_expr_instrs_end,
                        base_setup_instrs,
                        base_stack_size,
                        cls_stack_size,
                    ) = emit_base(
                        e,
                        env,
                        &x.0,
                        mode,
                        false,
                        null_coalesce_assignment,
                        base_offset,
                        rhs_stack_size,
                    )?;
                    let mk = get_elem_member_key(
                        e,
                        env,
                        rhs_stack_size + cls_stack_size,
                        opt_elem_expr,
                        null_coalesce_assignment,
                    )?;
                    let total_stack_size = elem_stack_size + base_stack_size + cls_stack_size;
                    let final_instr =
                        emit_pos_then(pos, emit_final_member_op(total_stack_size as usize, op, mk));
                    (
                        if null_coalesce_assignment {
                            elem_instrs
                        } else {
                            InstrSeq::gather(vec![
                                base_expr_instrs_begin,
                                elem_instrs,
                                base_expr_instrs_end,
                            ])
                        },
                        rhs_instrs,
                        InstrSeq::gather(vec![emit_pos(pos), base_setup_instrs, final_instr]),
                    )
                }
            },
            E_::ObjGet(x) => {
                let (e1, e2, nullflavor) = &**x;
                if nullflavor.eq(&ast_defs::OgNullFlavor::OGNullsafe) {
                    return Err(emit_fatal::raise_fatal_parse(
                        pos,
                        "?-> is not allowed in write context",
                    ));
                }
                let mode = match op {
                    LValOp::Unset => MemberOpMode::Unset,
                    _ => MemberOpMode::Define,
                };
                let prop_stack_size =
                    emit_prop_expr(e, env, nullflavor, 0, e2, null_coalesce_assignment)?.2;
                let base_offset = prop_stack_size + rhs_stack_size;
                let (
                    base_expr_instrs_begin,
                    base_expr_instrs_end,
                    base_setup_instrs,
                    base_stack_size,
                    cls_stack_size,
                ) = emit_base(
                    e,
                    env,
                    e1,
                    mode,
                    true,
                    null_coalesce_assignment,
                    base_offset,
                    rhs_stack_size,
                )?;
                let (mk, mut prop_instrs, _) = emit_prop_expr(
                    e,
                    env,
                    nullflavor,
                    rhs_stack_size + cls_stack_size,
                    e2,
                    null_coalesce_assignment,
                )?;
                if null_coalesce_assignment {
                    prop_instrs = InstrSeq::Empty;
                }
                let total_stack_size = prop_stack_size + base_stack_size + cls_stack_size;
                let final_instr =
                    emit_pos_then(pos, emit_final_member_op(total_stack_size as usize, op, mk));
                (
                    if null_coalesce_assignment {
                        prop_instrs
                    } else {
                        InstrSeq::gather(vec![
                            base_expr_instrs_begin,
                            prop_instrs,
                            base_expr_instrs_end,
                        ])
                    },
                    rhs_instrs,
                    InstrSeq::gather(vec![base_setup_instrs, final_instr]),
                )
            }
            E_::ClassGet(x) => {
                let (cid, prop) = &**x;
                let cexpr = ClassExpr::class_id_to_class_expr(e, false, false, &env.scope, cid);
                let final_instr_ = emit_final_static_op(e, cid, prop, op)?;
                let final_instr = emit_pos_then(pos, final_instr_);
                (
                    InstrSeq::of_pair(emit_class_expr(e, env, cexpr, prop)?),
                    rhs_instrs,
                    final_instr,
                )
            }
            E_::Unop(uop) => (
                InstrSeq::Empty,
                rhs_instrs,
                InstrSeq::gather(vec![
                    emit_lval_op_nonlist(
                        e,
                        env,
                        pos,
                        op,
                        &uop.1,
                        InstrSeq::Empty,
                        rhs_stack_size,
                        false,
                    )?,
                    from_unop(e.options(), &uop.0)?,
                ]),
            ),
            _ => {
                return Err(emit_fatal::raise_fatal_parse(
                    pos,
                    "Can't use return value in write context",
                ))
            }
        })
    };
    // TODO(shiqicao): remove clone!
    let mut env = env.clone();
    match op {
        LValOp::Set | LValOp::SetOp(_) | LValOp::IncDec(_) => env.with_allows_array_append(f),
        _ => f(&mut env),
    }
}

fn emit_class_expr(
    e: &mut Emitter,
    env: &Env,
    cexpr: ClassExpr,
    prop: &tast::ClassGetExpr,
) -> Result<(InstrSeq, InstrSeq)> {
    let load_prop = |e: &mut Emitter| match prop {
        tast::ClassGetExpr::CGstring((pos, id)) => Ok(emit_pos_then(
            pos,
            InstrSeq::make_string(string_utils::locals::strip_dollar(id)),
        )),
        tast::ClassGetExpr::CGexpr(expr) => emit_expr(e, env, expr),
    };
    Ok(match &cexpr {
        ClassExpr::Expr(expr)
            if expr.1.is_braced_expr()
                || expr.1.is_call()
                || expr.1.is_binop()
                || expr.1.is_class_get()
                || expr
                    .1
                    .as_lvar()
                    .map_or(false, |tast::Lid(_, id)| local_id::get_name(id) == "$this") =>
        {
            let cexpr_local = emit_expr(e, env, expr)?;
            (
                InstrSeq::Empty,
                InstrSeq::gather(vec![
                    cexpr_local,
                    scope::stash_top_in_unnamed_local(e, load_prop)?,
                    InstrSeq::make_classgetc(),
                ]),
            )
        }
        _ => {
            let pos = match prop {
                tast::ClassGetExpr::CGstring((pos, _))
                | tast::ClassGetExpr::CGexpr(tast::Expr(pos, _)) => pos,
            };
            (load_prop(e)?, emit_load_class_ref(e, env, pos, cexpr)?)
        }
    })
}

pub fn fixup_type_arg<'a>(
    env: &Env,
    isas: bool,
    hint: &'a tast::Hint,
) -> Result<impl AsRef<tast::Hint> + 'a> {
    struct Checker<'s> {
        erased_tparams: &'s [&'s str],
        isas: bool,
    };
    impl<'s> Visitor for Checker<'s> {
        type P = AstParams<(), Option<Error>>;

        fn object(&mut self) -> &mut dyn Visitor<P = Self::P> {
            self
        }

        fn visit_hint_fun(
            &mut self,
            c: &mut (),
            hf: &tast::HintFun,
        ) -> StdResult<(), Option<Error>> {
            hf.param_tys.accept(c, self.object())?;
            hf.return_ty.accept(c, self.object())
        }

        fn visit_hint(&mut self, c: &mut (), h: &tast::Hint) -> StdResult<(), Option<Error>> {
            use tast::{Hint_ as H_, Id};
            match h.1.as_ref() {
                H_::Happly(Id(_, id), _)
                    if self.erased_tparams.contains(&id.as_str()) && self.isas =>
                {
                    return Err(Some(emit_fatal::raise_fatal_parse(
                        &h.0,
                        "Erased generics are not allowd in is/as expressions",
                    )))
                }
                _ => (),
            }
            h.recurse(c, self.object())
        }

        fn visit_hint_(&mut self, c: &mut (), h: &tast::Hint_) -> StdResult<(), Option<Error>> {
            use tast::{Hint_ as H_, Id};
            match h {
                H_::Happly(Id(_, id), _) if self.erased_tparams.contains(&id.as_str()) => Err(None),
                _ => h.recurse(c, self.object()),
            }
        }
    }

    struct Updater<'s> {
        erased_tparams: &'s [&'s str],
    }
    impl<'s> VisitorMut for Updater<'s> {
        type P = AstParams<(), ()>;

        fn object(&mut self) -> &mut dyn VisitorMut<P = Self::P> {
            self
        }

        fn visit_hint_fun(&mut self, c: &mut (), hf: &mut tast::HintFun) -> StdResult<(), ()> {
            <Vec<tast::Hint> as NodeMut<Self::P>>::accept(&mut hf.param_tys, c, self.object())?;
            <tast::Hint as NodeMut<Self::P>>::accept(&mut hf.return_ty, c, self.object())
        }

        fn visit_hint_(&mut self, c: &mut (), h: &mut tast::Hint_) -> StdResult<(), ()> {
            use tast::{Hint_ as H_, Id};
            match h {
                H_::Happly(Id(_, id), _) if self.erased_tparams.contains(&id.as_str()) => {
                    Ok(*id = "_".into())
                }
                _ => h.recurse(c, self.object()),
            }
        }
    }
    let erased_tparams = get_erased_tparams(env);
    let erased_tparams = erased_tparams.as_slice();
    let mut checker = Checker {
        erased_tparams,
        isas,
    };
    match visit(&mut checker, &mut (), hint) {
        Ok(()) => Ok(Either::Left(hint)),
        Err(Some(error)) => Err(error),
        Err(None) => {
            let mut updater = Updater { erased_tparams };
            let mut hint = hint.clone();
            visit_mut(&mut updater, &mut (), &mut hint).unwrap();
            Ok(Either::Right(hint))
        }
    }
}

pub fn emit_reified_arg(
    e: &mut Emitter,
    env: &Env,
    pos: &Pos,
    isas: bool,
    hint: &tast::Hint,
) -> Result<(InstrSeq, bool)> {
    struct Collector<'a> {
        current_tags: &'a HashSet<&'a str>,
        // TODO(hrust): acc should be typed to (usize, HashMap<'str, usize>)
        // which avoids allocation. This currently isn't possible since visitor need to expose
        // lifttime of nodes, for example,
        // `fn visit_hint_(..., h_: &'a tast::Hint_) -> ... {}`
        acc: IndexSet<String>,
    }

    impl<'a> Collector<'a> {
        fn add_name(&mut self, name: &str) {
            if self.current_tags.contains(name) && !self.acc.contains(name) {
                self.acc.insert(name.into());
            }
        }
    }

    impl<'a> Visitor for Collector<'a> {
        type P = AstParams<(), ()>;

        fn object(&mut self) -> &mut dyn Visitor<P = Self::P> {
            self
        }

        fn visit_hint_(&mut self, c: &mut (), h_: &tast::Hint_) -> StdResult<(), ()> {
            use tast::{Hint_ as H_, Id};
            match h_ {
                H_::Haccess(_, sids) => Ok(sids.iter().for_each(|Id(_, name)| self.add_name(name))),
                H_::Happly(Id(_, name), h) => {
                    self.add_name(name);
                    h.accept(c, self.object())
                }
                H_::Habstr(name) => Ok(self.add_name(name)),
                _ => h_.recurse(c, self.object()),
            }
        }
    }
    let hint = fixup_type_arg(env, isas, hint)?;
    let hint = hint.as_ref();
    fn f<'a>(mut acc: HashSet<&'a str>, tparam: &'a tast::Tparam) -> HashSet<&'a str> {
        if tparam.reified != tast::ReifyKind::Erased {
            acc.insert(&tparam.name.1);
        }
        acc
    }
    let current_tags = env
        .scope
        .get_fun_tparams()
        .iter()
        .fold(HashSet::<&str>::new(), |acc, t| f(acc, &*t));
    let class_tparams = env.scope.get_class_tparams();
    let current_tags = class_tparams
        .list
        .iter()
        .fold(current_tags, |acc, t| f(acc, &*t));

    let mut collector = Collector {
        current_tags: &current_tags,
        acc: IndexSet::new(),
    };
    visit(&mut collector, &mut (), hint).unwrap();
    match hint.1.as_ref() {
        tast::Hint_::Happly(tast::Id(_, name), hs)
            if hs.is_empty() && current_tags.contains(name.as_str()) =>
        {
            Ok((emit_reified_type(e, env, pos, name)?, false))
        }
        _ => {
            let ts = get_type_structure_for_hint(e, &[], &collector.acc, hint)?;
            let ts_list = if collector.acc.is_empty() {
                ts
            } else {
                let values = collector
                    .acc
                    .iter()
                    .map(|v| emit_reified_type(e, env, pos, v))
                    .collect::<Result<Vec<_>>>()?;
                InstrSeq::gather(vec![InstrSeq::gather(values), ts])
            };
            Ok((
                InstrSeq::gather(vec![
                    ts_list,
                    InstrSeq::make_combine_and_resolve_type_struct(
                        (collector.acc.len() + 1) as isize,
                    ),
                ]),
                collector.acc.is_empty(),
            ))
        }
    }
}

pub fn get_local(e: &mut Emitter, env: &Env, pos: &Pos, s: &str) -> Result<local::Type> {
    if s == special_idents::DOLLAR_DOLLAR {
        unimplemented!()
    } else if special_idents::is_tmp_var(s) {
        Ok(e.local_gen().get_unnamed_for_tempname(s).clone())
    } else {
        Ok(local::Type::Named(s.into()))
    }
}

pub fn emit_is_null(e: &mut Emitter, env: &Env, expr: &tast::Expr) -> Result {
    if let Some(tast::Lid(pos, id)) = expr.1.as_lvar() {
        if !is_local_this(env, id) {
            return Ok(InstrSeq::make_istypel(
                get_local(e, env, pos, local_id::get_name(id))?,
                IstypeOp::OpNull,
            ));
        }
    }

    Ok(InstrSeq::gather(vec![
        emit_expr(e, env, expr)?,
        InstrSeq::make_istypec(IstypeOp::OpNull),
    ]))
}

pub fn emit_jmpnz(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    label: &Label,
) -> Result<EmitJmpResult> {
    let tast::Expr(pos, expr_) = expr;
    let opt = optimize_null_checks(e);
    Ok(
        match ast_constant_folder::expr_to_typed_value(e, &env.namespace, expr) {
            Ok(tv) => {
                if Into::<bool>::into(tv) {
                    EmitJmpResult {
                        instrs: emit_pos_then(pos, InstrSeq::make_jmp(label.clone())),
                        is_fallthrough: false,
                        is_label_used: true,
                    }
                } else {
                    EmitJmpResult {
                        instrs: emit_pos_then(pos, InstrSeq::Empty),
                        is_fallthrough: true,
                        is_label_used: false,
                    }
                }
            }
            Err(_) => {
                use {ast_defs::Uop as U, tast::Expr_ as E};
                match expr_ {
                    E::Unop(uo) if uo.0 == U::Unot => emit_jmpz(e, env, &uo.1, label)?,
                    E::Binop(bo) if bo.0.is_barbar() => {
                        let r1 = emit_jmpnz(e, env, &bo.1, label)?;
                        if r1.is_fallthrough {
                            let r2 = emit_jmpnz(e, env, &bo.2, label)?;
                            EmitJmpResult {
                                instrs: emit_pos_then(
                                    pos,
                                    InstrSeq::gather(vec![r1.instrs, r2.instrs]),
                                ),
                                is_fallthrough: r2.is_fallthrough,
                                is_label_used: r1.is_label_used || r2.is_label_used,
                            }
                        } else {
                            r1
                        }
                    }
                    E::Binop(bo) if bo.0.is_ampamp() => {
                        let skip_label = e.label_gen_mut().next_regular();
                        let r1 = emit_jmpz(e, env, &bo.1, &skip_label)?;
                        if !r1.is_fallthrough {
                            EmitJmpResult {
                                instrs: emit_pos_then(
                                    pos,
                                    InstrSeq::gather(vec![
                                        r1.instrs,
                                        InstrSeq::optional(
                                            r1.is_label_used,
                                            vec![InstrSeq::make_label(skip_label)],
                                        ),
                                    ]),
                                ),
                                is_fallthrough: r1.is_label_used,
                                is_label_used: false,
                            }
                        } else {
                            let r2 = emit_jmpnz(e, env, &bo.2, label)?;
                            EmitJmpResult {
                                instrs: emit_pos_then(
                                    pos,
                                    InstrSeq::gather(vec![
                                        r1.instrs,
                                        r2.instrs,
                                        InstrSeq::optional(
                                            r1.is_label_used,
                                            vec![InstrSeq::make_label(skip_label)],
                                        ),
                                    ]),
                                ),
                                is_fallthrough: r2.is_fallthrough || r1.is_label_used,
                                is_label_used: r2.is_label_used,
                            }
                        }
                    }
                    E::Binop(bo)
                        if bo.0.is_eqeqeq()
                            && ((bo.1).1.is_null() || (bo.2).1.is_null())
                            && opt =>
                    {
                        let is_null =
                            emit_is_null(e, env, if (bo.1).1.is_null() { &bo.2 } else { &bo.1 })?;
                        EmitJmpResult {
                            instrs: emit_pos_then(
                                pos,
                                InstrSeq::gather(vec![
                                    is_null,
                                    InstrSeq::make_jmpnz(label.clone()),
                                ]),
                            ),
                            is_fallthrough: true,
                            is_label_used: true,
                        }
                    }
                    E::Binop(bo)
                        if bo.0.is_diff2() && ((bo.1).1.is_null() || (bo.2).1.is_null()) && opt =>
                    {
                        let is_null =
                            emit_is_null(e, env, if (bo.1).1.is_null() { &bo.2 } else { &bo.1 })?;
                        EmitJmpResult {
                            instrs: emit_pos_then(
                                pos,
                                InstrSeq::gather(vec![is_null, InstrSeq::make_jmpz(label.clone())]),
                            ),
                            is_fallthrough: true,
                            is_label_used: true,
                        }
                    }
                    _ => {
                        let instr = emit_expr(e, env, expr)?;
                        EmitJmpResult {
                            instrs: emit_pos_then(
                                pos,
                                InstrSeq::gather(vec![instr, InstrSeq::make_jmpnz(label.clone())]),
                            ),
                            is_fallthrough: true,
                            is_label_used: true,
                        }
                    }
                }
            }
        },
    )
}

pub fn emit_jmpz(
    e: &mut Emitter,
    env: &Env,
    expr: &tast::Expr,
    label: &Label,
) -> Result<EmitJmpResult> {
    let tast::Expr(pos, expr_) = expr;
    let opt = optimize_null_checks(e);
    Ok(
        match ast_constant_folder::expr_to_typed_value(e, &env.namespace, expr) {
            Ok(v) => {
                let b: bool = v.into();
                if b {
                    EmitJmpResult {
                        instrs: emit_pos_then(pos, InstrSeq::Empty),
                        is_fallthrough: true,
                        is_label_used: false,
                    }
                } else {
                    EmitJmpResult {
                        instrs: emit_pos_then(pos, InstrSeq::make_jmp(label.clone())),
                        is_fallthrough: false,
                        is_label_used: true,
                    }
                }
            }
            Err(_) => {
                use {ast_defs::Uop as U, tast::Expr_ as E};
                match expr_ {
                    E::Unop(uo) if uo.0 == U::Unot => emit_jmpnz(e, env, &uo.1, label)?,
                    E::Binop(bo) if bo.0.is_barbar() => {
                        let skip_label = e.label_gen_mut().next_regular();
                        let r1 = emit_jmpnz(e, env, &bo.1, &skip_label)?;
                        if !r1.is_fallthrough {
                            EmitJmpResult {
                                instrs: emit_pos_then(
                                    pos,
                                    InstrSeq::gather(vec![
                                        r1.instrs,
                                        InstrSeq::optional(
                                            r1.is_label_used,
                                            vec![InstrSeq::make_label(skip_label)],
                                        ),
                                    ]),
                                ),
                                is_fallthrough: r1.is_label_used,
                                is_label_used: false,
                            }
                        } else {
                            let r2 = emit_jmpz(e, env, &bo.2, label)?;
                            EmitJmpResult {
                                instrs: emit_pos_then(
                                    pos,
                                    InstrSeq::gather(vec![
                                        r1.instrs,
                                        r2.instrs,
                                        InstrSeq::optional(
                                            r1.is_label_used,
                                            vec![InstrSeq::make_label(skip_label)],
                                        ),
                                    ]),
                                ),
                                is_fallthrough: r2.is_fallthrough || r1.is_label_used,
                                is_label_used: r2.is_label_used,
                            }
                        }
                    }
                    E::Binop(bo) if bo.0.is_ampamp() => {
                        let r1 = emit_jmpz(e, env, &bo.1, label)?;
                        if r1.is_fallthrough {
                            let r2 = emit_jmpz(e, env, &bo.2, label)?;
                            EmitJmpResult {
                                instrs: emit_pos_then(
                                    pos,
                                    InstrSeq::gather(vec![r1.instrs, r2.instrs]),
                                ),
                                is_fallthrough: r2.is_fallthrough,
                                is_label_used: r1.is_label_used || r2.is_label_used,
                            }
                        } else {
                            EmitJmpResult {
                                instrs: emit_pos_then(pos, r1.instrs),
                                is_fallthrough: false,
                                is_label_used: r1.is_label_used,
                            }
                        }
                    }
                    E::Binop(bo)
                        if bo.0.is_eqeqeq()
                            && ((bo.1).1.is_null() || (bo.2).1.is_null())
                            && opt =>
                    {
                        let is_null =
                            emit_is_null(e, env, if (bo.1).1.is_null() { &bo.2 } else { &bo.1 })?;
                        EmitJmpResult {
                            instrs: emit_pos_then(
                                pos,
                                InstrSeq::gather(vec![is_null, InstrSeq::make_jmpz(label.clone())]),
                            ),
                            is_fallthrough: true,
                            is_label_used: true,
                        }
                    }
                    E::Binop(bo)
                        if bo.0.is_diff2() && ((bo.1).1.is_null() || (bo.2).1.is_null()) && opt =>
                    {
                        let is_null =
                            emit_is_null(e, env, if (bo.1).1.is_null() { &bo.2 } else { &bo.1 })?;
                        EmitJmpResult {
                            instrs: emit_pos_then(
                                pos,
                                InstrSeq::gather(vec![
                                    is_null,
                                    InstrSeq::make_jmpnz(label.clone()),
                                ]),
                            ),
                            is_fallthrough: true,
                            is_label_used: true,
                        }
                    }
                    _ => {
                        let instr = emit_expr(e, env, expr)?;
                        EmitJmpResult {
                            instrs: emit_pos_then(
                                pos,
                                InstrSeq::gather(vec![instr, InstrSeq::make_jmpz(label.clone())]),
                            ),
                            is_fallthrough: true,
                            is_label_used: true,
                        }
                    }
                }
            }
        },
    )
}
