// Copyright (c) 2019, Facebook, Inc.
// All rights reserved.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.

use crate::{desugar_expression_tree::desugar, modifier};
use bstr::{BStr, BString, ByteSlice, B};
use bumpalo::Bump;
use escaper::*;
use hh_autoimport_rust as hh_autoimport;
use itertools::Either;
use lint_rust::LintError;
use naming_special_names_rust::{
    classes as special_classes, literal, rx, special_functions, special_idents,
    typehints as special_typehints, user_attributes as special_attrs,
};
use ocamlrep::rc::RcOc;
use oxidized::{
    aast,
    aast_visitor::{AstParams, Node, Visitor},
    ast,
    ast::Expr_ as E_,
    doc_comment::DocComment,
    errors::{Error as HHError, Naming, NastCheck},
    file_info,
    global_options::GlobalOptions,
    namespace_env::Env as NamespaceEnv,
    pos::Pos,
    s_map,
};
use parser_core_types::{
    indexed_source_text::IndexedSourceText,
    lexable_token::LexablePositionedToken,
    source_text::SourceText,
    syntax::{SyntaxValueType, SyntaxValueWithKind},
    syntax_by_ref::{
        positioned_token::{PositionedToken, TokenFactory as PositionedTokenFactory},
        positioned_value::PositionedValue,
        syntax::Syntax,
        syntax_variant_generated::{SyntaxVariant::*, *},
    },
    syntax_error, syntax_kind,
    syntax_trait::SyntaxTrait,
    token_factory::TokenMutator,
    token_kind::TokenKind as TK,
};
use regex::bytes::Regex;
use stack_limit::StackLimit;
use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashSet,
    matches, mem,
    rc::Rc,
    slice::Iter,
};

fn unescape_single(s: &str) -> std::result::Result<BString, escaper::InvalidString> {
    Ok(escaper::unescape_single(s)?.into())
}

fn unescape_nowdoc(s: &str) -> std::result::Result<BString, escaper::InvalidString> {
    Ok(escaper::unescape_nowdoc(s)?.into())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LiftedAwaitKind {
    LiftedFromStatement,
    LiftedFromConcurrent,
}

type LiftedAwaitExprs = Vec<(Option<ast::Lid>, ast::Expr)>;

#[derive(Debug, Clone)]
pub struct LiftedAwaits {
    pub awaits: LiftedAwaitExprs,
    lift_kind: LiftedAwaitKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ExprLocation {
    TopLevel,
    MemberSelect,
    InDoubleQuotedString,
    AsStatement,
    RightOfAssignment,
    RightOfAssignmentInUsingStatement,
    RightOfReturn,
    UsingStatement,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SuspensionKind {
    SKSync,
    SKAsync,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TokenOp {
    Skip,
    Noop,
    LeftTrim(usize),
    RightTrim(usize),
}

#[derive(Debug)]
pub struct FunHdr {
    suspension_kind: SuspensionKind,
    name: ast::Sid,
    constrs: Vec<ast::WhereConstraintHint>,
    type_parameters: Vec<ast::Tparam>,
    parameters: Vec<ast::FunParam>,
    contexts: Option<ast::Contexts>,
    unsafe_contexts: Option<ast::Contexts>,
    return_type: Option<ast::Hint>,
}

impl FunHdr {
    fn make_empty<TF: Clone>(env: &Env<TF>) -> Self {
        Self {
            suspension_kind: SuspensionKind::SKSync,
            name: ast::Id(env.mk_none_pos(), String::from("<ANONYMOUS>")),
            constrs: vec![],
            type_parameters: vec![],
            parameters: vec![],
            contexts: None,
            unsafe_contexts: None,
            return_type: None,
        }
    }
}

#[derive(Debug)]
pub struct State {
    pub cls_reified_generics: HashSet<String>,
    pub in_static_method: bool,
    pub parent_maybe_reified: bool,
    /// This provides a generic mechanism to delay raising parsing errors;
    /// since we're moving FFP errors away from CST to a stage after lowering
    /// _and_ want to prioritize errors before lowering, the lowering errors
    /// must be merely stored when the lowerer runs (until check for FFP runs (on AST)
    /// and raised _after_ FFP error checking (unless we run the lowerer twice,
    /// which would be expensive).
    pub lowpri_errors: Vec<(Pos, String)>,
    /// hh_errors captures errors after parsing, naming, nast, etc.
    pub hh_errors: Vec<HHError>,
    pub lint_errors: Vec<LintError>,
    pub doc_comments: Vec<Option<DocComment>>,

    pub local_id_counter: isize,

    // TODO(hrust): this check is to avoid crash in Ocaml.
    // Remove it after all Ocaml callers are eliminated.
    pub exp_recursion_depth: usize,
}

const EXP_RECUSION_LIMIT: usize = 30_000;

#[derive(Debug, Clone)]
pub struct Env<'a, TF> {
    pub codegen: bool,
    pub keep_errors: bool,
    quick_mode: bool,
    /// Show errors even in quick mode. Does not override keep_errors. Hotfix
    /// until we can properly set up saved states to surface parse errors during
    /// typechecking properly.
    pub show_all_errors: bool,
    fail_open: bool,
    file_mode: file_info::Mode,
    disable_lowering_parsing_error: bool,
    pub top_level_statements: bool, /* Whether we are (still) considering TLSs*/

    // Cache none pos, lazy_static doesn't allow Rc.
    pos_none: Pos,
    pub empty_ns_env: RcOc<NamespaceEnv>,

    pub saw_yield: bool, /* Information flowing back up */
    pub lifted_awaits: Option<LiftedAwaits>,
    pub tmp_var_counter: isize,

    pub indexed_source_text: &'a IndexedSourceText<'a>,
    pub parser_options: &'a GlobalOptions,
    pub stack_limit: Option<&'a StackLimit>,

    pub token_factory: TF,
    pub arena: &'a Bump,

    state: Rc<RefCell<State>>,
}

impl<'a, TF: Clone> Env<'a, TF> {
    pub fn make(
        codegen: bool,
        quick_mode: bool,
        keep_errors: bool,
        show_all_errors: bool,
        fail_open: bool,
        disable_lowering_parsing_error: bool,
        mode: file_info::Mode,
        indexed_source_text: &'a IndexedSourceText<'a>,
        parser_options: &'a GlobalOptions,
        use_default_namespace: bool,
        stack_limit: Option<&'a StackLimit>,
        token_factory: TF,
        arena: &'a Bump,
    ) -> Self {
        // (hrust) Ported from namespace_env.ml
        let empty_ns_env = if use_default_namespace {
            let mut nsenv = NamespaceEnv::empty(vec![], false, false);
            nsenv.is_codegen = codegen;
            nsenv.disable_xhp_element_mangling = parser_options.po_disable_xhp_element_mangling;
            nsenv
        } else {
            let mut ns_uses = hh_autoimport::NAMESPACES_MAP.clone();
            parser_options
                .po_auto_namespace_map
                .iter()
                .for_each(|(k, v)| {
                    &ns_uses.insert(k.into(), v.into());
                });
            NamespaceEnv {
                ns_uses,
                class_uses: hh_autoimport::TYPES_MAP.clone(),
                fun_uses: hh_autoimport::FUNCS_MAP.clone(),
                const_uses: hh_autoimport::CONSTS_MAP.clone(),
                record_def_uses: s_map::SMap::new(),
                name: None,
                auto_ns_map: parser_options.po_auto_namespace_map.clone(),
                is_codegen: codegen,
                disable_xhp_element_mangling: parser_options.po_disable_xhp_element_mangling,
            }
        };

        Env {
            codegen,
            keep_errors,
            quick_mode,
            show_all_errors,
            fail_open,
            disable_lowering_parsing_error,
            file_mode: mode,
            top_level_statements: true,
            saw_yield: false,
            lifted_awaits: None,
            tmp_var_counter: 1,
            indexed_source_text,
            parser_options,
            pos_none: Pos::make_none(),
            empty_ns_env: RcOc::new(empty_ns_env),
            stack_limit,
            token_factory,
            arena,

            state: Rc::new(RefCell::new(State {
                cls_reified_generics: HashSet::new(),
                in_static_method: false,
                parent_maybe_reified: false,
                lowpri_errors: vec![],
                doc_comments: vec![],
                local_id_counter: 1,
                hh_errors: vec![],
                lint_errors: vec![],
                exp_recursion_depth: 0,
            })),
        }
    }

    fn file_mode(&self) -> file_info::Mode {
        self.file_mode
    }

    fn should_surface_error(&self) -> bool {
        (!self.quick_mode || self.show_all_errors) && self.keep_errors
    }

    fn is_typechecker(&self) -> bool {
        !self.codegen
    }

    fn codegen(&self) -> bool {
        self.codegen
    }

    fn source_text(&self) -> &SourceText<'a> {
        self.indexed_source_text.source_text()
    }

    fn fail_open(&self) -> bool {
        self.fail_open
    }

    fn cls_reified_generics(&mut self) -> RefMut<HashSet<String>> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.cls_reified_generics)
    }

    fn in_static_method(&mut self) -> RefMut<bool> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.in_static_method)
    }

    fn parent_maybe_reified(&mut self) -> RefMut<bool> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.parent_maybe_reified)
    }

    pub fn lowpri_errors(&mut self) -> RefMut<Vec<(Pos, String)>> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.lowpri_errors)
    }

    pub fn hh_errors(&mut self) -> RefMut<Vec<HHError>> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.hh_errors)
    }

    pub fn lint_errors(&mut self) -> RefMut<Vec<LintError>> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.lint_errors)
    }

    fn top_docblock(&self) -> Ref<Option<DocComment>> {
        Ref::map(self.state.borrow(), |s| {
            s.doc_comments.last().unwrap_or(&None)
        })
    }

    fn exp_recursion_depth(&self) -> RefMut<usize> {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.exp_recursion_depth)
    }

    fn next_local_id(&self) -> isize {
        let mut id = RefMut::map(self.state.borrow_mut(), |s| &mut s.local_id_counter);
        *id = *id + 1;
        *id
    }

    fn push_docblock(&mut self, doc_comment: Option<DocComment>) {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.doc_comments).push(doc_comment)
    }

    fn pop_docblock(&mut self) {
        RefMut::map(self.state.borrow_mut(), |s| &mut s.doc_comments).pop();
    }

    fn make_tmp_var_name(&mut self) -> String {
        let name = String::from(special_idents::TMP_VAR_PREFIX) + &self.tmp_var_counter.to_string();
        self.tmp_var_counter += 1;
        name
    }

    fn mk_none_pos(&self) -> Pos {
        self.pos_none.clone()
    }

    fn clone_and_unset_toplevel_if_toplevel<'b, 'c>(
        e: &'b mut Env<'c, TF>,
    ) -> impl AsMut<Env<'c, TF>> + 'b {
        if e.top_level_statements {
            let mut cloned = e.clone();
            cloned.top_level_statements = false;
            Either::Left(cloned)
        } else {
            Either::Right(e)
        }
    }

    fn check_stack_limit(&self) {
        self.stack_limit
            .as_ref()
            .map(|limit| limit.panic_if_exceeded());
    }
}

impl<'a, TF> AsMut<Env<'a, TF>> for Env<'a, TF> {
    fn as_mut(&mut self) -> &mut Env<'a, TF> {
        self
    }
}

#[derive(Debug)]
pub enum Error {
    MissingSyntax {
        expecting: String,
        pos: Pos,
        node_name: String,
        kind: syntax_kind::SyntaxKind,
    },
    Failwith(String),
}

type Result<T> = std::result::Result<T, Error>;
type S<'arena, T, V> = &'arena Syntax<'arena, T, V>;

trait Lowerer<'a, T, V, TF>
where
    TF: TokenMutator<Token = T> + Clone + 'a,
    T: 'a + LexablePositionedToken + Copy,
    V: 'a + SyntaxValueWithKind + SyntaxValueType<T>,
    Syntax<'a, T, V>: SyntaxTrait,
{
    fn p_pos(node: S<'a, T, V>, env: &Env<TF>) -> Pos {
        node.position_exclusive(env.indexed_source_text)
            .unwrap_or_else(|| env.mk_none_pos())
    }

    fn raise_parsing_error(node: S<'a, T, V>, env: &mut Env<'a, TF>, msg: &str) {
        Self::raise_parsing_error_(Either::Left(node), env, msg)
    }

    fn raise_parsing_error_pos(pos: &Pos, env: &mut Env<'a, TF>, msg: &str) {
        Self::raise_parsing_error_(Either::Right(pos), env, msg)
    }

    fn raise_parsing_error_(
        node_or_pos: Either<S<'a, T, V>, &Pos>,
        env: &mut Env<'a, TF>,
        msg: &str,
    ) {
        if env.should_surface_error() {
            let pos = node_or_pos.either(|node| Self::p_pos(node, env), |pos| pos.clone());
            env.lowpri_errors().push((pos, String::from(msg)))
        } else if env.codegen() {
            let pos = node_or_pos.either(
                |node| {
                    node.position_exclusive(env.indexed_source_text)
                        .unwrap_or_else(|| env.mk_none_pos())
                },
                |pos| pos.clone(),
            );
            env.lowpri_errors().push((pos, String::from(msg)))
        }
    }

    fn raise_hh_error(env: &mut Env<'a, TF>, err: HHError) {
        env.hh_errors().push(err);
    }

    fn raise_lint_error(env: &mut Env<'a, TF>, err: LintError) {
        env.lint_errors().push(err);
    }

    fn failwith<N>(msg: impl Into<String>) -> Result<N> {
        Err(Error::Failwith(msg.into()))
    }

    fn text(node: S<'a, T, V>, env: &Env<TF>) -> String {
        String::from(node.text(env.source_text()))
    }

    fn text_str<'b>(node: S<'a, T, V>, env: &'b Env<TF>) -> &'b str {
        node.text(env.source_text())
    }

    fn lowering_error(env: &mut Env<'a, TF>, pos: &Pos, text: &str, syntax_kind: &str) {
        if env.is_typechecker()
            && !env.disable_lowering_parsing_error
            && env.lowpri_errors().is_empty()
        {
            Self::raise_parsing_error_pos(
                pos,
                env,
                &syntax_error::lowering_parsing_error(text, syntax_kind),
            )
        }
    }

    fn missing_syntax_<N>(
        fallback: Option<N>,
        expecting: &str,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<N> {
        let pos = Self::p_pos(node, env);
        let text = Self::text(node, env);
        Self::lowering_error(env, &pos, &text, expecting);
        if let Some(x) = fallback {
            if env.fail_open() {
                return Ok(x);
            }
        }
        Err(Error::MissingSyntax {
            expecting: String::from(expecting),
            pos: Self::p_pos(node, env),
            node_name: text,
            kind: node.kind(),
        })
    }

    fn missing_syntax<N>(expecting: &str, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<N> {
        Self::missing_syntax_(None, expecting, node, env)
    }

    fn is_num_octal_lit(s: &str) -> bool {
        !s.chars().any(|c| c == '8' || c == '9')
    }

    fn mp_optional<F, R>(p: F, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<Option<R>>
    where
        F: FnOnce(S<'a, T, V>, &mut Env<'a, TF>) -> Result<R>,
    {
        match &node.children {
            Missing => Ok(None),
            _ => p(node, env).map(Some),
        }
    }

    fn pos_qualified_name(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Sid> {
        if let QualifiedName(c) = &node.children {
            if let SyntaxList(l) = &c.parts.children {
                let p = Self::p_pos(node, env);
                let mut s = String::with_capacity(node.width());
                for i in l.iter() {
                    match &i.children {
                        ListItem(li) => {
                            s += Self::text_str(&li.item, env);
                            s += Self::text_str(&li.separator, env);
                        }
                        _ => s += Self::text_str(&i, env),
                    }
                }
                return Ok(ast::Id(p, s));
            }
        }
        Self::missing_syntax("qualified name", node, env)
    }

    fn pos_name(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Sid> {
        Self::pos_name_(node, env, None)
    }

    fn lid_from_pos_name(pos: Pos, name: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Lid> {
        let name = Self::pos_name(name, env)?;
        Ok(ast::Lid::new(pos, name.1))
    }

    fn lid_from_name(name: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Lid> {
        let name = Self::pos_name(name, env)?;
        Ok(ast::Lid::new(name.0, name.1))
    }

    fn p_pstring(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Pstring> {
        Self::p_pstring_(node, env, None)
    }

    fn p_pstring_(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        drop_prefix: Option<char>,
    ) -> Result<ast::Pstring> {
        let ast::Id(p, id) = Self::pos_name_(node, env, drop_prefix)?;
        Ok((p, id))
    }

    fn drop_prefix(s: &str, prefix: char) -> &str {
        if s.len() > 0 && s.chars().nth(0) == Some(prefix) {
            &s[1..]
        } else {
            s
        }
    }

    fn pos_name_(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        drop_prefix: Option<char>,
    ) -> Result<ast::Sid> {
        match &node.children {
            QualifiedName(_) => Self::pos_qualified_name(node, env),
            SimpleTypeSpecifier(c) => Self::pos_name_(&c.specifier, env, drop_prefix),
            _ => {
                let mut name = node.text(env.indexed_source_text.source_text());
                if let Some(prefix) = drop_prefix {
                    name = Self::drop_prefix(name, prefix);
                }
                let p = Self::p_pos(node, env);
                Ok(ast::Id(p, String::from(name)))
            }
        }
    }

    fn mk_str<F>(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        unescaper: F,
        mut content: &str,
    ) -> BString
    where
        F: Fn(&str) -> std::result::Result<BString, InvalidString>,
    {
        if let Some('b') = content.chars().nth(0) {
            content = content.get(1..).unwrap();
        }

        let len = content.len();
        let no_quotes_result = extract_unquoted_string(content, 0, len);
        match no_quotes_result {
            Ok(no_quotes) => {
                let result = unescaper(&no_quotes);
                match result {
                    Ok(s) => s,
                    Err(_) => {
                        Self::raise_parsing_error(
                            node,
                            env,
                            &format!("Malformed string literal <<{}>>", &no_quotes),
                        );
                        BString::from("")
                    }
                }
            }
            Err(_) => {
                Self::raise_parsing_error(
                    node,
                    env,
                    &format!("Malformed string literal <<{}>>", &content),
                );
                BString::from("")
            }
        }
    }

    fn unesc_dbl(s: &str) -> std::result::Result<BString, InvalidString> {
        let unesc_s = unescape_double(s)?;
        if unesc_s == B("''") || unesc_s == B("\"\"") {
            Ok(BString::from(""))
        } else {
            Ok(unesc_s)
        }
    }

    // TODO: return Cow<[u8]>
    fn unesc_xhp(s: &[u8]) -> Vec<u8> {
        lazy_static! {
            static ref WHITESPACE: Regex = Regex::new("[\x20\t\n\r\x0c]+").unwrap();
        }
        WHITESPACE.replace_all(s, &b" "[..]).into_owned()
    }

    fn unesc_xhp_attr(s: &[u8]) -> Vec<u8> {
        // TODO: change unesc_dbl to &[u8] -> BString
        let r = Self::get_quoted_content(s);
        let r = unsafe { std::str::from_utf8_unchecked(r) };
        Self::unesc_dbl(r).unwrap().into()
    }

    fn get_quoted_content(s: &[u8]) -> &[u8] {
        lazy_static! {
            static ref QUOTED: Regex = Regex::new(r#"^[\x20\t\n\r\x0c]*"((?:.|\n)*)""#).unwrap();
        }
        QUOTED
            .captures(s)
            .map_or(None, |c| c.get(1))
            .map_or(s, |m| m.as_bytes())
    }

    fn token_kind(node: S<'a, T, V>) -> Option<TK> {
        match &node.children {
            Token(t) => Some(t.kind()),
            _ => None,
        }
    }

    fn check_valid_reified_hint(env: &mut Env<'a, TF>, node: S<'a, T, V>, hint: &ast::Hint) {
        struct Checker<F: FnMut(&String)>(F);
        impl<'ast, F: FnMut(&String)> Visitor<'ast> for Checker<F> {
            type P = AstParams<(), ()>;

            fn object(&mut self) -> &mut dyn Visitor<'ast, P = Self::P> {
                self
            }

            fn visit_hint(&mut self, c: &mut (), h: &ast::Hint) -> std::result::Result<(), ()> {
                match h.1.as_ref() {
                    ast::Hint_::Happly(id, _) => {
                        self.0(&id.1);
                    }
                    ast::Hint_::Haccess(_, ids) => {
                        ids.iter().for_each(|id| self.0(&id.1));
                    }
                    _ => {}
                }
                h.recurse(c, self)
            }
        }

        if *env.in_static_method() {
            let f = |id: &String| {
                Self::fail_if_invalid_reified_generic(node, env, id);
            };
            let mut visitor = Checker(f);
            visitor.visit_hint(&mut (), hint).unwrap();
        }
    }

    fn p_closure_parameter(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(ast::Hint, Option<ast::ParamKind>)> {
        match &node.children {
            ClosureParameterTypeSpecifier(c) => {
                let kind = Self::mp_optional(Self::p_param_kind, &c.call_convention, env)?;
                let hint = Self::p_hint(&c.type_, env)?;
                Ok((hint, kind))
            }
            _ => Self::missing_syntax("closure parameter", node, env),
        }
    }

    fn mp_shape_expression_field<F, R>(
        f: F,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(ast::ShapeFieldName, R)>
    where
        F: Fn(S<'a, T, V>, &mut Env<'a, TF>) -> Result<R>,
    {
        match &node.children {
            FieldInitializer(c) => {
                let name = Self::p_shape_field_name(&c.name, env)?;
                let value = f(&c.value, env)?;
                Ok((name, value))
            }
            _ => Self::missing_syntax("shape field", node, env),
        }
    }

    fn p_shape_field_name(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::ShapeFieldName> {
        use ast::ShapeFieldName::*;
        let is_valid_shape_literal = |t: &T| {
            let is_str = t.kind() == TK::SingleQuotedStringLiteral
                || t.kind() == TK::DoubleQuotedStringLiteral;
            let text = t.text(env.source_text());
            let is_empty = text == "\'\'" || text == "\"\"";
            is_str && !is_empty
        };
        if let LiteralExpression(c) = &node.children {
            if let Token(t) = &c.expression.children {
                if is_valid_shape_literal(t) {
                    let ast::Id(p, n) = Self::pos_name(node, env)?;
                    let unescp = if t.kind() == TK::SingleQuotedStringLiteral {
                        unescape_single
                    } else {
                        Self::unesc_dbl
                    };
                    let str_ = Self::mk_str(node, env, unescp, &n);
                    if let Some(_) = ocaml_helper::int_of_string_opt(&str_.as_bytes()) {
                        Self::raise_parsing_error(
                            node,
                            env,
                            &syntax_error::shape_field_int_like_string,
                        )
                    }
                    return Ok(SFlitStr((p, str_)));
                }
            }
        }
        match &node.children {
            ScopeResolutionExpression(c) => Ok(SFclassConst(
                Self::pos_name(&c.qualifier, env)?,
                Self::p_pstring(&c.name, env)?,
            )),
            _ => {
                Self::raise_parsing_error(node, env, &syntax_error::invalid_shape_field_name);
                let ast::Id(p, n) = Self::pos_name(node, env)?;
                Ok(SFlitStr((p, Self::mk_str(node, env, Self::unesc_dbl, &n))))
            }
        }
    }

    fn p_shape_field(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::ShapeFieldInfo> {
        match &node.children {
            FieldSpecifier(c) => {
                let optional = !c.question.is_missing();
                let name = Self::p_shape_field_name(&c.name, env)?;
                let hint = Self::p_hint(&c.type_, env)?;
                Ok(ast::ShapeFieldInfo {
                    optional,
                    hint,
                    name,
                })
            }
            _ => {
                let (name, hint) = Self::mp_shape_expression_field(Self::p_hint, node, env)?;
                Ok(ast::ShapeFieldInfo {
                    optional: false,
                    name,
                    hint,
                })
            }
        }
    }

    fn p_targ(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Targ> {
        Ok(ast::Targ((), Self::p_hint(node, env)?))
    }

    fn p_hint_(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Hint_> {
        use ast::Hint_::*;
        let unary = |kw, ty, env: &mut Env<'a, TF>| {
            Ok(Happly(
                Self::pos_name(kw, env)?,
                Self::could_map(Self::p_hint, ty, env)?,
            ))
        };
        let binary = |kw, key, ty, env: &mut Env<'a, TF>| {
            let kw = Self::pos_name(kw, env)?;
            let key = Self::p_hint(key, env)?;
            Ok(Happly(
                kw,
                Self::map_flatten_(&Self::p_hint, ty, env, vec![key])?,
            ))
        };

        match &node.children {
            Token(token) if token.kind() == TK::Variable => {
                let ast::Id(_pos, name) = Self::pos_name(node, env)?;
                Ok(Hvar(name))
            }
            /* Dirty hack; CastExpression can have type represented by token */
            Token(_) | SimpleTypeSpecifier(_) | QualifiedName(_) => {
                let ast::Id(pos, name) = Self::pos_name(node, env)?;
                let mut suggest = |name: &str, canonical: &str| {
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::invalid_typehint_alias(name, canonical),
                    );
                };
                if "integer".eq_ignore_ascii_case(&name) {
                    suggest(&name, special_typehints::INT);
                } else if "boolean".eq_ignore_ascii_case(&name) {
                    suggest(&name, special_typehints::BOOL);
                } else if "double".eq_ignore_ascii_case(&name) {
                    suggest(&name, special_typehints::FLOAT);
                } else if "real".eq_ignore_ascii_case(&name) {
                    suggest(&name, special_typehints::FLOAT);
                }
                Ok(Happly(ast::Id(pos, name), vec![]))
            }
            ShapeTypeSpecifier(c) => {
                let allows_unknown_fields = !c.ellipsis.is_missing();
                /* if last element lacks a separator and ellipsis is present, error */
                if allows_unknown_fields {
                    if let SyntaxList(items) = &c.fields.children {
                        if let Some(ListItem(item)) = items.last().map(|i| &i.children) {
                            if item.separator.is_missing() {
                                Self::raise_parsing_error(
                                    node,
                                    env,
                                    &syntax_error::shape_type_ellipsis_without_trailing_comma,
                                )
                            }
                        }
                    }
                }

                let field_map = Self::could_map(Self::p_shape_field, &c.fields, env)?;
                // TODO:(shiqicao) improve perf
                // 1. replace HashSet by fnv hash map or something faster,
                // 2. move `set` to Env to avoid allocation,
                let mut set: HashSet<&BStr> = HashSet::with_capacity(field_map.len());
                for f in field_map.iter() {
                    if !set.insert(f.name.get_name()) {
                        Self::raise_hh_error(
                            env,
                            Naming::fd_name_already_bound(f.name.get_pos().clone()),
                        );
                    }
                }

                Ok(Hshape(ast::NastShapeInfo {
                    allows_unknown_fields,
                    field_map,
                }))
            }
            TupleTypeSpecifier(c) => Ok(Htuple(Self::could_map(Self::p_hint, &c.types, env)?)),
            UnionTypeSpecifier(c) => Ok(Hunion(Self::could_map(&Self::p_hint, &c.types, env)?)),
            IntersectionTypeSpecifier(c) => Ok(Hintersection(Self::could_map(
                &Self::p_hint,
                &c.types,
                env,
            )?)),
            KeysetTypeSpecifier(c) => Ok(Happly(
                Self::pos_name(&c.keyword, env)?,
                Self::could_map(Self::p_hint, &c.type_, env)?,
            )),
            VectorTypeSpecifier(c) => unary(&c.keyword, &c.type_, env),
            ClassnameTypeSpecifier(c) => unary(&c.keyword, &c.type_, env),
            TupleTypeExplicitSpecifier(c) => unary(&c.keyword, &c.types, env),
            VarrayTypeSpecifier(c) => unary(&c.keyword, &c.type_, env),
            DarrayTypeSpecifier(c) => binary(&c.keyword, &c.key, &c.value, env),
            DictionaryTypeSpecifier(c) => unary(&c.keyword, &c.members, env),
            GenericTypeSpecifier(c) => {
                let name = Self::pos_name(&c.class_type, env)?;
                let args = &c.argument_list;
                let type_args = match &args.children {
                    TypeArguments(c) => Self::could_map(Self::p_hint, &c.types, env)?,
                    _ => Self::missing_syntax("generic type arguments", args, env)?,
                };
                if env.codegen() {
                    let process = |name: ast::Sid, mut args: Vec<ast::Hint>| -> ast::Hint_ {
                        if args.len() == 1 {
                            let eq = |s| name.1.eq_ignore_ascii_case(s);
                            if (args[0].1.is_hfun()
                                && (eq(rx::RX)
                                    || eq(rx::RX_LOCAL)
                                    || eq(rx::RX_SHALLOW)
                                    || eq(rx::PURE)))
                                || (args[0].1.is_happly()
                                    && (eq(rx::MUTABLE)
                                        || eq(rx::MAYBE_MUTABLE)
                                        || eq(rx::OWNED_MUTABLE)))
                            {
                                return *args.pop().unwrap().1;
                            }
                        }
                        Happly(name, args)
                    };
                    Ok(process(name, type_args))
                } else {
                    Ok(Happly(name, type_args))
                }
            }
            NullableTypeSpecifier(c) => Ok(Hoption(Self::p_hint(&c.type_, env)?)),
            LikeTypeSpecifier(c) => Ok(Hlike(Self::p_hint(&c.type_, env)?)),
            SoftTypeSpecifier(c) => Ok(Hsoft(Self::p_hint(&c.type_, env)?)),
            ClosureTypeSpecifier(c) => {
                let (param_list, variadic_hints): (Vec<S<'a, T, V>>, Vec<S<'a, T, V>>) = c
                    .parameter_list
                    .syntax_node_to_list_skip_separator()
                    .partition(|n| match &n.children {
                        VariadicParameter(_) => false,
                        _ => true,
                    });
                let (type_hints, kinds) = param_list
                    .iter()
                    .map(|p| Self::p_closure_parameter(p, env))
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .into_iter()
                    .unzip();
                let variadic_hints = variadic_hints
                    .iter()
                    .map(|v| match &v.children {
                        VariadicParameter(c) => {
                            if c.type_.is_missing() {
                                Self::raise_parsing_error(
                                    v,
                                    env,
                                    "Cannot use ... without a typehint",
                                );
                            }
                            Ok(Some(Self::p_hint(&c.type_, env)?))
                        }
                        _ => panic!("expect variadic parameter"),
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if variadic_hints.len() > 1 {
                    return Self::failwith(format!(
                        "{} variadic parameters found. There should be no more than one.",
                        variadic_hints.len().to_string()
                    ));
                }
                let (ctxs, _) = Self::p_contexts(&c.contexts, env)?;
                Ok(Hfun(ast::HintFun {
                    reactive_kind: ast::FuncReactive::FNonreactive,
                    param_tys: type_hints,
                    param_kinds: kinds,
                    param_mutability: vec![],
                    variadic_ty: variadic_hints.into_iter().next().unwrap_or(None),
                    ctxs,
                    return_ty: Self::p_hint(&c.return_type, env)?,
                    is_mutable_return: true,
                }))
            }
            AttributizedSpecifier(c) => {
                let attrs = Self::p_user_attribute(&c.attribute_spec, env)?;
                let hint = Self::p_hint(&c.type_, env)?;
                if attrs.iter().any(|attr| attr.name.1 != special_attrs::SOFT) {
                    Self::raise_parsing_error(node, env, &syntax_error::only_soft_allowed);
                }
                Ok(*Self::soften_hint(&attrs, hint).1)
            }
            FunctionCtxTypeSpecifier(c) => {
                let ast::Id(_p, n) = Self::pos_name(&c.variable, env)?;
                Ok(HfunContext(n))
            }
            TypeConstant(c) => {
                let child = Self::pos_name(&c.right_type, env)?;
                match Self::p_hint_(&c.left_type, env)? {
                    Haccess(root, mut cs) => {
                        cs.push(child);
                        Ok(Haccess(root, cs))
                    }
                    Hvar(n) => {
                        let pos = Self::p_pos(&c.left_type, env);
                        let root = ast::Hint::new(pos, Hvar(n));
                        Ok(Haccess(root, vec![child]))
                    }
                    Happly(ty, param) => {
                        if param.is_empty() {
                            let root = ast::Hint::new(ty.0.clone(), Happly(ty, param));
                            Ok(Haccess(root, vec![child]))
                        } else {
                            Self::missing_syntax("type constant base", node, env)
                        }
                    }
                    _ => Self::missing_syntax("type constant base", node, env),
                }
            }
            ReifiedTypeArgument(_) => {
                Self::raise_parsing_error(node, env, &syntax_error::invalid_reified);
                Self::missing_syntax("refied type", node, env)
            }
            _ => Self::missing_syntax("type hint", node, env),
        }
    }

    fn p_hint(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Hint> {
        let hint_ = Self::p_hint_(node, env)?;
        let pos = Self::p_pos(node, env);
        let hint = ast::Hint::new(pos, hint_);
        Self::check_valid_reified_hint(env, node, &hint);
        Ok(hint)
    }

    fn p_simple_initializer(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Expr> {
        match &node.children {
            SimpleInitializer(c) => Self::p_expr(&c.value, env),
            _ => Self::missing_syntax("simple initializer", node, env),
        }
    }

    fn p_member(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<(ast::Expr, ast::Expr)> {
        match &node.children {
            ElementInitializer(c) => Ok((Self::p_expr(&c.key, env)?, Self::p_expr(&c.value, env)?)),
            _ => Self::missing_syntax("darray intrinsic expression element", node, env),
        }
    }

    fn expand_type_args(ty: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<Vec<ast::Hint>> {
        match &ty.children {
            TypeArguments(c) => Self::could_map(Self::p_hint, &c.types, env),
            _ => Ok(vec![]),
        }
    }

    fn p_afield(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Afield> {
        match &node.children {
            ElementInitializer(c) => Ok(ast::Afield::AFkvalue(
                Self::p_expr(&c.key, env)?,
                Self::p_expr(&c.value, env)?,
            )),
            _ => Ok(ast::Afield::AFvalue(Self::p_expr(node, env)?)),
        }
    }

    fn check_intrinsic_type_arg_varity(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        tys: Vec<ast::Hint>,
    ) -> Option<ast::CollectionTarg> {
        let count = tys.len();
        let mut tys = tys.into_iter();
        match count {
            2 => Some(ast::CollectionTarg::CollectionTKV(
                ast::Targ((), tys.next().unwrap()),
                ast::Targ((), tys.next().unwrap()),
            )),
            1 => Some(ast::CollectionTarg::CollectionTV(ast::Targ(
                (),
                tys.next().unwrap(),
            ))),
            0 => None,
            _ => {
                Self::raise_parsing_error(
                    node,
                    env,
                    &syntax_error::collection_intrinsic_many_typeargs,
                );
                None
            }
        }
    }

    fn p_import_flavor(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::ImportFlavor> {
        use ast::ImportFlavor::*;
        match Self::token_kind(node) {
            Some(TK::Include) => Ok(Include),
            Some(TK::Require) => Ok(Require),
            Some(TK::Include_once) => Ok(IncludeOnce),
            Some(TK::Require_once) => Ok(RequireOnce),
            _ => Self::missing_syntax("import flavor", node, env),
        }
    }

    fn p_null_flavor(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::OgNullFlavor> {
        use ast::OgNullFlavor::*;
        match Self::token_kind(node) {
            Some(TK::QuestionMinusGreaterThan) => Ok(OGNullsafe),
            Some(TK::MinusGreaterThan) => Ok(OGNullthrows),
            _ => Self::missing_syntax("null flavor", node, env),
        }
    }

    fn wrap_unescaper<F>(unescaper: F, s: &str) -> Result<BString>
    where
        F: FnOnce(&str) -> std::result::Result<BString, InvalidString>,
    {
        unescaper(s).map_err(|e| Error::Failwith(e.msg))
    }

    fn fail_if_invalid_class_creation(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        id: impl AsRef<str>,
    ) {
        let id = id.as_ref();
        let is_in_static_method = *env.in_static_method();
        if is_in_static_method
            && ((id == special_classes::SELF && !env.cls_reified_generics().is_empty())
                || (id == special_classes::PARENT && *env.parent_maybe_reified()))
        {
            Self::raise_parsing_error(node, env, &syntax_error::static_method_reified_obj_creation);
        }
    }

    fn fail_if_invalid_reified_generic(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        id: impl AsRef<str>,
    ) {
        let is_in_static_method = *env.in_static_method();
        if is_in_static_method && env.cls_reified_generics().contains(id.as_ref()) {
            Self::raise_parsing_error(
                node,
                env,
                &syntax_error::cls_reified_generic_in_static_method,
            );
        }
    }

    fn rfind(s: &[u8], mut i: usize, c: u8) -> Result<usize> {
        if i >= s.len() {
            return Self::failwith("index out of range");
        }
        i += 1;
        while i > 0 {
            i -= 1;
            if s[i] == c {
                return Ok(i);
            }
        }
        Self::failwith("char not found")
    }

    fn prep_string2(
        nodes: &'a [Syntax<'a, T, V>],
        env: &mut Env<'a, TF>,
    ) -> Result<(TokenOp, TokenOp)> {
        use TokenOp::*;
        let is_qoute = |c| c == b'\"' || c == b'`';
        let start_is_qoute = |s: &[u8]| {
            (s.len() > 0 && is_qoute(s[0])) || (s.len() > 1 && (s[0] == b'b' && s[1] == b'\"'))
        };
        let last_is_qoute = |s: &[u8]| s.len() > 0 && is_qoute(s[s.len() - 1]);
        let is_heredoc = |s: &[u8]| (s.len() > 3 && &s[0..3] == b"<<<");
        let mut nodes = nodes.iter();
        let first = nodes.next();
        match first.map(|n| &n.children) {
            Some(Token(t)) => {
                let raise = |env| {
                    Self::raise_parsing_error(first.unwrap(), env, "Malformed String2 SyntaxList");
                };
                let text = t.text_raw(env.source_text());
                if start_is_qoute(text) {
                    let first_token_op = match text[0] {
                        b'b' if text.len() > 2 => LeftTrim(2),
                        _ if is_qoute(text[0]) && text.len() > 1 => LeftTrim(1),
                        _ => Skip,
                    };
                    if let Some(Token(t)) = nodes.last().map(|n| &n.children) {
                        let last_text = t.text_raw(env.source_text());
                        if last_is_qoute(last_text) {
                            let last_taken_op = match last_text.len() {
                                n if n > 1 => RightTrim(1),
                                _ => Skip,
                            };
                            return Ok((first_token_op, last_taken_op));
                        }
                    }
                    raise(env);
                    Ok((first_token_op, Noop))
                } else if is_heredoc(text) {
                    let trim_size = text
                        .iter()
                        .position(|c| *c == b'\n')
                        .ok_or_else(|| Error::Failwith(String::from("newline not found")))?
                        + 1;
                    let first_token_op = match trim_size {
                        _ if trim_size == text.len() => Skip,
                        _ => LeftTrim(trim_size),
                    };
                    if let Some(Token(t)) = nodes.last().map(|n| &n.children) {
                        let text = t.text_raw(env.source_text());
                        let len = text.len();
                        if len != 0 {
                            let n = Self::rfind(text, len - 2, b'\n')?;
                            let last_token_op = match n {
                                0 => Skip,
                                _ => RightTrim(len - n),
                            };
                            return Ok((first_token_op, last_token_op));
                        }
                    }
                    raise(env);
                    Ok((first_token_op, Noop))
                } else {
                    Ok((Noop, Noop))
                }
            }
            _ => Ok((Noop, Noop)),
        }
    }

    fn process_token_op(
        env: &mut Env<'a, TF>,
        op: TokenOp,
        node: S<'a, T, V>,
    ) -> Result<Option<S<'a, T, V>>> {
        use TokenOp::*;
        match op {
            LeftTrim(n) => match &node.children {
                Token(t) => {
                    let token = env.token_factory.trim_left(t, n);
                    let node = env.arena.alloc(<Syntax<'a, T, V>>::make_token(token));
                    Ok(Some(node))
                }
                _ => Self::failwith("Token expected"),
            },
            RightTrim(n) => match &node.children {
                Token(t) => {
                    let token = env.token_factory.trim_right(t, n);
                    let node = env.arena.alloc(<Syntax<'a, T, V>>::make_token(token));
                    Ok(Some(node))
                }
                _ => Self::failwith("Token expected"),
            },
            _ => Ok(None),
        }
    }

    fn p_string2(nodes: &'a [Syntax<'a, T, V>], env: &mut Env<'a, TF>) -> Result<Vec<ast::Expr>> {
        use TokenOp::*;
        let (head_op, tail_op) = Self::prep_string2(nodes, env)?;
        let mut result = Vec::with_capacity(nodes.len());
        let mut i = 0;
        let last = nodes.len() - 1;
        while i <= last {
            let op = match i {
                0 => head_op,
                _ if i == last => tail_op,
                _ => Noop,
            };

            if op == Skip {
                i += 1;
                continue;
            }

            let node = Self::process_token_op(env, op, &nodes[i])?;
            let node = node.unwrap_or(&nodes[i]);

            if Self::token_kind(node) == Some(TK::Dollar) && i < last {
                if let EmbeddedBracedExpression(_) = &nodes[i + 1].children {
                    Self::raise_parsing_error(
                        &nodes[i + 1],
                        env,
                        &syntax_error::outside_dollar_str_interp,
                    );

                    result.push(Self::p_expr_with_loc(
                        ExprLocation::InDoubleQuotedString,
                        &nodes[i + 1],
                        env,
                    )?);
                    i += 2;
                    continue;
                }
            }

            result.push(Self::p_expr_with_loc(
                ExprLocation::InDoubleQuotedString,
                node,
                env,
            )?);
            i += 1;
        }
        Ok(result)
    }

    fn p_expr_l(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<Vec<ast::Expr>> {
        let p_expr = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::Expr> {
            Self::p_expr_with_loc(ExprLocation::TopLevel, n, e)
        };
        Self::could_map(p_expr, node, env)
    }

    fn p_expr(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Expr> {
        Self::p_expr_with_loc(ExprLocation::TopLevel, node, env)
    }

    fn p_expr_with_loc(
        location: ExprLocation,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::Expr> {
        Self::p_expr_impl(location, node, env, None)
    }

    fn p_expr_impl(
        location: ExprLocation,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        parent_pos: Option<Pos>,
    ) -> Result<ast::Expr> {
        match &node.children {
            BracedExpression(c) => {
                // Either a dynamic method lookup on a dynamic value:
                //   $foo->{$meth_name}();
                // or an XHP splice.
                //   <p id={$id}>hello</p>;
                // In both cases, unwrap, consistent with parentheses.
                Self::p_expr_impl(location, &c.expression, env, parent_pos)
            }
            ParenthesizedExpression(c) => {
                Self::p_expr_impl(location, &c.expression, env, parent_pos)
            }
            ETSpliceExpression(c) => {
                let pos = Self::p_pos(node, env);

                let inner_pos = Self::p_pos(&c.expression, env);
                let inner_expr_ = Self::p_expr_impl_(location, &c.expression, env, parent_pos)?;
                let inner_expr = ast::Expr::new(inner_pos, inner_expr_);
                Ok(ast::Expr::new(
                    pos,
                    ast::Expr_::ETSplice(Box::new(inner_expr)),
                ))
            }
            _ => {
                let pos = Self::p_pos(node, env);
                let expr_ = Self::p_expr_impl_(location, node, env, parent_pos)?;
                Ok(ast::Expr::new(pos, expr_))
            }
        }
    }

    fn p_expr_lit(
        location: ExprLocation,
        parent: S<'a, T, V>,
        expr: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::Expr_> {
        match &expr.children {
            Token(_) => {
                let s = expr.text(env.indexed_source_text.source_text());
                let check_lint_err = |e: &mut Env<'a, TF>, s: &str, expected: &str| {
                    if !e.codegen() && s != expected {
                        Self::raise_lint_error(
                            e,
                            LintError::lowercase_constant(Self::p_pos(expr, e), s),
                        );
                    }
                };
                match (location, Self::token_kind(expr)) {
                    (ExprLocation::InDoubleQuotedString, _) if env.codegen() => {
                        Ok(E_::String(Self::mk_str(expr, env, Self::unesc_dbl, s)))
                    }
                    (_, Some(TK::OctalLiteral))
                        if env.is_typechecker() && !Self::is_num_octal_lit(s) =>
                    {
                        Self::raise_parsing_error(
                            parent,
                            env,
                            &syntax_error::invalid_octal_integer,
                        );
                        Self::missing_syntax("octal", expr, env)
                    }
                    (_, Some(TK::DecimalLiteral))
                    | (_, Some(TK::OctalLiteral))
                    | (_, Some(TK::HexadecimalLiteral))
                    | (_, Some(TK::BinaryLiteral)) => Ok(E_::Int(s.replace("_", ""))),
                    (_, Some(TK::FloatingLiteral)) => Ok(E_::Float(String::from(s))),
                    (_, Some(TK::SingleQuotedStringLiteral)) => {
                        Ok(E_::String(Self::mk_str(expr, env, unescape_single, s)))
                    }
                    (_, Some(TK::DoubleQuotedStringLiteral)) => {
                        Ok(E_::String(Self::mk_str(expr, env, unescape_double, s)))
                    }
                    (_, Some(TK::HeredocStringLiteral)) => {
                        Ok(E_::String(Self::mk_str(expr, env, unescape_heredoc, s)))
                    }
                    (_, Some(TK::NowdocStringLiteral)) => {
                        Ok(E_::String(Self::mk_str(expr, env, unescape_nowdoc, s)))
                    }
                    (_, Some(TK::NullLiteral)) => {
                        check_lint_err(env, s, literal::NULL);
                        Ok(E_::Null)
                    }
                    (_, Some(TK::BooleanLiteral)) => {
                        if s.eq_ignore_ascii_case(literal::FALSE) {
                            check_lint_err(env, s, literal::FALSE);
                            Ok(E_::False)
                        } else if s.eq_ignore_ascii_case(literal::TRUE) {
                            check_lint_err(env, s, literal::TRUE);
                            Ok(E_::True)
                        } else {
                            Self::missing_syntax(&format!("boolean (not: {})", s), expr, env)
                        }
                    }
                    _ => Self::missing_syntax("literal", expr, env),
                }
            }
            SyntaxList(ts) => Ok(E_::String2(Self::p_string2(ts, env)?)),
            _ => Self::missing_syntax("literal expressoin", expr, env),
        }
    }

    fn p_expr_with_loc_(
        location: ExprLocation,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::Expr_> {
        Self::p_expr_impl_(location, node, env, None)
    }

    fn p_expr_impl_(
        location: ExprLocation,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        parent_pos: Option<Pos>,
    ) -> Result<ast::Expr_> {
        if *env.exp_recursion_depth() >= EXP_RECUSION_LIMIT {
            Err(Error::Failwith("Expression recursion limit reached".into()))
        } else {
            *env.exp_recursion_depth() += 1;
            let r = Self::p_expr_impl__(location, node, env, parent_pos);
            *env.exp_recursion_depth() -= 1;
            r
        }
    }

    fn p_expr_impl__(
        location: ExprLocation,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        parent_pos: Option<Pos>,
    ) -> Result<ast::Expr_> {
        env.check_stack_limit();
        use ast::Expr as E;
        let split_args_vararg = |
            arg_list_node: S<'a, T, V>,
            e: &mut Env<'a, TF>,
        | -> Result<(Vec<ast::Expr>, Option<ast::Expr>)> {
            let mut arg_list: Vec<_> = arg_list_node.syntax_node_to_list_skip_separator().collect();
            if let Some(last_arg) = arg_list.last() {
                if let DecoratedExpression(c) = &last_arg.children {
                    if Self::token_kind(&c.decorator) == Some(TK::DotDotDot) {
                        let _ = arg_list.pop();
                        let args: std::result::Result<Vec<_>, _> =
                            arg_list.iter().map(|a| Self::p_expr(a, e)).collect();
                        let args = args?;
                        let vararg = Self::p_expr(&c.expression, e)?;
                        return Ok((args, Some(vararg)));
                    }
                }
            }
            Ok((Self::could_map(Self::p_expr, arg_list_node, e)?, None))
        };
        let mk_lid = |p: Pos, s: String| ast::Lid(p, (0, s));
        let mk_name_lid = |name: S<'a, T, V>, env: &mut Env<'a, TF>| {
            let name = Self::pos_name(name, env)?;
            Ok(mk_lid(name.0, name.1))
        };
        let mk_lvar =
            |name: S<'a, T, V>, env: &mut Env<'a, TF>| Ok(E_::mk_lvar(mk_name_lid(name, env)?));
        let mk_id_expr = |name: ast::Sid| E::new(name.0.clone(), E_::mk_id(name));
        let p_intri_expr = |kw, ty, v, e: &mut Env<'a, TF>| {
            let hints = Self::expand_type_args(ty, e)?;
            let hints = Self::check_intrinsic_type_arg_varity(node, e, hints);
            Ok(E_::mk_collection(
                Self::pos_name(kw, e)?,
                hints,
                Self::could_map(Self::p_afield, v, e)?,
            ))
        };
        let p_special_call =
            |recv: S<'a, T, V>, args: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::Expr_> {
                let pos_if_has_parens = match &recv.children {
                    ParenthesizedExpression(_) => Some(Self::p_pos(recv, e)),
                    _ => None,
                };
                let recv = Self::p_expr(recv, e)?;
                let recv = match (&recv.1, pos_if_has_parens) {
                    (E_::ObjGet(t), Some(ref _p)) => {
                        let (a, b, c, _false) = &**t;
                        E::new(
                            recv.0.clone(),
                            E_::mk_obj_get(a.clone(), b.clone(), c.clone(), true),
                        )
                    }
                    (E_::ClassGet(c), Some(ref _p)) => {
                        let (a, b, _false) = &**c;
                        E::new(recv.0.clone(), E_::mk_class_get(a.clone(), b.clone(), true))
                    }
                    _ => recv,
                };
                let (args, varargs) = split_args_vararg(args, e)?;
                Ok(E_::mk_call(recv, vec![], args, varargs))
            };
        let p_obj_get = |
            recv: S<'a, T, V>,
            op: S<'a, T, V>,
            name: S<'a, T, V>,
            e: &mut Env<'a, TF>,
        | -> Result<ast::Expr_> {
            if recv.is_object_creation_expression() && !e.codegen() {
                Self::raise_parsing_error(recv, e, &syntax_error::invalid_constructor_method_call);
            }
            let recv = Self::p_expr(recv, e)?;
            let name = Self::p_expr_with_loc(ExprLocation::MemberSelect, name, e)?;
            let op = Self::p_null_flavor(op, e)?;
            Ok(E_::mk_obj_get(recv, name, op, false))
        };
        let pos = match parent_pos {
            None => Self::p_pos(node, env),
            Some(p) => p,
        };
        match &node.children {
            LambdaExpression(c) => {
                let suspension_kind = Self::mk_suspension_kind(&c.async_);
                let (params, (ctxs, unsafe_ctxs), ret) = match &c.signature.children {
                    LambdaSignature(c) => {
                        let params = Self::could_map(Self::p_fun_param, &c.parameters, env)?;
                        let (ctxs, unsafe_ctxs) = Self::p_contexts(&c.contexts, env)?;
                        if Self::has_polymorphic_context(&ctxs) {
                            Self::raise_parsing_error(
                                &c.contexts,
                                env,
                                &syntax_error::lambda_effect_polymorphic,
                            );
                        }
                        let ret = Self::mp_optional(Self::p_hint, &c.type_, env)?;
                        (params, (ctxs, unsafe_ctxs), ret)
                    }
                    Token(_) => {
                        let ast::Id(p, n) = Self::pos_name(&c.signature, env)?;
                        (
                            vec![ast::FunParam {
                                annotation: p.clone(),
                                type_hint: ast::TypeHint((), None),
                                is_variadic: false,
                                pos: p,
                                name: n,
                                expr: None,
                                callconv: None,
                                user_attributes: vec![],
                                visibility: None,
                            }],
                            (None, None),
                            None,
                        )
                    }
                    _ => Self::missing_syntax("lambda signature", &c.signature, env)?,
                };

                let (body, yield_) = if !c.body.is_compound_statement() {
                    Self::mp_yielding(Self::p_function_body, &c.body, env)?
                } else {
                    let mut env1 = Env::clone_and_unset_toplevel_if_toplevel(env);
                    Self::mp_yielding(&Self::p_function_body, &c.body, env1.as_mut())?
                };
                let external = c.body.is_external();
                let fun = ast::Fun_ {
                    span: pos.clone(),
                    annotation: (),
                    mode: env.file_mode(),
                    ret: ast::TypeHint((), ret),
                    name: ast::Id(pos, String::from(";anonymous")),
                    tparams: vec![],
                    where_constraints: vec![],
                    body: ast::FuncBody {
                        ast: body,
                        annotation: (),
                    },
                    fun_kind: Self::mk_fun_kind(suspension_kind, yield_),
                    variadic: Self::determine_variadicity(&params),
                    params,
                    ctxs,
                    unsafe_ctxs,
                    user_attributes: Self::p_user_attributes(&c.attribute_spec, env)?,
                    file_attributes: vec![],
                    external,
                    namespace: Self::mk_empty_ns_env(env),
                    doc_comment: None,
                    static_: false,
                };
                Ok(E_::mk_lfun(fun, vec![]))
            }
            BracedExpression(c) => Self::p_expr_with_loc_(location, &c.expression, env),
            EmbeddedBracedExpression(c) => {
                Self::p_expr_impl_(location, &c.expression, env, Some(pos))
            }
            ParenthesizedExpression(c) => Self::p_expr_with_loc_(location, &c.expression, env),
            DictionaryIntrinsicExpression(c) => {
                p_intri_expr(&c.keyword, &c.explicit_type, &c.members, env)
            }
            KeysetIntrinsicExpression(c) => {
                p_intri_expr(&c.keyword, &c.explicit_type, &c.members, env)
            }
            VectorIntrinsicExpression(c) => {
                p_intri_expr(&c.keyword, &c.explicit_type, &c.members, env)
            }
            CollectionLiteralExpression(c) => {
                let (collection_name, hints) = match &c.name.children {
                    SimpleTypeSpecifier(c) => (Self::pos_name(&c.specifier, env)?, None),
                    GenericTypeSpecifier(c) => {
                        let hints = Self::expand_type_args(&c.argument_list, env)?;
                        let hints = Self::check_intrinsic_type_arg_varity(node, env, hints);
                        (Self::pos_name(&c.class_type, env)?, hints)
                    }
                    _ => (Self::pos_name(&c.name, env)?, None),
                };
                Ok(E_::mk_collection(
                    collection_name,
                    hints,
                    Self::could_map(Self::p_afield, &c.initializers, env)?,
                ))
            }
            VarrayIntrinsicExpression(c) => {
                let hints = Self::expand_type_args(&c.explicit_type, env)?;
                let hints = Self::check_intrinsic_type_arg_varity(node, env, hints);
                let targ = match hints {
                    Some(ast::CollectionTarg::CollectionTV(ty)) => Some(ty),
                    None => None,
                    _ => Self::missing_syntax("VarrayIntrinsicExpression type args", node, env)?,
                };
                Ok(E_::mk_varray(
                    targ,
                    Self::could_map(Self::p_expr, &c.members, env)?,
                ))
            }
            DarrayIntrinsicExpression(c) => {
                let hints = Self::expand_type_args(&c.explicit_type, env)?;
                let hints = Self::check_intrinsic_type_arg_varity(node, env, hints);
                match hints {
                    Some(ast::CollectionTarg::CollectionTKV(tk, tv)) => Ok(E_::mk_darray(
                        Some((tk, tv)),
                        Self::could_map(Self::p_member, &c.members, env)?,
                    )),
                    None => Ok(E_::mk_darray(
                        None,
                        Self::could_map(Self::p_member, &c.members, env)?,
                    )),
                    _ => Self::missing_syntax("DarrayIntrinsicExpression type args", node, env),
                }
            }
            ListExpression(c) => {
                /* TODO: Or tie in with other intrinsics and post-process to List */
                let p_binder_or_ignore =
                    |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::Expr> {
                        match &n.children {
                            Missing => Ok(E::new(e.mk_none_pos(), E_::Omitted)),
                            _ => Self::p_expr(n, e),
                        }
                    };
                Ok(E_::List(Self::could_map(
                    &p_binder_or_ignore,
                    &c.members,
                    env,
                )?))
            }
            EvalExpression(c) => p_special_call(&c.keyword, &c.argument, env),
            IssetExpression(c) => p_special_call(&c.keyword, &c.argument_list, env),
            TupleExpression(c) => p_special_call(&c.keyword, &c.items, env),
            FunctionCallExpression(c) => {
                let recv = &c.receiver;
                let args = &c.argument_list;
                let get_hhas_adata = || {
                    if Self::text_str(recv, env) == "__hhas_adata" {
                        if let SyntaxList(l) = &args.children {
                            if let Some(li) = l.first() {
                                if let ListItem(i) = &li.children {
                                    if let LiteralExpression(le) = &i.item.children {
                                        let expr = &le.expression;
                                        if Self::token_kind(expr) == Some(TK::NowdocStringLiteral) {
                                            return Some(expr);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None
                };
                match get_hhas_adata() {
                    Some(expr) => {
                        let literal_expression_pos = Self::p_pos(expr, env);
                        let s = extract_unquoted_string(Self::text_str(expr, env), 0, expr.width())
                            .map_err(|e| Error::Failwith(e.msg))?;
                        Ok(E_::mk_call(
                            Self::p_expr(recv, env)?,
                            vec![],
                            vec![E::new(literal_expression_pos, E_::String(s.into()))],
                            None,
                        ))
                    }
                    None => {
                        let targs = match (&recv.children, &c.type_args.children) {
                            (_, TypeArguments(c)) => Self::could_map(Self::p_targ, &c.types, env)?,
                            /* TODO might not be needed */
                            (GenericTypeSpecifier(c), _) => match &c.argument_list.children {
                                TypeArguments(c) => Self::could_map(Self::p_targ, &c.types, env)?,
                                _ => vec![],
                            },
                            _ => vec![],
                        };

                        /* preserve parens on receiver of call expression
                        to allow distinguishing between
                        ($a->b)() // invoke on callable property
                        $a->b()   // method call */
                        let pos_if_has_parens = match &recv.children {
                            ParenthesizedExpression(_) => Some(Self::p_pos(recv, env)),
                            _ => None,
                        };
                        let recv = Self::p_expr(recv, env)?;
                        let recv = match (&recv.1, pos_if_has_parens) {
                            (E_::ObjGet(t), Some(ref _p)) => {
                                let (a, b, c, _false) = &**t;
                                E::new(
                                    recv.0.clone(),
                                    E_::mk_obj_get(a.clone(), b.clone(), c.clone(), true),
                                )
                            }
                            (E_::ClassGet(c), Some(ref _p)) => {
                                let (a, b, _false) = &**c;
                                E::new(recv.0.clone(), E_::mk_class_get(a.clone(), b.clone(), true))
                            }
                            _ => recv,
                        };
                        let (args, varargs) = split_args_vararg(args, env)?;
                        Ok(E_::mk_call(recv, targs, args, varargs))
                    }
                }
            }
            FunctionPointerExpression(c) => {
                let targs = match &c.type_args.children {
                    TypeArguments(c) => Self::could_map(Self::p_targ, &c.types, env)?,
                    _ => vec![],
                };

                let recv = Self::p_expr(&c.receiver, env)?;

                match &recv.1 {
                    aast::Expr_::Id(id) => Ok(E_::mk_function_pointer(
                        aast::FunctionPtrId::FPId(*(id.to_owned())),
                        targs,
                    )),
                    aast::Expr_::ClassConst(c) => {
                        if let aast::ClassId_::CIexpr(aast::Expr(_, aast::Expr_::Id(_))) = (c.0).1 {
                            Ok(E_::mk_function_pointer(
                                aast::FunctionPtrId::FPClassConst(c.0.to_owned(), c.1.to_owned()),
                                targs,
                            ))
                        } else {
                            Self::raise_parsing_error(
                                node,
                                env,
                                &syntax_error::function_pointer_bad_recv,
                            );
                            Self::missing_syntax("function or static method", node, env)
                        }
                    }
                    _ => {
                        Self::raise_parsing_error(
                            node,
                            env,
                            &syntax_error::function_pointer_bad_recv,
                        );
                        Self::missing_syntax("function or static method", node, env)
                    }
                }
            }
            QualifiedName(_) => match location {
                ExprLocation::InDoubleQuotedString => {
                    let ast::Id(_, n) = Self::pos_qualified_name(node, env)?;
                    Ok(E_::String(n.into()))
                }
                _ => Ok(E_::mk_id(Self::pos_qualified_name(node, env)?)),
            },
            VariableExpression(c) => Ok(E_::mk_lvar(Self::lid_from_pos_name(
                pos,
                &c.expression,
                env,
            )?)),
            PipeVariableExpression(_) => Ok(E_::mk_lvar(mk_lid(
                pos,
                special_idents::DOLLAR_DOLLAR.into(),
            ))),
            InclusionExpression(c) => Ok(E_::mk_import(
                Self::p_import_flavor(&c.require, env)?,
                Self::p_expr(&c.filename, env)?,
            )),
            MemberSelectionExpression(c) => p_obj_get(&c.object, &c.operator, &c.name, env),
            SafeMemberSelectionExpression(c) => p_obj_get(&c.object, &c.operator, &c.name, env),
            EmbeddedMemberSelectionExpression(c) => p_obj_get(&c.object, &c.operator, &c.name, env),
            PrefixUnaryExpression(_) | PostfixUnaryExpression(_) | DecoratedExpression(_) => {
                let (operand, op, postfix) = match &node.children {
                    PrefixUnaryExpression(c) => (&c.operand, &c.operator, false),
                    PostfixUnaryExpression(c) => (&c.operand, &c.operator, true),
                    DecoratedExpression(c) => (&c.expression, &c.decorator, false),
                    _ => Self::missing_syntax("unary exppr", node, env)?,
                };

                /**
                 * FFP does not destinguish between ++$i and $i++ on the level of token
                 * kind annotation. Prevent duplication by switching on `postfix` for
                 * the two operatores for which AST /does/ differentiate between
                 * fixities.
                 */
                use ast::Uop::*;
                let mk_unop = |op, e| Ok(E_::mk_unop(op, e));
                let op_kind = Self::token_kind(op);
                if let Some(TK::At) = op_kind {
                    if env.parser_options.po_disallow_silence {
                        Self::raise_parsing_error(op, env, &syntax_error::no_silence);
                    }
                    if env.codegen() {
                        let expr = Self::p_expr(operand, env)?;
                        mk_unop(Usilence, expr)
                    } else {
                        let expr =
                            Self::p_expr_impl(ExprLocation::TopLevel, operand, env, Some(pos))?;
                        Ok(expr.1)
                    }
                } else {
                    let expr = Self::p_expr(operand, env)?;
                    match op_kind {
                        Some(TK::PlusPlus) if postfix => mk_unop(Upincr, expr),
                        Some(TK::MinusMinus) if postfix => mk_unop(Updecr, expr),
                        Some(TK::PlusPlus) => mk_unop(Uincr, expr),
                        Some(TK::MinusMinus) => mk_unop(Udecr, expr),
                        Some(TK::Exclamation) => mk_unop(Unot, expr),
                        Some(TK::Tilde) => mk_unop(Utild, expr),
                        Some(TK::Plus) => mk_unop(Uplus, expr),
                        Some(TK::Minus) => mk_unop(Uminus, expr),
                        Some(TK::Inout) => Ok(E_::mk_callconv(ast::ParamKind::Pinout, expr)),
                        Some(TK::Await) => Self::lift_await(pos, expr, env, location),
                        Some(TK::Clone) => Ok(E_::mk_clone(expr)),
                        Some(TK::Print) => Ok(E_::mk_call(
                            E::new(
                                pos.clone(),
                                E_::mk_id(ast::Id(pos, special_functions::ECHO.into())),
                            ),
                            vec![],
                            vec![expr],
                            None,
                        )),
                        Some(TK::Dollar) => {
                            Self::raise_parsing_error(
                                op,
                                env,
                                &syntax_error::invalid_variable_name,
                            );
                            Ok(E_::Omitted)
                        }
                        _ => Self::missing_syntax("unary operator", node, env),
                    }
                }
            }
            BinaryExpression(c) => {
                use ExprLocation::*;
                let rlocation = if Self::token_kind(&c.operator) == Some(TK::Equal) {
                    match location {
                        AsStatement => RightOfAssignment,
                        UsingStatement => RightOfAssignmentInUsingStatement,
                        _ => TopLevel,
                    }
                } else {
                    TopLevel
                };
                let bop_ast_node = Self::p_bop(
                    pos,
                    &c.operator,
                    Self::p_expr(&c.left_operand, env)?,
                    Self::p_expr_with_loc(rlocation, &c.right_operand, env)?,
                    env,
                )?;
                if let Some((ast::Bop::Eq(_), lhs, _)) = bop_ast_node.as_binop() {
                    Self::check_lvalue(lhs, env);
                }
                Ok(bop_ast_node)
            }
            Token(t) => {
                use ExprLocation::*;
                match (location, t.kind()) {
                    (MemberSelect, TK::Variable) => mk_lvar(node, env),
                    (InDoubleQuotedString, TK::HeredocStringLiteral)
                    | (InDoubleQuotedString, TK::HeredocStringLiteralHead)
                    | (InDoubleQuotedString, TK::HeredocStringLiteralTail) => Ok(E_::String(
                        Self::wrap_unescaper(unescape_heredoc, Self::text_str(node, env))?,
                    )),
                    (InDoubleQuotedString, _) => Ok(E_::String(Self::wrap_unescaper(
                        Self::unesc_dbl,
                        Self::text_str(node, env),
                    )?)),
                    (MemberSelect, _)
                    | (TopLevel, _)
                    | (AsStatement, _)
                    | (UsingStatement, _)
                    | (RightOfAssignment, _)
                    | (RightOfAssignmentInUsingStatement, _)
                    | (RightOfReturn, _) => Ok(E_::mk_id(Self::pos_name(node, env)?)),
                }
            }
            YieldExpression(c) => {
                use ExprLocation::*;
                env.saw_yield = true;
                if location != AsStatement
                    && location != RightOfAssignment
                    && location != RightOfAssignmentInUsingStatement
                {
                    Self::raise_parsing_error(node, env, &syntax_error::invalid_yield);
                }
                if c.operand.is_missing() {
                    Ok(E_::mk_yield(ast::Afield::AFvalue(E::new(pos, E_::Null))))
                } else {
                    Ok(E_::mk_yield(Self::p_afield(&c.operand, env)?))
                }
            }
            DefineExpression(c) => {
                let name = Self::pos_name(&c.keyword, env)?;
                Ok(E_::mk_call(
                    mk_id_expr(name),
                    vec![],
                    c.argument_list
                        .syntax_node_to_list_skip_separator()
                        .map(|x| Self::p_expr(x, env))
                        .collect::<std::result::Result<Vec<_>, _>>()?,
                    None,
                ))
            }
            ScopeResolutionExpression(c) => {
                let qual = Self::p_expr(&c.qualifier, env)?;
                if let E_::Id(id) = &qual.1 {
                    Self::fail_if_invalid_reified_generic(node, env, &id.1);
                }
                match &c.name.children {
                    Token(token) if token.kind() == TK::Variable => {
                        let ast::Id(p, name) = Self::pos_name(&c.name, env)?;
                        Ok(E_::mk_class_get(
                            ast::ClassId(pos, ast::ClassId_::CIexpr(qual)),
                            ast::ClassGetExpr::CGstring((p, name)),
                            false,
                        ))
                    }
                    _ => {
                        let E(p, expr_) = Self::p_expr(&c.name, env)?;
                        match expr_ {
                            E_::String(id) => Ok(E_::mk_class_const(
                                ast::ClassId(pos, ast::ClassId_::CIexpr(qual)),
                                (
                                    p,
                                    String::from_utf8(id.into())
                                        .map_err(|e| Error::Failwith(e.to_string()))?,
                                ),
                            )),
                            E_::Id(id) => {
                                let ast::Id(p, n) = *id;
                                Ok(E_::mk_class_const(
                                    ast::ClassId(pos, ast::ClassId_::CIexpr(qual)),
                                    (p, n),
                                ))
                            }
                            E_::Lvar(id) => {
                                let ast::Lid(p, (_, n)) = *id;
                                Ok(E_::mk_class_get(
                                    ast::ClassId(pos, ast::ClassId_::CIexpr(qual)),
                                    ast::ClassGetExpr::CGstring((p, n)),
                                    false,
                                ))
                            }
                            _ => Ok(E_::mk_class_get(
                                ast::ClassId(pos, ast::ClassId_::CIexpr(qual)),
                                ast::ClassGetExpr::CGexpr(E(p, expr_)),
                                false,
                            )),
                        }
                    }
                }
            }
            CastExpression(c) => Ok(E_::mk_cast(
                Self::p_hint(&c.type_, env)?,
                Self::p_expr(&c.operand, env)?,
            )),
            PrefixedCodeExpression(c) => {
                let src_expr = Self::p_expr(&c.expression, env)?;
                let hint = Self::p_hint(&c.prefix, env)?;
                let desugared_expr = desugar(&hint, &src_expr, env);
                Ok(E_::mk_expression_tree(ast::ExpressionTree {
                    hint,
                    src_expr,
                    desugared_expr,
                }))
            }
            ConditionalExpression(c) => {
                let alter = Self::p_expr(&c.alternative, env)?;
                let consequence = Self::mp_optional(Self::p_expr, &c.consequence, env)?;
                let condition = Self::p_expr(&c.test, env)?;
                Ok(E_::mk_eif(condition, consequence, alter))
            }
            SubscriptExpression(c) => Ok(E_::mk_array_get(
                Self::p_expr(&c.receiver, env)?,
                Self::mp_optional(Self::p_expr, &c.index, env)?,
            )),
            EmbeddedSubscriptExpression(c) => Ok(E_::mk_array_get(
                Self::p_expr(&c.receiver, env)?,
                Self::mp_optional(|n, e| Self::p_expr_with_loc(location, n, e), &c.index, env)?,
            )),
            ShapeExpression(c) => Ok(E_::Shape(Self::could_map(
                |n: S<'a, T, V>, e: &mut Env<'a, TF>| {
                    Self::mp_shape_expression_field(&Self::p_expr, n, e)
                },
                &c.fields,
                env,
            )?)),
            ObjectCreationExpression(c) => Self::p_expr_impl_(location, &c.object, env, Some(pos)),
            ConstructorCall(c) => {
                let (args, varargs) = split_args_vararg(&c.argument_list, env)?;
                let (e, hl) = match &c.type_.children {
                    GenericTypeSpecifier(c) => {
                        let name = Self::pos_name(&c.class_type, env)?;
                        let hints = match &c.argument_list.children {
                            TypeArguments(c) => Self::could_map(Self::p_targ, &c.types, env)?,
                            _ => Self::missing_syntax(
                                "generic type arguments",
                                &c.argument_list,
                                env,
                            )?,
                        };
                        (mk_id_expr(name), hints)
                    }
                    SimpleTypeSpecifier(_) => {
                        let name = Self::pos_name(&c.type_, env)?;
                        (mk_id_expr(name), vec![])
                    }
                    _ => (Self::p_expr(&c.type_, env)?, vec![]),
                };
                if let E_::Id(name) = &e.1 {
                    Self::fail_if_invalid_reified_generic(node, env, &name.1);
                    Self::fail_if_invalid_class_creation(node, env, &name.1);
                }
                Ok(E_::mk_new(
                    ast::ClassId(pos.clone(), ast::ClassId_::CIexpr(e)),
                    hl,
                    args,
                    varargs,
                    pos,
                ))
            }
            GenericTypeSpecifier(c) => {
                if !c.argument_list.is_missing() {
                    Self::raise_parsing_error(
                        &c.argument_list,
                        env,
                        &syntax_error::targs_not_allowed,
                    )
                }
                Ok(E_::mk_id(Self::pos_name(&c.class_type, env)?))
            }
            RecordCreationExpression(c) => {
                let id = Self::pos_name(&c.type_, env)?;
                Ok(E_::mk_record(
                    id,
                    Self::could_map(Self::p_member, &c.members, env)?,
                ))
            }
            LiteralExpression(c) => Self::p_expr_lit(location, node, &c.expression, env),
            PrefixedStringExpression(c) => {
                /* Temporarily allow only`re`- prefixed strings */
                let name_text = Self::text(&c.name, env);
                if name_text != "re" {
                    Self::raise_parsing_error(node, env, &syntax_error::non_re_prefix);
                }
                Ok(E_::mk_prefixed_string(
                    name_text,
                    Self::p_expr(&c.str, env)?,
                ))
            }
            IsExpression(c) => Ok(E_::mk_is(
                Self::p_expr(&c.left_operand, env)?,
                Self::p_hint(&c.right_operand, env)?,
            )),
            AsExpression(c) => Ok(E_::mk_as(
                Self::p_expr(&c.left_operand, env)?,
                Self::p_hint(&c.right_operand, env)?,
                false,
            )),
            NullableAsExpression(c) => Ok(E_::mk_as(
                Self::p_expr(&c.left_operand, env)?,
                Self::p_hint(&c.right_operand, env)?,
                true,
            )),
            AnonymousFunction(c) => {
                if env.parser_options.po_disable_static_closures
                    && Self::token_kind(&c.static_keyword) == Some(TK::Static)
                {
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::static_closures_are_disabled,
                    );
                }
                let p_arg = |n: S<'a, T, V>, e: &mut Env<'a, TF>| match &n.children {
                    Token(_) => mk_name_lid(n, e),
                    _ => Self::missing_syntax("use variable", n, e),
                };
                let (ctxs, unsafe_ctxs) = Self::p_contexts(&c.ctx_list, env)?;
                let p_use = |n: S<'a, T, V>, e: &mut Env<'a, TF>| match &n.children {
                    AnonymousFunctionUseClause(c) => Self::could_map(p_arg, &c.variables, e),
                    _ => Ok(vec![]),
                };
                let suspension_kind = Self::mk_suspension_kind(&c.async_keyword);
                let (body, yield_) = {
                    let mut env1 = Env::clone_and_unset_toplevel_if_toplevel(env);
                    Self::mp_yielding(&Self::p_function_body, &c.body, env1.as_mut())?
                };
                let doc_comment =
                    Self::extract_docblock(node, env).or_else(|| env.top_docblock().clone());
                let user_attributes = Self::p_user_attributes(&c.attribute_spec, env)?;
                let external = c.body.is_external();
                let params = Self::could_map(Self::p_fun_param, &c.parameters, env)?;
                let name_pos = Self::p_fun_pos(node, env);
                let fun = ast::Fun_ {
                    span: Self::p_pos(node, env),
                    annotation: (),
                    mode: env.file_mode(),
                    ret: ast::TypeHint((), Self::mp_optional(Self::p_hint, &c.type_, env)?),
                    name: ast::Id(name_pos, String::from(";anonymous")),
                    tparams: vec![],
                    where_constraints: vec![],
                    body: ast::FuncBody {
                        ast: body,
                        annotation: (),
                    },
                    fun_kind: Self::mk_fun_kind(suspension_kind, yield_),
                    variadic: Self::determine_variadicity(&params),
                    params,
                    ctxs,
                    unsafe_ctxs,
                    user_attributes,
                    file_attributes: vec![],
                    external,
                    namespace: Self::mk_empty_ns_env(env),
                    doc_comment,
                    static_: !c.static_keyword.is_missing(),
                };
                let uses = p_use(&c.use_, env).unwrap_or_else(|_| vec![]);
                Ok(E_::mk_efun(fun, uses))
            }
            AwaitableCreationExpression(c) => {
                let suspension_kind = Self::mk_suspension_kind(&c.async_);
                let (blk, yld) =
                    Self::mp_yielding(&Self::p_function_body, &c.compound_statement, env)?;
                let user_attributes = Self::p_user_attributes(&c.attribute_spec, env)?;
                let external = c.compound_statement.is_external();
                let name_pos = Self::p_fun_pos(node, env);
                let body = ast::Fun_ {
                    span: pos.clone(),
                    annotation: (),
                    mode: env.file_mode(),
                    ret: ast::TypeHint((), None),
                    name: ast::Id(name_pos, String::from(";anonymous")),
                    tparams: vec![],
                    where_constraints: vec![],
                    body: ast::FuncBody {
                        ast: if blk.len() == 0 {
                            let pos = Self::p_pos(&c.compound_statement, env);
                            vec![ast::Stmt::noop(pos)]
                        } else {
                            blk
                        },
                        annotation: (),
                    },
                    fun_kind: Self::mk_fun_kind(suspension_kind, yld),
                    variadic: Self::determine_variadicity(&[]),
                    params: vec![],
                    ctxs: None,        // TODO(T70095684)
                    unsafe_ctxs: None, // TODO(T70095684)
                    user_attributes,
                    file_attributes: vec![],
                    external,
                    namespace: Self::mk_empty_ns_env(env),
                    doc_comment: None,
                    static_: false,
                };
                Ok(E_::mk_call(
                    E::new(pos, E_::mk_lfun(body, vec![])),
                    vec![],
                    vec![],
                    None,
                ))
            }
            XHPExpression(c) if c.open.is_xhp_open() => {
                if let XHPOpen(c1) = &c.open.children {
                    let name = Self::pos_name(&c1.name, env)?;
                    let attrs = Self::could_map(Self::p_xhp_attr, &c1.attributes, env)?;
                    let exprs = Self::aggregate_xhp_tokens(env, &c.body)?
                        .iter()
                        .map(|n| Self::p_xhp_embedded(Self::unesc_xhp, n, env))
                        .collect::<std::result::Result<Vec<_>, _>>()?;

                    let id = if env.empty_ns_env.disable_xhp_element_mangling {
                        ast::Id(name.0, name.1)
                    } else {
                        ast::Id(name.0, String::from(":") + &name.1)
                    };

                    Ok(E_::mk_xml(
                        // TODO: update pos_name to support prefix
                        id, attrs, exprs,
                    ))
                } else {
                    Self::failwith("expect xhp open")
                }
            }
            EnumAtomExpression(c) => Ok(E_::EnumAtom(Self::pos_name(&c.expression, env)?.1)),
            _ => Self::missing_syntax_(Some(E_::Null), "expression", node, env),
        }
    }

    fn check_lvalue(ast::Expr(p, expr_): &ast::Expr, env: &mut Env<'a, TF>) {
        use aast::Expr_::*;
        let mut raise = |s| Self::raise_parsing_error_pos(p, env, s);
        match expr_ {
            ObjGet(og) => {
                if og.as_ref().3 {
                    raise("Invalid lvalue")
                } else {
                    match og.as_ref() {
                        (_, ast::Expr(_, Id(_)), ast::OgNullFlavor::OGNullsafe, _) => {
                            raise("?-> syntax is not supported for lvalues")
                        }
                        (_, ast::Expr(_, Id(sid)), _, _) if sid.1.as_bytes()[0] == b':' => {
                            raise("->: syntax is not supported for lvalues")
                        }
                        _ => {}
                    }
                }
            }
            ArrayGet(ag) => {
                if let ClassConst(_) = (ag.0).1 {
                    raise("Array-like class consts are not valid lvalues");
                }
            }
            Call(c) => match &(c.0).1 {
                Id(sid) if sid.1 == "tuple" => {
                    raise("Tuple cannot be used as an lvalue. Maybe you meant list?")
                }
                _ => raise("Invalid lvalue"),
            },
            List(l) => {
                for i in l.iter() {
                    Self::check_lvalue(i, env);
                }
            }
            Darray(_) | Varray(_) | Shape(_) | Collection(_) | Record(_) | Null | True | False
            | Id(_) | Clone(_) | ClassConst(_) | Int(_) | Float(_) | PrefixedString(_)
            | String(_) | String2(_) | Yield(_) | Await(_) | Cast(_) | Unop(_) | Binop(_)
            | Eif(_) | New(_) | Efun(_) | Lfun(_) | Xml(_) | Import(_) | Pipe(_) | Callconv(_)
            | Is(_) | As(_) => raise("Invalid lvalue"),
            _ => {}
        }
    }

    fn p_xhp_embedded<F>(escaper: F, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Expr>
    where
        F: FnOnce(&[u8]) -> Vec<u8>,
    {
        if let Some(kind) = Self::token_kind(node) {
            if env.codegen() && TK::XHPStringLiteral == kind {
                let p = Self::p_pos(node, env);
                /* for XHP string literals (attribute values) just extract
                value from quotes and decode HTML entities  */
                let text = html_entities::decode(&Self::get_quoted_content(
                    node.full_text(env.source_text()),
                ));
                Ok(ast::Expr::new(p, E_::make_string(text)))
            } else if env.codegen() && TK::XHPBody == kind {
                let p = Self::p_pos(node, env);
                /* for XHP body - only decode HTML entities */
                let text =
                    html_entities::decode(&Self::unesc_xhp(node.full_text(env.source_text())));
                Ok(ast::Expr::new(p, E_::make_string(text)))
            } else {
                let p = Self::p_pos(node, env);
                let s = escaper(node.full_text(env.source_text()));
                Ok(ast::Expr::new(p, E_::make_string(s)))
            }
        } else {
            Self::p_expr(node, env)
        }
    }


    fn p_xhp_attr(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::XhpAttribute> {
        match &node.children {
            XHPSimpleAttribute(c) => {
                let attr_expr = &c.expression;
                let name = Self::p_pstring(&c.name, env)?;
                let expr = if attr_expr.is_braced_expression()
                    && env.file_mode() == file_info::Mode::Mdecl
                    && !env.codegen()
                {
                    ast::Expr::new(env.mk_none_pos(), E_::Null)
                } else {
                    Self::p_xhp_embedded(Self::unesc_xhp_attr, attr_expr, env)?
                };
                Ok(ast::XhpAttribute::XhpSimple(name, expr))
            }
            XHPSpreadAttribute(c) => {
                let expr = Self::p_xhp_embedded(Self::unesc_xhp, &c.expression, env)?;
                Ok(ast::XhpAttribute::XhpSpread(expr))
            }
            _ => Self::missing_syntax("XHP attribute", node, env),
        }
    }

    fn aggregate_xhp_tokens(env: &mut Env<'a, TF>, nodes: S<'a, T, V>) -> Result<Vec<S<'a, T, V>>> {
        let nodes = nodes.syntax_node_to_list_skip_separator();
        let mut state = (None, None, vec![]); // (start, end, result)
        let mut combine =
            |state: &mut (Option<S<'a, T, V>>, Option<S<'a, T, V>>, Vec<S<'a, T, V>>)| {
                match (state.0, state.1) {
                    (Some(s), None) => state.2.push(s),
                    (Some(s), Some(e)) => {
                        let token = env
                            .token_factory
                            .concatenate(s.get_token().unwrap(), e.get_token().unwrap());
                        let node = env.arena.alloc(<Syntax<T, V>>::make_token(token));
                        state.2.push(node)
                    }
                    _ => {}
                }
                state.0 = None;
                state.1 = None;
                Ok(())
            };
        for n in nodes {
            match &n.children {
                Token(t) if t.kind() == TK::XHPComment => {
                    if state.0.is_some() {
                        combine(&mut state)?;
                    }
                }
                Token(_) => {
                    if state.0.is_none() {
                        state.0 = Some(n)
                    } else {
                        state.1 = Some(n)
                    }
                }
                _ => {
                    combine(&mut state)?;
                    state.2.push(n);
                }
            }
        }
        combine(&mut state)?;
        Ok(state.2)
    }

    fn p_bop(
        pos: Pos,
        node: S<'a, T, V>,
        lhs: ast::Expr,
        rhs: ast::Expr,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::Expr_> {
        use ast::Bop::*;
        let mk = |op, l, r| Ok(E_::mk_binop(op, l, r));
        let mk_eq = |op, l, r| Ok(E_::mk_binop(Eq(Some(Box::new(op))), l, r));
        match Self::token_kind(node) {
            Some(TK::Equal) => mk(Eq(None), lhs, rhs),
            Some(TK::Bar) => mk(Bar, lhs, rhs),
            Some(TK::Ampersand) => mk(Amp, lhs, rhs),
            Some(TK::Plus) => mk(Plus, lhs, rhs),
            Some(TK::Minus) => mk(Minus, lhs, rhs),
            Some(TK::Star) => mk(Star, lhs, rhs),
            Some(TK::Carat) => mk(Xor, lhs, rhs),
            Some(TK::Slash) => mk(Slash, lhs, rhs),
            Some(TK::Dot) => mk(Dot, lhs, rhs),
            Some(TK::Percent) => mk(Percent, lhs, rhs),
            Some(TK::LessThan) => mk(Lt, lhs, rhs),
            Some(TK::GreaterThan) => mk(Gt, lhs, rhs),
            Some(TK::EqualEqual) => mk(Eqeq, lhs, rhs),
            Some(TK::LessThanEqual) => mk(Lte, lhs, rhs),
            Some(TK::GreaterThanEqual) => mk(Gte, lhs, rhs),
            Some(TK::StarStar) => mk(Starstar, lhs, rhs),
            Some(TK::ExclamationEqual) => mk(Diff, lhs, rhs),
            Some(TK::BarEqual) => mk_eq(Bar, lhs, rhs),
            Some(TK::PlusEqual) => mk_eq(Plus, lhs, rhs),
            Some(TK::MinusEqual) => mk_eq(Minus, lhs, rhs),
            Some(TK::StarEqual) => mk_eq(Star, lhs, rhs),
            Some(TK::StarStarEqual) => mk_eq(Starstar, lhs, rhs),
            Some(TK::SlashEqual) => mk_eq(Slash, lhs, rhs),
            Some(TK::DotEqual) => mk_eq(Dot, lhs, rhs),
            Some(TK::PercentEqual) => mk_eq(Percent, lhs, rhs),
            Some(TK::CaratEqual) => mk_eq(Xor, lhs, rhs),
            Some(TK::AmpersandEqual) => mk_eq(Amp, lhs, rhs),
            Some(TK::BarBar) => mk(Barbar, lhs, rhs),
            Some(TK::AmpersandAmpersand) => mk(Ampamp, lhs, rhs),
            Some(TK::LessThanLessThan) => mk(Ltlt, lhs, rhs),
            Some(TK::GreaterThanGreaterThan) => mk(Gtgt, lhs, rhs),
            Some(TK::EqualEqualEqual) => mk(Eqeqeq, lhs, rhs),
            Some(TK::LessThanLessThanEqual) => mk_eq(Ltlt, lhs, rhs),
            Some(TK::GreaterThanGreaterThanEqual) => mk_eq(Gtgt, lhs, rhs),
            Some(TK::ExclamationEqualEqual) => mk(Diff2, lhs, rhs),
            Some(TK::LessThanEqualGreaterThan) => mk(Cmp, lhs, rhs),
            Some(TK::QuestionQuestion) => mk(QuestionQuestion, lhs, rhs),
            Some(TK::QuestionQuestionEqual) => mk_eq(QuestionQuestion, lhs, rhs),
            /* The ugly duckling; In the FFP, `|>` is parsed as a
             * `BinaryOperator`, whereas the typed AST has separate constructors for
             * Pipe and Binop. This is why we don't just project onto a
             * `bop`, but a `expr -> expr -> expr_`.
             */
            Some(TK::BarGreaterThan) => {
                let lid =
                    ast::Lid::from_counter(pos, env.next_local_id(), special_idents::DOLLAR_DOLLAR);
                Ok(E_::mk_pipe(lid, lhs, rhs))
            }
            Some(TK::QuestionColon) => Ok(E_::mk_eif(lhs, None, rhs)),
            _ => Self::missing_syntax("binary operator", node, env),
        }
    }

    fn p_exprs_with_loc(n: S<'a, T, V>, e: &mut Env<'a, TF>) -> Result<(Pos, Vec<ast::Expr>)> {
        let loc = Self::p_pos(&n, e);
        let p_expr = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::Expr> {
            Self::p_expr_with_loc(ExprLocation::UsingStatement, n, e)
        };
        Ok((loc, Self::could_map(p_expr, n, e)?))
    }

    fn p_stmt_list_(
        pos: &Pos,
        mut nodes: Iter<S<'a, T, V>>,
        env: &mut Env<'a, TF>,
    ) -> Result<Vec<ast::Stmt>> {
        let mut r = vec![];
        loop {
            match nodes.next() {
                Some(n) => match &n.children {
                    UsingStatementFunctionScoped(c) => {
                        let body = Self::p_stmt_list_(pos, nodes, env)?;
                        let f = |e: &mut Env<'a, TF>| {
                            Ok(ast::Stmt::new(
                                pos.clone(),
                                ast::Stmt_::mk_using(ast::UsingStmt {
                                    is_block_scoped: false,
                                    has_await: !c.await_keyword.is_missing(),
                                    exprs: Self::p_exprs_with_loc(&c.expression, e)?,
                                    block: body,
                                }),
                            ))
                        };
                        let using = Self::lift_awaits_in_statement_(f, Either::Right(pos), env)?;
                        r.push(using);
                        break Ok(r);
                    }
                    _ => {
                        r.push(Self::p_stmt(n, env)?);
                    }
                },
                _ => break Ok(r),
            }
        }
    }

    fn handle_loop_body(pos: Pos, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Stmt> {
        let list: Vec<_> = node.syntax_node_to_list_skip_separator().collect();
        let blk: Vec<_> = Self::p_stmt_list_(&pos, list.iter(), env)?
            .into_iter()
            .filter(|stmt| !stmt.1.is_noop())
            .collect();
        let body = if blk.len() == 0 {
            vec![Self::mk_noop(env)]
        } else {
            blk
        };
        Ok(ast::Stmt::new(pos, ast::Stmt_::mk_block(body)))
    }

    fn is_simple_assignment_await_expression(node: S<'a, T, V>) -> bool {
        match &node.children {
            BinaryExpression(c) => {
                Self::token_kind(&c.operator) == Some(TK::Equal)
                    && Self::is_simple_await_expression(&c.right_operand)
            }
            _ => false,
        }
    }

    fn is_simple_await_expression(node: S<'a, T, V>) -> bool {
        match &node.children {
            PrefixUnaryExpression(c) => Self::token_kind(&c.operator) == Some(TK::Await),
            _ => false,
        }
    }

    fn with_new_nonconcurrent_scope<F, R>(f: F, env: &mut Env<'a, TF>) -> R
    where
        F: FnOnce(&mut Env<'a, TF>) -> R,
    {
        let saved_lifted_awaits = env.lifted_awaits.take();
        let result = f(env);
        env.lifted_awaits = saved_lifted_awaits;
        result
    }

    fn with_new_concurrent_scope<F, R>(f: F, env: &mut Env<'a, TF>) -> Result<(LiftedAwaitExprs, R)>
    where
        F: FnOnce(&mut Env<'a, TF>) -> Result<R>,
    {
        let saved_lifted_awaits = env.lifted_awaits.replace(LiftedAwaits {
            awaits: vec![],
            lift_kind: LiftedAwaitKind::LiftedFromConcurrent,
        });
        let result = f(env);
        let lifted_awaits = mem::replace(&mut env.lifted_awaits, saved_lifted_awaits);
        let result = result?;
        let awaits = match lifted_awaits {
            Some(la) => Self::process_lifted_awaits(la)?,
            None => Self::failwith("lifted awaits should not be None")?,
        };
        Ok((awaits, result))
    }

    fn process_lifted_awaits(mut awaits: LiftedAwaits) -> Result<LiftedAwaitExprs> {
        for await_ in awaits.awaits.iter() {
            if (await_.1).0.is_none() {
                return Self::failwith("none pos in lifted awaits");
            }
        }
        awaits
            .awaits
            .sort_unstable_by(|a1, a2| Pos::cmp(&(a1.1).0, &(a2.1).0));
        Ok(awaits.awaits)
    }

    fn clear_statement_scope<F, R>(f: F, env: &mut Env<'a, TF>) -> R
    where
        F: FnOnce(&mut Env<'a, TF>) -> R,
    {
        use LiftedAwaitKind::*;
        match &env.lifted_awaits {
            Some(LiftedAwaits { lift_kind, .. }) if *lift_kind == LiftedFromStatement => {
                let saved_lifted_awaits = env.lifted_awaits.take();
                let result = f(env);
                env.lifted_awaits = saved_lifted_awaits;
                result
            }
            _ => f(env),
        }
    }

    fn lift_awaits_in_statement<F>(
        f: F,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::Stmt>
    where
        F: FnOnce(&mut Env<'a, TF>) -> Result<ast::Stmt>,
    {
        Self::lift_awaits_in_statement_(f, Either::Left(node), env)
    }

    fn lift_awaits_in_statement_<F>(
        f: F,
        pos: Either<S<'a, T, V>, &Pos>,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::Stmt>
    where
        F: FnOnce(&mut Env<'a, TF>) -> Result<ast::Stmt>,
    {
        use LiftedAwaitKind::*;
        let (lifted_awaits, result) = match env.lifted_awaits {
            Some(LiftedAwaits { lift_kind, .. }) if lift_kind == LiftedFromConcurrent => {
                (None, f(env)?)
            }
            _ => {
                let saved = env.lifted_awaits.replace(LiftedAwaits {
                    awaits: vec![],
                    lift_kind: LiftedFromStatement,
                });
                let result = f(env);
                let lifted_awaits = mem::replace(&mut env.lifted_awaits, saved);
                let result = result?;
                (lifted_awaits, result)
            }
        };
        if let Some(lifted_awaits) = lifted_awaits {
            if !lifted_awaits.awaits.is_empty() {
                let awaits = Self::process_lifted_awaits(lifted_awaits)?;
                let pos = match pos {
                    Either::Left(n) => Self::p_pos(n, env),
                    Either::Right(p) => p.clone(),
                };
                return Ok(ast::Stmt::new(
                    pos,
                    ast::Stmt_::mk_awaitall(awaits, vec![result]),
                ));
            }
        }
        Ok(result)
    }

    fn lift_await(
        parent_pos: Pos,
        expr: ast::Expr,
        env: &mut Env<'a, TF>,
        location: ExprLocation,
    ) -> Result<ast::Expr_> {
        use ExprLocation::*;
        match (&env.lifted_awaits, location) {
            (_, UsingStatement) | (_, RightOfAssignmentInUsingStatement) | (None, _) => {
                Ok(E_::mk_await(expr))
            }
            (Some(_), _) => {
                if location != AsStatement {
                    let name = env.make_tmp_var_name();
                    let lid = ast::Lid::new(parent_pos, name.clone());
                    let await_lid = ast::Lid::new(expr.0.clone(), name);
                    let await_ = (Some(await_lid), expr);
                    env.lifted_awaits.as_mut().map(|aw| aw.awaits.push(await_));
                    Ok(E_::mk_lvar(lid))
                } else {
                    env.lifted_awaits
                        .as_mut()
                        .map(|aw| aw.awaits.push((None, expr)));
                    Ok(E_::Null)
                }
            }
        }
    }

    fn p_stmt(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Stmt> {
        Self::clear_statement_scope(
            |e: &mut Env<'a, TF>| {
                let docblock = Self::extract_docblock(node, e);
                e.push_docblock(docblock);
                let result = Self::p_stmt_(node, e);
                e.pop_docblock();
                result
            },
            env,
        )
    }

    fn p_stmt_(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Stmt> {
        let pos = Self::p_pos(node, env);
        use ast::{Stmt, Stmt_ as S_};
        let new = Stmt::new;
        match &node.children {
            SwitchStatement(c) => {
                let p_label = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::Case> {
                    match &n.children {
                        CaseLabel(c) => {
                            Ok(ast::Case::Case(Self::p_expr(&c.expression, e)?, vec![]))
                        }
                        DefaultLabel(_) => Ok(ast::Case::Default(Self::p_pos(n, e), vec![])),
                        _ => Self::missing_syntax("switch label", n, e),
                    }
                };
                let p_section = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<Vec<ast::Case>> {
                    match &n.children {
                        SwitchSection(c) => {
                            let mut blk = Self::could_map(Self::p_stmt, &c.statements, e)?;
                            if !c.fallthrough.is_missing() {
                                blk.push(new(e.mk_none_pos(), S_::Fallthrough));
                            }
                            let mut labels = Self::could_map(p_label, &c.labels, e)?;
                            match labels.last_mut() {
                                Some(ast::Case::Default(_, b)) => *b = blk,
                                Some(ast::Case::Case(_, b)) => *b = blk,
                                _ => Self::raise_parsing_error(n, e, "Malformed block result"),
                            }
                            Ok(labels)
                        }
                        _ => Self::missing_syntax("switch section", n, e),
                    }
                };
                let f = |env: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    Ok(new(
                        pos,
                        S_::mk_switch(
                            Self::p_expr(&c.expression, env)?,
                            itertools::concat(Self::could_map(p_section, &c.sections, env)?),
                        ),
                    ))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            IfStatement(c) => {
                let p_else_if =
                    |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<(ast::Expr, ast::Block)> {
                        match &n.children {
                            ElseifClause(c) => Ok((
                                Self::p_expr(&c.condition, e)?,
                                Self::p_block(true, &c.statement, e)?,
                            )),
                            _ => Self::missing_syntax("elseif clause", n, e),
                        }
                    };
                let f = |env: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    let condition = Self::p_expr(&c.condition, env)?;
                    let statement = Self::p_block(true /* remove noop */, &c.statement, env)?;
                    let else_ = match &c.else_clause.children {
                        ElseClause(c) => Self::p_block(true, &c.statement, env)?,
                        Missing => vec![Self::mk_noop(env)],
                        _ => Self::missing_syntax("else clause", &c.else_clause, env)?,
                    };
                    let else_ifs = Self::could_map(p_else_if, &c.elseif_clauses, env)?;
                    let else_if = else_ifs
                        .into_iter()
                        .rev()
                        .fold(else_, |child, (cond, stmts)| {
                            vec![new(pos.clone(), S_::mk_if(cond, stmts, child))]
                        });
                    Ok(new(pos, S_::mk_if(condition, statement, else_if)))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            ExpressionStatement(c) => {
                let expr = &c.expression;
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    if expr.is_missing() {
                        Ok(new(pos, S_::Noop))
                    } else {
                        Ok(new(
                            pos,
                            S_::mk_expr(Self::p_expr_with_loc(ExprLocation::AsStatement, expr, e)?),
                        ))
                    }
                };
                if Self::is_simple_assignment_await_expression(expr)
                    || Self::is_simple_await_expression(expr)
                {
                    f(env)
                } else {
                    Self::lift_awaits_in_statement(f, node, env)
                }
            }
            CompoundStatement(c) => Self::handle_loop_body(pos, &c.statements, env),
            SyntaxList(_) => Self::handle_loop_body(pos, node, env),
            ThrowStatement(c) => Self::lift_awaits_in_statement(
                |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    Ok(new(pos, S_::mk_throw(Self::p_expr(&c.expression, e)?)))
                },
                node,
                env,
            ),
            DoStatement(c) => Ok(new(
                pos,
                S_::mk_do(
                    Self::p_block(false /* remove noop */, &c.body, env)?,
                    Self::p_expr(&c.condition, env)?,
                ),
            )),
            WhileStatement(c) => Ok(new(
                pos,
                S_::mk_while(
                    Self::p_expr(&c.condition, env)?,
                    Self::p_block(true, &c.body, env)?,
                ),
            )),
            UsingStatementBlockScoped(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    Ok(new(
                        pos,
                        S_::mk_using(ast::UsingStmt {
                            is_block_scoped: true,
                            has_await: !&c.await_keyword.is_missing(),
                            exprs: Self::p_exprs_with_loc(&c.expressions, e)?,
                            block: Self::p_block(false, &c.body, e)?,
                        }),
                    ))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            UsingStatementFunctionScoped(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    Ok(new(
                        pos,
                        S_::mk_using(ast::UsingStmt {
                            is_block_scoped: false,
                            has_await: !&c.await_keyword.is_missing(),
                            exprs: Self::p_exprs_with_loc(&c.expression, e)?,
                            block: vec![Self::mk_noop(e)],
                        }),
                    ))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            ForStatement(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    let ini = Self::p_expr_l(&c.initializer, e)?;
                    let ctr = Self::mp_optional(Self::p_expr, &c.control, e)?;
                    let eol = Self::p_expr_l(&c.end_of_loop, e)?;
                    let blk = Self::p_block(true, &c.body, e)?;
                    Ok(Stmt::new(pos, S_::mk_for(ini, ctr, eol, blk)))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            ForeachStatement(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    let col = Self::p_expr(&c.collection, e)?;
                    let akw = match Self::token_kind(&c.await_keyword) {
                        Some(TK::Await) => Some(Self::p_pos(&c.await_keyword, e)),
                        _ => None,
                    };
                    let value = Self::p_expr(&c.value, e)?;
                    let akv = match (akw, &c.key.children) {
                        (Some(p), Missing) => ast::AsExpr::AwaitAsV(p, value),
                        (None, Missing) => ast::AsExpr::AsV(value),
                        (Some(p), _) => ast::AsExpr::AwaitAsKv(p, Self::p_expr(&c.key, e)?, value),
                        (None, _) => ast::AsExpr::AsKv(Self::p_expr(&c.key, e)?, value),
                    };
                    let blk = Self::p_block(true, &c.body, e)?;
                    Ok(new(pos, S_::mk_foreach(col, akv, blk)))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            TryStatement(c) => Ok(new(
                pos,
                S_::mk_try(
                    Self::p_block(false, &c.compound_statement, env)?,
                    Self::could_map(
                        |n: S<'a, T, V>, e| match &n.children {
                            CatchClause(c) => Ok(ast::Catch(
                                Self::pos_name(&c.type_, e)?,
                                Self::lid_from_name(&c.variable, e)?,
                                Self::p_block(true, &c.body, e)?,
                            )),
                            _ => Self::missing_syntax("catch clause", n, e),
                        },
                        &c.catch_clauses,
                        env,
                    )?,
                    match &c.finally_clause.children {
                        FinallyClause(c) => Self::p_block(false, &c.body, env)?,
                        _ => vec![],
                    },
                ),
            )),
            ReturnStatement(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    let expr = match &c.expression.children {
                        Missing => None,
                        _ => Some(Self::p_expr_with_loc(
                            ExprLocation::RightOfReturn,
                            &c.expression,
                            e,
                        )?),
                    };
                    Ok(ast::Stmt::new(pos, ast::Stmt_::mk_return(expr)))
                };
                if Self::is_simple_await_expression(&c.expression) {
                    f(env)
                } else {
                    Self::lift_awaits_in_statement(f, node, env)
                }
            }
            YieldBreakStatement(_) => Ok(ast::Stmt::new(pos, ast::Stmt_::mk_yield_break())),
            EchoStatement(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    let echo = match &c.keyword.children {
                        QualifiedName(_) | SimpleTypeSpecifier(_) | Token(_) => {
                            let name = Self::pos_name(&c.keyword, e)?;
                            ast::Expr::new(name.0.clone(), E_::mk_id(name))
                        }
                        _ => Self::missing_syntax("id", &c.keyword, e)?,
                    };
                    let args = Self::could_map(Self::p_expr, &c.expressions, e)?;
                    Ok(new(
                        pos.clone(),
                        S_::mk_expr(ast::Expr::new(pos, E_::mk_call(echo, vec![], args, None))),
                    ))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            UnsetStatement(c) => {
                let f = |e: &mut Env<'a, TF>| -> Result<ast::Stmt> {
                    let args = Self::could_map(Self::p_expr, &c.variables, e)?;
                    if e.parser_options.po_disable_unset_class_const {
                        args.iter()
                            .for_each(|arg| Self::check_mutate_class_const(arg, node, e))
                    }
                    let unset = match &c.keyword.children {
                        QualifiedName(_) | SimpleTypeSpecifier(_) | Token(_) => {
                            let name = Self::pos_name(&c.keyword, e)?;
                            ast::Expr::new(name.0.clone(), E_::mk_id(name))
                        }
                        _ => Self::missing_syntax("id", &c.keyword, e)?,
                    };
                    Ok(new(
                        pos.clone(),
                        S_::mk_expr(ast::Expr::new(pos, E_::mk_call(unset, vec![], args, None))),
                    ))
                };
                Self::lift_awaits_in_statement(f, node, env)
            }
            BreakStatement(_) => Ok(new(pos, S_::Break)),
            ContinueStatement(_) => Ok(new(pos, S_::Continue)),
            ConcurrentStatement(c) => {
                let keyword_pos = Self::p_pos(&c.keyword, env);
                let (lifted_awaits, Stmt(stmt_pos, stmt)) = Self::with_new_concurrent_scope(
                    |e: &mut Env<'a, TF>| Self::p_stmt(&c.statement, e),
                    env,
                )?;
                let stmt = match stmt {
                    S_::Block(stmts) => {
                        use ast::Bop::Eq;
                        use ast::Expr as E;
                        /* Reuse tmp vars from lifted_awaits, this is safe because there will
                         * always be more awaits with tmp vars than statements with assignments */
                        let mut tmp_vars = lifted_awaits
                            .iter()
                            .filter_map(|lifted_await| lifted_await.0.as_ref().map(|x| &x.1));
                        let mut body_stmts = vec![];
                        let mut assign_stmts = vec![];
                        for n in stmts.into_iter() {
                            if !n.is_assign_expr() {
                                body_stmts.push(n);
                                continue;
                            }

                            if let Some(tv) = tmp_vars.next() {
                                if let Stmt(p1, S_::Expr(expr)) = n {
                                    if let E(p2, E_::Binop(bop)) = *expr {
                                        if let (Eq(op), e1, e2) = *bop {
                                            let tmp_n = E::mk_lvar(&e2.0, &(tv.1));
                                            if tmp_n.lvar_name() != e2.lvar_name() {
                                                let new_n = new(
                                                    p1.clone(),
                                                    S_::mk_expr(E::new(
                                                        p2.clone(),
                                                        E_::mk_binop(
                                                            Eq(None),
                                                            tmp_n.clone(),
                                                            e2.clone(),
                                                        ),
                                                    )),
                                                );
                                                body_stmts.push(new_n);
                                            }
                                            let assign_stmt = new(
                                                p1,
                                                S_::mk_expr(E::new(
                                                    p2,
                                                    E_::mk_binop(Eq(op), e1, tmp_n),
                                                )),
                                            );
                                            assign_stmts.push(assign_stmt);
                                            continue;
                                        }
                                    }
                                }

                                Self::failwith("Expect assignment stmt")?;
                            } else {
                                Self::raise_parsing_error_pos(
                                    &stmt_pos,
                                    env,
                                    &syntax_error::statement_without_await_in_concurrent_block,
                                );
                                body_stmts.push(n)
                            }
                        }
                        body_stmts.append(&mut assign_stmts);
                        new(stmt_pos, S_::mk_block(body_stmts))
                    }
                    _ => Self::failwith("Unexpected concurrent stmt structure")?,
                };
                Ok(new(keyword_pos, S_::mk_awaitall(lifted_awaits, vec![stmt])))
            }
            MarkupSection(_) => Self::p_markup(node, env),
            _ => Self::missing_syntax_(
                Some(new(env.mk_none_pos(), S_::Noop)),
                "statement",
                node,
                env,
            ),
        }
    }
    fn check_mutate_class_const(e: &ast::Expr, node: S<'a, T, V>, env: &mut Env<'a, TF>) {
        match &e.1 {
            E_::ArrayGet(c) if c.1.is_some() => Self::check_mutate_class_const(&c.0, node, env),
            E_::ClassConst(_) => {
                Self::raise_parsing_error(node, env, &syntax_error::const_mutation)
            }
            _ => {}
        }
    }

    fn is_hashbang(node: S<'a, T, V>, env: &Env<TF>) -> bool {
        let text = Self::text_str(node, env);
        lazy_static! {
            static ref RE: regex::Regex = regex::Regex::new("^#!.*\n").unwrap();
        }
        text.lines().nth(1).is_none() && // only one line
        RE.is_match(text)
    }

    fn p_markup(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Stmt> {
        match &node.children {
            MarkupSection(c) => {
                let markup_hashbang = &c.hashbang;
                let pos = Self::p_pos(node, env);
                let f = pos.filename();
                if (f.has_extension("hack") || f.has_extension("hackpartial"))
                    && !(&c.suffix.is_missing())
                {
                    let ext = f.path().extension().unwrap(); // has_extension ensures this is a Some
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::error1060(&ext.to_str().unwrap()),
                    );
                } else if markup_hashbang.width() > 0 && !Self::is_hashbang(markup_hashbang, env) {
                    Self::raise_parsing_error(node, env, &syntax_error::error1001);
                }
                let stmt_ = ast::Stmt_::mk_markup((pos.clone(), Self::text(markup_hashbang, env)));
                Ok(ast::Stmt::new(pos, stmt_))
            }
            _ => Self::failwith("invalid node"),
        }
    }

    fn p_modifiers<F: Fn(R, modifier::Kind) -> R, R>(
        on_kind: F,
        mut init: R,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(modifier::KindSet, R)> {
        let mut kind_set = modifier::KindSet::new();
        for n in node.syntax_node_to_list_skip_separator() {
            let token_kind = Self::token_kind(n).map_or(None, modifier::from_token_kind);
            match token_kind {
                Some(kind) => {
                    kind_set.add(kind);
                    init = on_kind(init, kind);
                }
                _ => Self::missing_syntax("kind", n, env)?,
            }
        }
        Ok((kind_set, init))
    }

    fn p_kinds(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<modifier::KindSet> {
        Self::p_modifiers(|_, _| {}, (), node, env).map(|r| r.0)
    }

    fn could_map<R, F>(f: F, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<Vec<R>>
    where
        F: Fn(S<'a, T, V>, &mut Env<'a, TF>) -> Result<R>,
    {
        Self::map_flatten_(f, node, env, vec![])
    }

    fn map_flatten_<R, F>(
        f: F,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        acc: Vec<R>,
    ) -> Result<Vec<R>>
    where
        F: Fn(S<'a, T, V>, &mut Env<'a, TF>) -> Result<R>,
    {
        let op = |mut v: Vec<R>, a| {
            v.push(a);
            v
        };
        Self::map_fold(&f, &op, node, env, acc)
    }

    fn could_map_filter<R, F>(
        f: F,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(Vec<R>, bool)>
    where
        F: Fn(S<'a, T, V>, &mut Env<'a, TF>) -> Result<Option<R>>,
    {
        Self::map_flatten_filter_(f, node, env, (vec![], false))
    }

    #[inline]
    fn map_flatten_filter_<R, F>(
        f: F,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        acc: (Vec<R>, bool),
    ) -> Result<(Vec<R>, bool)>
    where
        F: Fn(S<'a, T, V>, &mut Env<'a, TF>) -> Result<Option<R>>,
    {
        let op = |mut v: (Vec<R>, bool), a| -> (Vec<R>, bool) {
            match a {
                Option::None => (v.0, true),
                Option::Some(a) => {
                    v.0.push(a);
                    v
                }
            }
        };
        Self::map_fold(&f, &op, node, env, acc)
    }

    fn map_fold<A, R, F, O>(
        f: &F,
        op: &O,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        mut acc: A,
    ) -> Result<A>
    where
        F: Fn(S<'a, T, V>, &mut Env<'a, TF>) -> Result<R>,
        O: Fn(A, R) -> A,
    {
        for n in node.syntax_node_to_list_skip_separator() {
            acc = op(acc, f(n, env)?);
        }
        Ok(acc)
    }

    fn p_visibility(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<Option<ast::Visibility>> {
        let first_vis =
            |r: Option<ast::Visibility>, kind| r.or_else(|| modifier::to_visibility(kind));
        Self::p_modifiers(first_vis, None, node, env).map(|r| r.1)
    }

    fn p_visibility_or(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        default: ast::Visibility,
    ) -> Result<ast::Visibility> {
        Self::p_visibility(node, env).map(|v| v.unwrap_or(default))
    }

    fn p_visibility_last_win(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Option<ast::Visibility>> {
        let last_vis = |r, kind| modifier::to_visibility(kind).or(r);
        Self::p_modifiers(last_vis, None, node, env).map(|r| r.1)
    }

    fn p_visibility_last_win_or(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
        default: ast::Visibility,
    ) -> Result<ast::Visibility> {
        Self::p_visibility_last_win(node, env).map(|v| v.unwrap_or(default))
    }

    fn has_soft(attrs: &[ast::UserAttribute]) -> bool {
        attrs.iter().any(|attr| attr.name.1 == special_attrs::SOFT)
    }

    fn soften_hint(attrs: &[ast::UserAttribute], hint: ast::Hint) -> ast::Hint {
        if Self::has_soft(attrs) {
            ast::Hint::new(hint.0.clone(), ast::Hint_::Hsoft(hint))
        } else {
            hint
        }
    }

    fn has_polymorphic_context(contexts: &Option<ast::Contexts>) -> bool {
        use ast::Hint_::{Haccess, HfunContext, Hvar};
        if let Some(ast::Contexts(_, ref context_hints)) = contexts {
            return context_hints.iter().any(|c| match *c.1 {
                HfunContext(_) => true,
                Haccess(ref root, _) => {
                    if let Hvar(_) = *root.1 {
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            });
        } else {
            false
        }
    }

    fn rewrite_effect_polymorphism(
        env: &mut Env<'a, TF>,
        params: &mut Vec<ast::FunParam>,
        tparams: &mut Vec<ast::Tparam>,
        contexts: &Option<ast::Contexts>,
        where_constraints: &mut Vec<ast::WhereConstraintHint>,
    ) {
        use ast::{Hint, Hint_, ReifyKind, Variance};
        use Hint_::{Haccess, Happly, HfunContext, Hvar};

        if !Self::has_polymorphic_context(contexts) {
            return;
        }
        let ast::Contexts(ref _p, ref context_hints) = contexts.as_ref().unwrap();
        let tp = |name, v| ast::Tparam {
            variance: Variance::Invariant,
            name,
            parameters: vec![],
            constraints: v,
            reified: ReifyKind::Erased,
            user_attributes: vec![],
        };

        // For polymorphic context with form `ctx $f`
        // require that `(function (ts)[_]: t) $f` exists
        // rewrite as `(function (ts)[ctx $f]: t) $f`
        // add a type parameter named "Tctx$f"
        let rewrite_fun_ctx =
            |env: &mut Env<'a, TF>, tparams: &mut Vec<ast::Tparam>, hint: &mut Hint, name: &str| {
                let mut invalid = |p| {
                    Self::raise_parsing_error_pos(
                        p,
                        env,
                        &syntax_error::ctx_fun_invalid_type_hint(name),
                    )
                };
                match *hint.1 {
                    Hint_::Hfun(ref mut hf) => {
                        if let Some(ast::Contexts(ref p, ref mut hl)) = &mut hf.ctxs {
                            if let [ref mut h] = *hl.as_mut_slice() {
                                if let Hint_::Happly(ast::Id(ref pos, s), _) = &*h.1 {
                                    if s == "_" {
                                        *h.1 = Hint_::HfunContext(name.to_string());
                                        tparams.push(tp(
                                            ast::Id(h.0.clone(), "Tctx".to_string() + name),
                                            vec![],
                                        ));
                                    } else {
                                        invalid(pos);
                                    }
                                } else {
                                    invalid(p);
                                }
                            } else {
                                invalid(p);
                            }
                        } else {
                            invalid(&hint.0);
                        }
                    }
                    _ => invalid(&hint.0),
                }
            };

        // For polymorphic context with form `$g::C`
        // if $g's type is not a type parameter
        //   add one named "T$g" constrained by $g's type
        //   replace $g's type hint
        // let Tg denote $g's final type (must be a type parameter).
        // add a type parameter "T$g@C"
        // add a where constraint T$g@C = Tg :: C
        let rewrite_arg_ctx = |
            env: &mut Env<'a, TF>,
            tparams: &mut Vec<ast::Tparam>,
            where_constraints: &mut Vec<ast::WhereConstraintHint>,
            hint: &mut Hint,
            param_pos: &Pos,
            name: &str,
            context_pos: &Pos,
            cst: &ast::Id,
        | match *hint.1 {
            Happly(ast::Id(_, ref type_name), _) => {
                if !tparams.iter().any(|h| h.name.1 == *type_name) {
                    // If the parameter is X $g, create tparam `T$g as X` and replace $g's type hint
                    let id = ast::Id(param_pos.clone(), "T".to_string() + name);
                    tparams.push(tp(
                        id.clone(),
                        vec![(ast::ConstraintKind::ConstraintAs, hint.clone())],
                    ));
                    *hint = ast::Hint::new(param_pos.clone(), Happly(id, vec![]));
                };
                let right = ast::Hint::new(
                    context_pos.clone(),
                    Haccess(hint.clone(), vec![cst.clone()]),
                );
                let left_id = ast::Id(
                    context_pos.clone(),
                    // IMPORTANT: using `::` here will not work, because
                    // Typing_taccess constructs its own fake type parameter
                    // for the Taccess with `::`. So, if the two type parameters
                    // are named `Tprefix` and `Tprefix::Const`, the latter
                    // will collide with the one generated for `Tprefix`::Const
                    "T".to_string() + name + "@" + &cst.1,
                );
                tparams.push(tp(left_id.clone(), vec![]));
                let left = ast::Hint::new(context_pos.clone(), Happly(left_id, vec![]));
                where_constraints.push(ast::WhereConstraintHint(
                    left,
                    ast::ConstraintKind::ConstraintEq,
                    right,
                ))
            }
            _ => Self::raise_parsing_error_pos(
                &hint.0,
                env,
                &syntax_error::ctx_var_invalid_type_hint(name),
            ),
        };

        let mut hint_by_param: std::collections::HashMap<
            &str,
            (&mut Option<ast::Hint>, &Pos, aast::IsVariadic),
            std::hash::BuildHasherDefault<fnv::FnvHasher>,
        > = fnv::FnvHashMap::default();
        for param in params.iter_mut() {
            hint_by_param.insert(
                param.name.as_ref(),
                (&mut param.type_hint.1, &param.pos, param.is_variadic),
            );
        }


        for context_hint in context_hints {
            match *context_hint.1 {
                HfunContext(ref name) => match hint_by_param.get_mut::<str>(name) {
                    Some((hint_opt, param_pos, _is_variadic)) => match hint_opt {
                        Some(ref mut param_hint) => match *param_hint.1 {
                            Hint_::Hoption(ref mut h) => rewrite_fun_ctx(env, tparams, h, name),
                            _ => rewrite_fun_ctx(env, tparams, param_hint, name),
                        },
                        None => Self::raise_parsing_error_pos(
                            param_pos,
                            env,
                            &syntax_error::ctx_var_missing_type_hint(name),
                        ),
                    },

                    None => Self::raise_parsing_error_pos(
                        &context_hint.0,
                        env,
                        &syntax_error::ctx_var_invalid_parameter(name),
                    ),
                },
                Haccess(ref root, ref csts) => {
                    if let Hvar(ref name) = *root.1 {
                        match hint_by_param.get_mut::<str>(name) {
                            Some((hint_opt, param_pos, is_variadic)) => {
                                if *is_variadic {
                                    Self::raise_parsing_error_pos(
                                        param_pos,
                                        env,
                                        &syntax_error::ctx_var_variadic(name),
                                    )
                                } else {
                                    match hint_opt {
                                        Some(ref mut param_hint) => match *param_hint.1 {
                                            Hint_::Hoption(ref mut h) => rewrite_arg_ctx(
                                                env,
                                                tparams,
                                                where_constraints,
                                                h,
                                                param_pos,
                                                name,
                                                &context_hint.0,
                                                &csts[0],
                                            ),
                                            _ => rewrite_arg_ctx(
                                                env,
                                                tparams,
                                                where_constraints,
                                                param_hint,
                                                param_pos,
                                                name,
                                                &context_hint.0,
                                                &csts[0],
                                            ),
                                        },
                                        None => Self::raise_parsing_error_pos(
                                            param_pos,
                                            env,
                                            &syntax_error::ctx_var_missing_type_hint(name),
                                        ),
                                    }
                                }
                            }
                            None => Self::raise_parsing_error_pos(
                                &root.0,
                                env,
                                &syntax_error::ctx_var_invalid_parameter(name),
                            ),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn p_fun_param_default_value(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Option<ast::Expr>> {
        match &node.children {
            SimpleInitializer(c) => Self::mp_optional(Self::p_expr, &c.value, env),
            _ => Ok(None),
        }
    }

    fn p_param_kind(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::ParamKind> {
        match Self::token_kind(node) {
            Some(TK::Inout) => Ok(ast::ParamKind::Pinout),
            _ => Self::missing_syntax("param kind", node, env),
        }
    }

    fn param_template(node: S<'a, T, V>, env: &Env<TF>) -> ast::FunParam {
        let pos = Self::p_pos(node, env);
        ast::FunParam {
            annotation: pos.clone(),
            type_hint: ast::TypeHint((), None),
            is_variadic: false,
            pos,
            name: Self::text(node, env),
            expr: None,
            callconv: None,
            user_attributes: vec![],
            visibility: None,
        }
    }

    fn p_fun_param(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::FunParam> {
        match &node.children {
            ParameterDeclaration(ParameterDeclarationChildren {
                attribute,
                visibility,
                call_convention,
                type_,
                name,
                default_value,
            }) => {
                let (is_variadic, name) = match &name.children {
                    DecoratedExpression(DecoratedExpressionChildren {
                        decorator,
                        expression,
                    }) => {
                        let decorator = Self::text_str(decorator, env);
                        match &expression.children {
                            DecoratedExpression(c) => {
                                let nested_expression = &c.expression;
                                let nested_decorator = Self::text_str(&c.decorator, env);
                                (
                                    decorator == "..." || nested_decorator == "...",
                                    nested_expression,
                                )
                            }
                            _ => (decorator == "...", expression),
                        }
                    }
                    _ => (false, name),
                };
                let user_attributes = Self::p_user_attributes(attribute, env)?;
                let pos = Self::p_pos(name, env);
                let name = Self::text(name, env);
                let hint = Self::mp_optional(Self::p_hint, type_, env)?;
                let hint = hint.map(|h| Self::soften_hint(&user_attributes, h));

                if is_variadic && !user_attributes.is_empty() {
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::no_attributes_on_variadic_parameter,
                    );
                }
                Ok(ast::FunParam {
                    annotation: pos.clone(),
                    type_hint: ast::TypeHint((), hint),
                    user_attributes,
                    is_variadic,
                    pos,
                    name,
                    expr: Self::p_fun_param_default_value(default_value, env)?,
                    callconv: Self::mp_optional(Self::p_param_kind, call_convention, env)?,
                    /* implicit field via constructor parameter.
                     * This is always None except for constructors and the modifier
                     * can be only Public or Protected or Private.
                     */
                    visibility: Self::p_visibility(visibility, env)?,
                })
            }
            VariadicParameter(_) => {
                let mut param = Self::param_template(node, env);
                param.is_variadic = true;
                Ok(param)
            }
            Token(_) if Self::text_str(node, env) == "..." => {
                let mut param = Self::param_template(node, env);
                param.is_variadic = true;
                Ok(param)
            }
            _ => Self::missing_syntax("function parameter", node, env),
        }
    }

    fn p_tconstraint_ty(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Hint> {
        match &node.children {
            TypeConstraint(c) => Self::p_hint(&c.type_, env),
            _ => Self::missing_syntax("type constraint", node, env),
        }
    }

    fn p_tconstraint(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(ast::ConstraintKind, ast::Hint)> {
        match &node.children {
            TypeConstraint(c) => Ok((
                match Self::token_kind(&c.keyword) {
                    Some(TK::As) => ast::ConstraintKind::ConstraintAs,
                    Some(TK::Super) => ast::ConstraintKind::ConstraintSuper,
                    Some(TK::Equal) => ast::ConstraintKind::ConstraintEq,
                    _ => Self::missing_syntax("constraint operator", &c.keyword, env)?,
                },
                Self::p_hint(&c.type_, env)?,
            )),
            _ => Self::missing_syntax("type constraint", node, env),
        }
    }

    fn p_tparam(is_class: bool, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Tparam> {
        match &node.children {
            TypeParameter(TypeParameterChildren {
                attribute_spec,
                reified,
                variance,
                name,
                param_params,
                constraints,
            }) => {
                let user_attributes = Self::p_user_attributes(attribute_spec, env)?;
                let is_reified = !reified.is_missing();
                if is_class && is_reified {
                    let type_name = Self::text(name, env);
                    env.cls_reified_generics().insert(type_name);
                }
                let variance = match Self::token_kind(variance) {
                    Some(TK::Plus) => ast::Variance::Covariant,
                    Some(TK::Minus) => ast::Variance::Contravariant,
                    _ => ast::Variance::Invariant,
                };
                if is_reified && variance != ast::Variance::Invariant {
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::non_invariant_reified_generic,
                    );
                }
                let reified = match (is_reified, Self::has_soft(&user_attributes)) {
                    (true, true) => ast::ReifyKind::SoftReified,
                    (true, false) => ast::ReifyKind::Reified,
                    _ => ast::ReifyKind::Erased,
                };
                let parameters = Self::p_tparam_l(is_class, param_params, env)?;
                Ok(ast::Tparam {
                    variance,
                    name: Self::pos_name(name, env)?,
                    parameters,
                    constraints: Self::could_map(Self::p_tconstraint, constraints, env)?,
                    reified,
                    user_attributes,
                })
            }
            _ => Self::missing_syntax("type parameter", node, env),
        }
    }

    fn p_tparam_l(
        is_class: bool,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Vec<ast::Tparam>> {
        match &node.children {
            Missing => Ok(vec![]),
            TypeParameters(c) => {
                Self::could_map(|n, e| Self::p_tparam(is_class, n, e), &c.parameters, env)
            }
            _ => Self::missing_syntax("type parameter", node, env),
        }
    }

    fn p_contexts(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(Option<ast::Contexts>, Option<ast::Contexts>)> {
        match &node.children {
            Missing => Ok((None, None)),
            Contexts(c) => {
                let hints = Self::could_map(&Self::p_hint, &c.types, env)?;
                let pos = Self::p_pos(node, env);
                let ctxs = ast::Contexts(pos, hints);
                let unsafe_ctxs = ctxs.clone();
                Ok((Some(ctxs), Some(unsafe_ctxs)))
            }
            _ => Self::missing_syntax("contexts", node, env),
        }
    }

    fn p_context_list_to_intersection(
        ctx_list: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Option<ast::Hint>> {
        Ok(Self::mp_optional(Self::p_contexts, &ctx_list, env)?
            .and_then(|t| t.0)
            .map(|t| ast::Hint::new(t.0, ast::Hint_::Hintersection(t.1))))
    }

    fn p_fun_hdr(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<FunHdr> {
        match &node.children {
            FunctionDeclarationHeader(FunctionDeclarationHeaderChildren {
                modifiers,
                name,
                where_clause,
                type_parameter_list,
                parameter_list,
                type_,
                contexts,
                ..
            }) => {
                if name.value.is_missing() {
                    Self::raise_parsing_error(name, env, &syntax_error::empty_method_name);
                }
                let kinds = Self::p_kinds(modifiers, env)?;
                let has_async = kinds.has(modifier::ASYNC);
                let mut type_parameters = Self::p_tparam_l(false, type_parameter_list, env)?;
                let mut parameters = Self::could_map(Self::p_fun_param, parameter_list, env)?;
                let (contexts, unsafe_contexts) = Self::p_contexts(contexts, env)?;
                let mut constrs = Self::p_where_constraint(false, node, where_clause, env)?;
                Self::rewrite_effect_polymorphism(
                    env,
                    &mut parameters,
                    &mut type_parameters,
                    &contexts,
                    &mut constrs,
                );
                let return_type = Self::mp_optional(Self::p_hint, type_, env)?;
                let suspension_kind = Self::mk_suspension_kind_(has_async);
                let name = Self::pos_name(name, env)?;
                Ok(FunHdr {
                    suspension_kind,
                    name,
                    constrs,
                    type_parameters,
                    parameters,
                    contexts,
                    unsafe_contexts,
                    return_type,
                })
            }
            LambdaSignature(LambdaSignatureChildren {
                parameters,
                contexts,
                type_,
                ..
            }) => {
                let mut header = FunHdr::make_empty(env);
                header.parameters = Self::could_map(Self::p_fun_param, parameters, env)?;
                let (contexts, unsafe_contexts) = Self::p_contexts(contexts, env)?;
                header.contexts = contexts;
                header.unsafe_contexts = unsafe_contexts;
                header.return_type = Self::mp_optional(Self::p_hint, type_, env)?;
                Ok(header)
            }
            Token(_) => Ok(FunHdr::make_empty(env)),
            _ => Self::missing_syntax("function header", node, env),
        }
    }

    fn determine_variadicity(params: &[ast::FunParam]) -> ast::FunVariadicity {
        use aast::FunVariadicity::*;
        if let Some(x) = params.last() {
            match (x.is_variadic, &x.name) {
                (false, _) => FVnonVariadic,
                (true, name) if name == "..." => FVellipsis(x.pos.clone()),
                (true, _) => FVvariadicArg(x.clone()),
            }
        } else {
            FVnonVariadic
        }
    }

    fn p_fun_pos(node: S<'a, T, V>, env: &Env<TF>) -> Pos {
        let get_pos = |n: S<'a, T, V>, p: Pos| -> Pos {
            if let FunctionDeclarationHeader(c1) = &n.children {
                if !c1.keyword.is_missing() {
                    return Pos::btw_nocheck(Self::p_pos(&c1.keyword, env), p);
                }
            }
            p
        };
        let p = Self::p_pos(node, env);
        match &node.children {
            FunctionDeclaration(c) if env.codegen() => get_pos(&c.declaration_header, p),
            MethodishDeclaration(c) if env.codegen() => get_pos(&c.function_decl_header, p),
            _ => p,
        }
    }

    fn p_block(remove_noop: bool, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Block> {
        let ast::Stmt(p, stmt_) = Self::p_stmt(node, env)?;
        if let ast::Stmt_::Block(blk) = stmt_ {
            if remove_noop && blk.len() == 1 && blk[0].1.is_noop() {
                return Ok(vec![]);
            }
            return Ok(blk);
        } else {
            Ok(vec![ast::Stmt(p, stmt_)])
        }
    }

    fn mk_noop(env: &Env<TF>) -> ast::Stmt {
        ast::Stmt::noop(env.mk_none_pos())
    }

    fn p_function_body(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Block> {
        let mk_noop_result = |e: &Env<TF>| Ok(vec![Self::mk_noop(e)]);
        let f = |e: &mut Env<'a, TF>| -> Result<ast::Block> {
            match &node.children {
                Missing => Ok(vec![]),
                CompoundStatement(c) => {
                    let compound_statements = &c.statements.children;
                    let compound_right_brace = &c.right_brace.children;
                    match (compound_statements, compound_right_brace) {
                        (Missing, Token(_)) => mk_noop_result(e),
                        (SyntaxList(t), _) if t.len() == 1 && t[0].is_yield() => {
                            e.saw_yield = true;
                            mk_noop_result(e)
                        }
                        _ => {
                            if !e.top_level_statements
                                && ((e.file_mode() == file_info::Mode::Mdecl && !e.codegen())
                                    || e.quick_mode)
                            {
                                mk_noop_result(e)
                            } else {
                                Self::p_block(false /*remove noop*/, node, e)
                            }
                        }
                    }
                }
                _ => {
                    let f = |e: &mut Env<'a, TF>| {
                        let expr = Self::p_expr(node, e)?;
                        Ok(ast::Stmt::new(
                            expr.0.clone(),
                            ast::Stmt_::mk_return(Some(expr)),
                        ))
                    };
                    if Self::is_simple_await_expression(node) {
                        Ok(vec![f(e)?])
                    } else {
                        Ok(vec![Self::lift_awaits_in_statement(f, node, e)?])
                    }
                }
            }
        };
        Self::with_new_nonconcurrent_scope(f, env)
    }

    fn mk_suspension_kind(async_keyword: S<'a, T, V>) -> SuspensionKind {
        Self::mk_suspension_kind_(!async_keyword.is_missing())
    }

    fn mk_suspension_kind_(has_async: bool) -> SuspensionKind {
        if has_async {
            SuspensionKind::SKAsync
        } else {
            SuspensionKind::SKSync
        }
    }

    fn mk_fun_kind(suspension_kind: SuspensionKind, yield_: bool) -> ast::FunKind {
        use ast::FunKind::*;
        use SuspensionKind::*;
        match (suspension_kind, yield_) {
            (SKSync, true) => FGenerator,
            (SKAsync, true) => FAsyncGenerator,
            (SKSync, false) => FSync,
            (SKAsync, false) => FAsync,
        }
    }

    fn process_attribute_constructor_call(
        node: S<'a, T, V>,
        constructor_call_argument_list: S<'a, T, V>,
        constructor_call_type: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<ast::UserAttribute> {
        let name = Self::pos_name(constructor_call_type, env)?;
        if name.1.eq_ignore_ascii_case("__reified")
            || name.1.eq_ignore_ascii_case("__hasreifiedparent")
        {
            Self::raise_parsing_error(node, env, &syntax_error::reified_attribute);
        } else if name.1.eq_ignore_ascii_case(special_attrs::SOFT)
            && constructor_call_argument_list
                .syntax_node_to_list_skip_separator()
                .count()
                > 0
        {
            Self::raise_parsing_error(node, env, &syntax_error::soft_no_arguments);
        }
        let params = Self::could_map(
            |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::Expr> {
                Self::is_valid_attribute_arg(n, e);
                Self::p_expr(n, e)
            },
            constructor_call_argument_list,
            env,
        )?;
        Ok(ast::UserAttribute { name, params })
    }

    // Arguments to attributes must be literals (int, string, etc), collections
    // (eg vec, dict, keyset, etc), Foo::class strings, shapes, string
    // concatenations, or tuples.
    fn is_valid_attribute_arg(node: S<'a, T, V>, env: &mut Env<'a, TF>) {
        let mut is_valid_list = |nodes: S<'a, T, V>| {
            let _ = Self::could_map(
                |n, e| {
                    Self::is_valid_attribute_arg(n, e);
                    Ok(())
                },
                nodes,
                env,
            );
            ()
        };
        match &node.children {
            ParenthesizedExpression(c) => Self::is_valid_attribute_arg(&c.expression, env),
            // Normal literals (string, int, etc)
            LiteralExpression(_) => {}
            // ::class strings
            ScopeResolutionExpression(c) => {
                if let Some(TK::Class) = Self::token_kind(&c.name) {
                } else {
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::expression_as_attribute_arguments,
                    );
                }
            }
            // Negations
            PrefixUnaryExpression(c) => {
                Self::is_valid_attribute_arg(&c.operand, env);
                match Self::token_kind(&c.operator) {
                    Some(TK::Minus) => {}
                    Some(TK::Plus) => {}
                    _ => Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::expression_as_attribute_arguments,
                    ),
                }
            }
            // String concatenation
            BinaryExpression(c) => {
                if let Some(TK::Dot) = Self::token_kind(&c.operator) {
                    Self::is_valid_attribute_arg(&c.left_operand, env);
                    Self::is_valid_attribute_arg(&c.right_operand, env);
                } else {
                    Self::raise_parsing_error(
                        node,
                        env,
                        &syntax_error::expression_as_attribute_arguments,
                    );
                }
            }
            // Top-level Collections
            DarrayIntrinsicExpression(c) => is_valid_list(&c.members),
            DictionaryIntrinsicExpression(c) => is_valid_list(&c.members),
            KeysetIntrinsicExpression(c) => is_valid_list(&c.members),
            VarrayIntrinsicExpression(c) => is_valid_list(&c.members),
            VectorIntrinsicExpression(c) => is_valid_list(&c.members),
            ShapeExpression(c) => is_valid_list(&c.fields),
            TupleExpression(c) => is_valid_list(&c.items),
            // Collection Internals
            FieldInitializer(c) => {
                Self::is_valid_attribute_arg(&c.name, env);
                Self::is_valid_attribute_arg(&c.value, env);
            }
            ElementInitializer(c) => {
                Self::is_valid_attribute_arg(&c.key, env);
                Self::is_valid_attribute_arg(&c.value, env);
            }
            // Everything else is not allowed
            _ => Self::raise_parsing_error(
                node,
                env,
                &syntax_error::expression_as_attribute_arguments,
            ),
        }
    }

    fn p_user_attribute(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Vec<ast::UserAttribute>> {
        let p_attr = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::UserAttribute> {
            match &n.children {
                ConstructorCall(c) => {
                    Self::process_attribute_constructor_call(node, &c.argument_list, &c.type_, e)
                }
                _ => Self::missing_syntax("attribute", node, e),
            }
        };
        match &node.children {
            FileAttributeSpecification(c) => Self::could_map(p_attr, &c.attributes, env),
            OldAttributeSpecification(c) => Self::could_map(p_attr, &c.attributes, env),
            AttributeSpecification(c) => Self::could_map(
                |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::UserAttribute> {
                    match &n.children {
                        Attribute(c) => p_attr(&c.attribute_name, e),
                        _ => Self::missing_syntax("attribute", node, e),
                    }
                },
                &c.attributes,
                env,
            ),
            _ => Self::missing_syntax("attribute specification", node, env),
        }
    }

    fn p_user_attributes(
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Vec<ast::UserAttribute>> {
        Self::map_fold(
            &Self::p_user_attribute,
            &|mut acc: Vec<ast::UserAttribute>, mut x: Vec<ast::UserAttribute>| {
                acc.append(&mut x);
                acc
            },
            node,
            env,
            vec![],
        )
    }

    fn mp_yielding<F, R>(p: F, node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<(R, bool)>
    where
        F: FnOnce(S<'a, T, V>, &mut Env<'a, TF>) -> Result<R>,
    {
        let outer_saw_yield = env.saw_yield;
        env.saw_yield = false;
        let r = p(node, env);
        let saw_yield = env.saw_yield;
        env.saw_yield = outer_saw_yield;
        Ok((r?, saw_yield))
    }

    fn mk_empty_ns_env(env: &Env<TF>) -> RcOc<NamespaceEnv> {
        RcOc::clone(&env.empty_ns_env)
    }

    fn extract_docblock(node: S<'a, T, V>, env: &Env<TF>) -> Option<DocComment> {
        #[derive(Copy, Clone, Eq, PartialEq)]
        enum ScanState {
            DocComment,
            EmbeddedCmt,
            EndDoc,
            EndEmbedded,
            Free,
            LineCmt,
            MaybeDoc,
            MaybeDoc2,
            SawSlash,
        }
        use ScanState::*;
        // `parse` mixes loop and recursion to use less stack space.
        fn parse(
            str: &str,
            start: usize,
            state: ScanState,
            idx: usize,
        ) -> Option<(usize, usize, String)> {
            let is_whitespace = |c| c == ' ' || c == '\t' || c == '\n' || c == '\r';
            let mut s = (start, state, idx);
            let chars = str.as_bytes();
            loop {
                if s.2 == str.len() {
                    break None;
                }

                let next = s.2 + 1;
                match (s.1, chars[s.2] as char) {
                    (LineCmt, '\n') => s = (next, Free, next),
                    (EndEmbedded, '/') => s = (next, Free, next),
                    (EndDoc, '/') => {
                        let r = parse(str, next, Free, next);
                        match r {
                            d @ Some(_) => break d,
                            None => break Some((s.0, s.2 + 1, String::from(&str[s.0..s.2 + 1]))),
                        }
                    }
                    /* PHP has line comments delimited by a # */
                    (Free, '#') => s = (next, LineCmt, next),
                    /* All other comment delimiters start with a / */
                    (Free, '/') => s = (s.2, SawSlash, next),
                    /* After a / in trivia, we must see either another / or a * */
                    (SawSlash, '/') => s = (next, LineCmt, next),
                    (SawSlash, '*') => s = (s.0, MaybeDoc, next),
                    (MaybeDoc, '*') => s = (s.0, MaybeDoc2, next),
                    (MaybeDoc, _) => s = (s.0, EmbeddedCmt, next),
                    (MaybeDoc2, '/') => s = (next, Free, next),
                    /* Doc comments have a space after the second star */
                    (MaybeDoc2, c) if is_whitespace(c) => s = (s.0, DocComment, s.2),
                    (MaybeDoc2, _) => s = (s.0, EmbeddedCmt, next),
                    (DocComment, '*') => s = (s.0, EndDoc, next),
                    (DocComment, _) => s = (s.0, DocComment, next),
                    (EndDoc, _) => s = (s.0, DocComment, next),
                    /* A * without a / does not end an embedded comment */
                    (EmbeddedCmt, '*') => s = (s.0, EndEmbedded, next),
                    (EndEmbedded, '*') => s = (s.0, EndEmbedded, next),
                    (EndEmbedded, _) => s = (s.0, EmbeddedCmt, next),
                    /* Whitespace skips everywhere else */
                    (_, c) if is_whitespace(c) => s = (s.0, s.1, next),
                    /* When scanning comments, anything else is accepted */
                    (LineCmt, _) => s = (s.0, s.1, next),
                    (EmbeddedCmt, _) => s = (s.0, s.1, next),
                    _ => break None,
                }
            }
        }
        let str = node.leading_text(env.indexed_source_text.source_text());
        parse(str, 0, Free, 0).map(|(start, end, txt)| {
            let anchor = node.leading_start_offset();
            let pos = env
                .indexed_source_text
                .relative_pos(anchor + start, anchor + end);
            let ps = (pos, txt);
            oxidized::doc_comment::DocComment::new(ps)
        })
    }

    fn p_xhp_child(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::XhpChild> {
        use ast::XhpChild::*;
        use ast::XhpChildOp::*;
        match &node.children {
            Token(_) => Self::pos_name(node, env).map(ChildName),
            PostfixUnaryExpression(c) => {
                let operand = Self::p_xhp_child(&c.operand, env)?;
                let operator = match Self::token_kind(&c.operator) {
                    Some(TK::Question) => ChildQuestion,
                    Some(TK::Plus) => ChildPlus,
                    Some(TK::Star) => ChildStar,
                    _ => Self::missing_syntax("xhp children operator", node, env)?,
                };
                Ok(ChildUnary(Box::new(operand), operator))
            }
            BinaryExpression(c) => {
                let left = Self::p_xhp_child(&c.left_operand, env)?;
                let right = Self::p_xhp_child(&c.right_operand, env)?;
                Ok(ChildBinary(Box::new(left), Box::new(right)))
            }
            XHPChildrenParenthesizedList(c) => {
                let children: std::result::Result<Vec<_>, _> = c
                    .xhp_children
                    .syntax_node_to_list_skip_separator()
                    .map(|c| Self::p_xhp_child(c, env))
                    .collect();
                Ok(ChildList(children?))
            }
            _ => Self::missing_syntax("xhp children", node, env),
        }
    }

    fn p_class_elt_(
        class: &mut ast::Class_,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<()> {
        use ast::Visibility;
        let doc_comment_opt = Self::extract_docblock(node, env);
        let has_fun_header = |m: &MethodishDeclarationChildren<T, V>| {
            matches!(
                m.function_decl_header.children,
                FunctionDeclarationHeader(_)
            )
        };
        let p_method_vis = |
            node: S<'a, T, V>,
            name_pos: &Pos,
            env: &mut Env<'a, TF>,
        | -> Result<Visibility> {
            match Self::p_visibility_last_win(node, env)? {
                None => {
                    Self::raise_hh_error(env, Naming::method_needs_visibility(name_pos.clone()));
                    Ok(Visibility::Public)
                }
                Some(v) => Ok(v),
            }
        };
        match &node.children {
            ConstDeclaration(c) => {
                // TODO: make wrap `type_` `doc_comment` by `Rc` in ClassConst to avoid clone
                let type_ = Self::mp_optional(Self::p_hint, &c.type_specifier, env)?;
                // using map_fold can save one Vec allocation, but ocaml's behavior is that
                // if anything throw, it will discard all lowered elements. So adding to class
                // must be at the last.
                let mut class_consts = Self::could_map(
                    |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::ClassConst> {
                        match &n.children {
                            ConstantDeclarator(c) => {
                                let id = Self::pos_name(&c.name, e)?;
                                let expr = if n.is_abstract() {
                                    None
                                } else {
                                    Self::mp_optional(
                                        Self::p_simple_initializer,
                                        &c.initializer,
                                        e,
                                    )?
                                };
                                Ok(ast::ClassConst {
                                    type_: type_.clone(),
                                    id,
                                    expr,
                                    doc_comment: doc_comment_opt.clone(),
                                })
                            }
                            _ => Self::missing_syntax("constant declarator", n, e),
                        }
                    },
                    &c.declarators,
                    env,
                )?;
                Ok(class.consts.append(&mut class_consts))
            }
            TypeConstDeclaration(c) => {
                if !c.type_parameters.is_missing() {
                    Self::raise_parsing_error(node, env, &syntax_error::tparams_in_tconst);
                }
                let user_attributes = Self::p_user_attributes(&c.attribute_spec, env)?;
                let type__ = Self::mp_optional(Self::p_hint, &c.type_specifier, env)?
                    .map(|hint| Self::soften_hint(&user_attributes, hint));
                let kinds = Self::p_kinds(&c.modifiers, env)?;
                let name = Self::pos_name(&c.name, env)?;
                let constraint =
                    Self::mp_optional(Self::p_tconstraint_ty, &c.type_constraint, env)?;
                let span = Self::p_pos(node, env);
                let has_abstract = kinds.has(modifier::ABSTRACT);
                let (type_, abstract_kind) = match (has_abstract, &constraint, &type__) {
                    (false, _, None) => {
                        Self::raise_hh_error(
                            env,
                            NastCheck::not_abstract_without_typeconst(name.0.clone()),
                        );
                        (constraint.clone(), ast::TypeconstAbstractKind::TCConcrete)
                    }
                    (false, None, Some(_)) => (type__, ast::TypeconstAbstractKind::TCConcrete),
                    (false, Some(_), Some(_)) => {
                        (type__, ast::TypeconstAbstractKind::TCPartiallyAbstract)
                    }
                    (true, _, None) => (
                        type__.clone(),
                        ast::TypeconstAbstractKind::TCAbstract(type__),
                    ),
                    (true, _, Some(_)) => (None, ast::TypeconstAbstractKind::TCAbstract(type__)),
                };
                Ok(class.typeconsts.push(ast::ClassTypeconst {
                    abstract_: abstract_kind,
                    name,
                    constraint,
                    type_,
                    user_attributes,
                    span,
                    doc_comment: doc_comment_opt,
                    is_ctx: false,
                }))
            }
            ContextConstDeclaration(c) => {
                if !c.type_parameters.is_missing() {
                    Self::raise_parsing_error(node, env, &syntax_error::tparams_in_tconst);
                }
                let name = Self::pos_name(&c.name, env)?;
                let context = Self::p_context_list_to_intersection(&c.ctx_list, env)?;
                if !c.constraint.is_missing() && env.is_typechecker() {
                    Self::raise_parsing_error(
                        &c.constraint,
                        env,
                        "Constraints on ctx constants are not allowed",
                    );
                }
                let span = Self::p_pos(node, env);
                let kinds = Self::p_kinds(&c.modifiers, env)?;
                let has_abstract = kinds.has(modifier::ABSTRACT);
                let (context, abstract_kind) = match (has_abstract, &context) {
                    (false, None) => {
                        Self::raise_hh_error(
                            env,
                            NastCheck::not_abstract_without_typeconst(name.0.clone()),
                        );
                        (None, ast::TypeconstAbstractKind::TCConcrete)
                    }
                    (false, Some(_)) => (context, ast::TypeconstAbstractKind::TCConcrete),
                    (true, None) => (
                        context.clone(),
                        ast::TypeconstAbstractKind::TCAbstract(context),
                    ),
                    (true, Some(_)) => (None, ast::TypeconstAbstractKind::TCAbstract(context)),
                };
                Ok(class.typeconsts.push(ast::ClassTypeconst {
                    abstract_: abstract_kind,
                    name,
                    constraint: None,
                    type_: context,
                    user_attributes: vec![],
                    span,
                    doc_comment: doc_comment_opt,
                    is_ctx: true,
                }))
            }
            PropertyDeclaration(c) => {
                let user_attributes = Self::p_user_attributes(&c.attribute_spec, env)?;
                let type_ = Self::mp_optional(Self::p_hint, &c.type_, env)?
                    .map(|t| Self::soften_hint(&user_attributes, t));
                let kinds = Self::p_kinds(&c.modifiers, env)?;
                let vis = Self::p_visibility_last_win_or(&c.modifiers, env, Visibility::Public)?;
                let doc_comment = if env.quick_mode {
                    None
                } else {
                    doc_comment_opt
                };
                let name_exprs = Self::could_map(
                    |n, e| -> Result<(Pos, ast::Sid, Option<ast::Expr>)> {
                        match &n.children {
                            PropertyDeclarator(c) => {
                                let name = Self::pos_name_(&c.name, e, Some('$'))?;
                                let pos = Self::p_pos(n, e);
                                let expr = Self::mp_optional(
                                    Self::p_simple_initializer,
                                    &c.initializer,
                                    e,
                                )?;
                                Ok((pos, name, expr))
                            }
                            _ => Self::missing_syntax("property declarator", n, e),
                        }
                    },
                    &c.declarators,
                    env,
                )?;

                let mut i = 0;
                for name_expr in name_exprs.into_iter() {
                    class.vars.push(ast::ClassVar {
                        final_: kinds.has(modifier::FINAL),
                        xhp_attr: None,
                        abstract_: kinds.has(modifier::ABSTRACT),
                        visibility: vis,
                        type_: ast::TypeHint((), type_.clone()),
                        id: name_expr.1,
                        expr: name_expr.2,
                        user_attributes: user_attributes.clone(),
                        doc_comment: if i == 0 { doc_comment.clone() } else { None },
                        is_promoted_variadic: false,
                        is_static: kinds.has(modifier::STATIC),
                        span: name_expr.0,
                    });
                    i += 1;
                }
                Ok(())
            }
            MethodishDeclaration(c) if has_fun_header(c) => {
                let classvar_init = |param: &ast::FunParam| -> (ast::Stmt, ast::ClassVar) {
                    let cvname = Self::drop_prefix(&param.name, '$');
                    let p = &param.pos;
                    let span = match &param.expr {
                        Some(ast::Expr(pos_end, _)) => {
                            Pos::btw(p, pos_end).unwrap_or_else(|_| p.clone())
                        }
                        _ => p.clone(),
                    };
                    let e = |expr_: ast::Expr_| -> ast::Expr { ast::Expr::new(p.clone(), expr_) };
                    let lid = |s: &str| -> ast::Lid { ast::Lid(p.clone(), (0, s.to_string())) };
                    (
                        ast::Stmt::new(
                            p.clone(),
                            ast::Stmt_::mk_expr(e(E_::mk_binop(
                                ast::Bop::Eq(None),
                                e(E_::mk_obj_get(
                                    e(E_::mk_lvar(lid(special_idents::THIS))),
                                    e(E_::mk_id(ast::Id(p.clone(), cvname.to_string()))),
                                    ast::OgNullFlavor::OGNullthrows,
                                    false,
                                )),
                                e(E_::mk_lvar(lid(&param.name))),
                            ))),
                        ),
                        ast::ClassVar {
                            final_: false,
                            xhp_attr: None,
                            abstract_: false,
                            visibility: param.visibility.unwrap(),
                            type_: param.type_hint.clone(),
                            id: ast::Id(p.clone(), cvname.to_string()),
                            expr: None,
                            user_attributes: param.user_attributes.clone(),
                            doc_comment: None,
                            is_promoted_variadic: param.is_variadic,
                            is_static: false,
                            span,
                        },
                    )
                };
                let header = &c.function_decl_header;
                let h = match &header.children {
                    FunctionDeclarationHeader(h) => h,
                    _ => panic!(),
                };
                let hdr = Self::p_fun_hdr(header, env)?;
                let (mut member_init, mut member_def): (Vec<ast::Stmt>, Vec<ast::ClassVar>) = hdr
                    .parameters
                    .iter()
                    .filter_map(|p| p.visibility.map(|_| classvar_init(p)))
                    .unzip();

                let kinds = Self::p_kinds(&h.modifiers, env)?;
                let visibility = p_method_vis(&h.modifiers, &hdr.name.0, env)?;
                let is_static = kinds.has(modifier::STATIC);
                *env.in_static_method() = is_static;
                let (mut body, body_has_yield) =
                    Self::mp_yielding(Self::p_function_body, &c.function_body, env)?;
                if env.codegen() {
                    member_init.reverse();
                }
                member_init.append(&mut body);
                let body = member_init;
                *env.in_static_method() = false;
                let is_abstract = kinds.has(modifier::ABSTRACT);
                let is_external = !is_abstract && c.function_body.is_external();
                let user_attributes = Self::p_user_attributes(&c.attribute, env)?;
                Self::check_effect_polymorphic_memoized(
                    &hdr.contexts,
                    &user_attributes,
                    "method",
                    env,
                );
                let method = ast::Method_ {
                    span: Self::p_fun_pos(node, env),
                    annotation: (),
                    final_: kinds.has(modifier::FINAL),
                    abstract_: is_abstract,
                    static_: is_static,
                    name: hdr.name,
                    visibility,
                    tparams: hdr.type_parameters,
                    where_constraints: hdr.constrs,
                    variadic: Self::determine_variadicity(&hdr.parameters),
                    params: hdr.parameters,
                    ctxs: hdr.contexts,
                    unsafe_ctxs: hdr.unsafe_contexts,
                    body: ast::FuncBody {
                        annotation: (),
                        ast: body,
                    },
                    fun_kind: Self::mk_fun_kind(hdr.suspension_kind, body_has_yield),
                    user_attributes,
                    ret: ast::TypeHint((), hdr.return_type),
                    external: is_external,
                    doc_comment: doc_comment_opt,
                };
                class.vars.append(&mut member_def);
                Ok(class.methods.push(method))
            }
            TraitUseConflictResolution(c) => {
                type Ret = Result<Either<ast::InsteadofAlias, ast::UseAsAlias>>;
                let p_item = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Ret {
                    match &n.children {
                        TraitUsePrecedenceItem(c) => {
                            let (qualifier, name) = match &c.name.children {
                                ScopeResolutionExpression(c) => (
                                    Self::pos_name(&c.qualifier, e)?,
                                    Self::p_pstring(&c.name, e)?,
                                ),
                                _ => Self::missing_syntax("trait use precedence item", n, e)?,
                            };
                            let removed_names =
                                Self::could_map(Self::pos_name, &c.removed_names, e)?;
                            Self::raise_hh_error(e, Naming::unsupported_instead_of(name.0.clone()));
                            Ok(Either::Left(ast::InsteadofAlias(
                                qualifier,
                                name,
                                removed_names,
                            )))
                        }
                        TraitUseAliasItem(c) => {
                            let (qualifier, name) = match &c.aliasing_name.children {
                                ScopeResolutionExpression(c) => (
                                    Some(Self::pos_name(&c.qualifier, e)?),
                                    Self::p_pstring(&c.name, e)?,
                                ),
                                _ => (None, Self::p_pstring(&c.aliasing_name, e)?),
                            };
                            let (kinds, mut vis_raw) = Self::p_modifiers(
                                |mut acc, kind| -> Vec<ast::UseAsVisibility> {
                                    if let Some(v) = modifier::to_use_as_visibility(kind) {
                                        acc.push(v);
                                    }
                                    acc
                                },
                                vec![],
                                &c.modifiers,
                                e,
                            )?;
                            let vis = if kinds.is_empty() || kinds.has_any(modifier::VISIBILITIES) {
                                vis_raw
                            } else {
                                let mut v = vec![ast::UseAsVisibility::UseAsPublic];
                                v.append(&mut vis_raw);
                                v
                            };
                            let aliased_name = if !c.aliased_name.is_missing() {
                                Some(Self::pos_name(&c.aliased_name, e)?)
                            } else {
                                None
                            };
                            Self::raise_hh_error(
                                e,
                                Naming::unsupported_trait_use_as(name.0.clone()),
                            );
                            Ok(Either::Right(ast::UseAsAlias(
                                qualifier,
                                name,
                                aliased_name,
                                vis,
                            )))
                        }
                        _ => Self::missing_syntax("trait use conflict resolution item", n, e),
                    }
                };
                let mut uses = Self::could_map(Self::p_hint, &c.names, env)?;
                let elts = Self::could_map(p_item, &c.clauses, env)?;
                class.uses.append(&mut uses);
                for elt in elts.into_iter() {
                    match elt {
                        Either::Left(l) => class.insteadof_alias.push(l),
                        Either::Right(r) => class.use_as_alias.push(r),
                    }
                }
                Ok(())
            }
            TraitUse(c) => {
                let mut uses = Self::could_map(Self::p_hint, &c.names, env)?;
                Ok(class.uses.append(&mut uses))
            }
            RequireClause(c) => {
                let hint = Self::p_hint(&c.name, env)?;
                let is_extends = match Self::token_kind(&c.kind) {
                    Some(TK::Implements) => false,
                    Some(TK::Extends) => true,
                    _ => Self::missing_syntax("trait require kind", &c.kind, env)?,
                };
                Ok(class.reqs.push((hint, is_extends)))
            }
            XHPClassAttributeDeclaration(c) => {
                type Ret = Result<Either<ast::XhpAttr, ast::Hint>>;
                let p_attr = |node: S<'a, T, V>, env: &mut Env<'a, TF>| -> Ret {
                    let mk_attr_use = |n: S<'a, T, V>, env: &mut Env<'a, TF>| {
                        Ok(Either::Right(ast::Hint(
                            Self::p_pos(n, env),
                            Box::new(ast::Hint_::Happly(Self::pos_name(n, env)?, vec![])),
                        )))
                    };
                    match &node.children {
                        XHPClassAttribute(c) => {
                            let ast::Id(p, name) = Self::pos_name(&c.name, env)?;
                            if let TypeConstant(_) = &c.type_.children {
                                if env.is_typechecker() {
                                    Self::raise_parsing_error(
                                        &c.type_,
                                        env,
                                        &syntax_error::xhp_class_attribute_type_constant,
                                    )
                                }
                            }
                            let req = match &c.required.children {
                                XHPRequired(_) => Some(ast::XhpAttrTag::Required),
                                XHPLateinit(_) => Some(ast::XhpAttrTag::LateInit),
                                _ => None,
                            };
                            let pos = if c.initializer.is_missing() {
                                p.clone()
                            } else {
                                Pos::btw(&p, &Self::p_pos(&c.initializer, env))
                                    .map_err(Error::Failwith)?
                            };
                            let (hint, enum_) = match &c.type_.children {
                                XHPEnumType(c1) => {
                                    let p = Self::p_pos(&c.type_, env);
                                    let vals = Self::could_map(Self::p_expr, &c1.values, env)?;
                                    (None, Some((p, vals)))
                                }
                                _ => (Some(Self::p_hint(&c.type_, env)?), None),
                            };
                            let init_expr =
                                Self::mp_optional(Self::p_simple_initializer, &c.initializer, env)?;
                            let xhp_attr = ast::XhpAttr(
                                ast::TypeHint((), hint.clone()),
                                ast::ClassVar {
                                    final_: false,
                                    xhp_attr: Some(ast::XhpAttrInfo { xai_tag: req }),
                                    abstract_: false,
                                    visibility: ast::Visibility::Public,
                                    type_: ast::TypeHint((), hint),
                                    id: ast::Id(p, String::from(":") + &name),
                                    expr: init_expr,
                                    user_attributes: vec![],
                                    doc_comment: None,
                                    is_promoted_variadic: false,
                                    is_static: false,
                                    span: pos,
                                },
                                req,
                                enum_,
                            );
                            Ok(Either::Left(xhp_attr))
                        }
                        XHPSimpleClassAttribute(c) => mk_attr_use(&c.type_, env),
                        Token(_) => mk_attr_use(node, env),
                        _ => Self::missing_syntax("XHP attribute", node, env),
                    }
                };
                let attrs = Self::could_map(p_attr, &c.attributes, env)?;
                for attr in attrs.into_iter() {
                    match attr {
                        Either::Left(attr) => class.xhp_attrs.push(attr),
                        Either::Right(xhp_attr_use) => class.xhp_attr_uses.push(xhp_attr_use),
                    }
                }
                Ok(())
            }
            XHPChildrenDeclaration(c) => {
                let p = Self::p_pos(node, env);
                Ok(class
                    .xhp_children
                    .push((p, Self::p_xhp_child(&c.expression, env)?)))
            }
            XHPCategoryDeclaration(c) => {
                let p = Self::p_pos(node, env);
                let categories =
                    Self::could_map(|n, e| Self::p_pstring_(n, e, Some('%')), &c.categories, env)?;
                if let Some((_, cs)) = &class.xhp_category {
                    if let Some(category) = cs.first() {
                        Self::raise_hh_error(
                            env,
                            NastCheck::multiple_xhp_category(category.0.clone()),
                        )
                    }
                }
                Ok(class.xhp_category = Some((p, categories)))
            }
            _ => Self::missing_syntax("class element", node, env),
        }
    }

    fn p_class_elt(
        class: &mut ast::Class_,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<()> {
        let r = Self::p_class_elt_(class, node, env);
        match r {
            // match ocaml behavior, don't throw if missing syntax when fail_open is true
            Err(Error::MissingSyntax { .. }) if env.fail_open() => Ok(()),
            _ => r,
        }
    }

    fn contains_class_body(c: &ClassishDeclarationChildren<T, V>) -> bool {
        matches!(&c.body.children, ClassishBody(_))
    }

    fn p_where_constraint(
        is_class: bool,
        parent: S<'a, T, V>,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<Vec<ast::WhereConstraintHint>> {
        match &node.children {
            Missing => Ok(vec![]),
            WhereClause(c) => {
                let f = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::WhereConstraintHint> {
                    match &n.children {
                        WhereConstraint(c) => {
                            use ast::ConstraintKind::*;
                            let l = Self::p_hint(&c.left_type, e)?;
                            let o = &c.operator;
                            let o = match Self::token_kind(o) {
                                Some(TK::Equal) => ConstraintEq,
                                Some(TK::As) => ConstraintAs,
                                Some(TK::Super) => ConstraintSuper,
                                _ => Self::missing_syntax("constraint operator", o, e)?,
                            };
                            Ok(ast::WhereConstraintHint(
                                l,
                                o,
                                Self::p_hint(&c.right_type, e)?,
                            ))
                        }
                        _ => Self::missing_syntax("where constraint", n, e),
                    }
                };
                c.constraints
                    .syntax_node_to_list_skip_separator()
                    .map(|n| f(n, env))
                    .collect()
            }
            _ => {
                if is_class {
                    Self::missing_syntax("classish declaration constraints", parent, env)
                } else {
                    Self::missing_syntax("function header constraints", parent, env)
                }
            }
        }
    }

    fn p_namespace_use_kind(kind: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::NsKind> {
        use ast::NsKind::*;
        match &kind.children {
            Missing => Ok(NSClassAndNamespace),
            _ => match Self::token_kind(kind) {
                Some(TK::Namespace) => Ok(NSNamespace),
                Some(TK::Type) => Ok(NSClass),
                Some(TK::Function) => Ok(NSFun),
                Some(TK::Const) => Ok(NSConst),
                _ => Self::missing_syntax("namespace use kind", kind, env),
            },
        }
    }

    fn p_namespace_use_clause(
        prefix: Option<S<'a, T, V>>,
        kind: Result<ast::NsKind>,
        node: S<'a, T, V>,
        env: &mut Env<'a, TF>,
    ) -> Result<(ast::NsKind, ast::Sid, ast::Sid)> {
        lazy_static! {
            static ref NAMESPACE_USE: regex::Regex = regex::Regex::new("[^\\\\]*$").unwrap();
        }

        match &node.children {
            NamespaceUseClause(NamespaceUseClauseChildren {
                clause_kind,
                alias,
                name,
                ..
            }) => {
                let ast::Id(p, n) = match (prefix, Self::pos_name(name, env)?) {
                    (None, id) => id,
                    (Some(prefix), ast::Id(p, n)) => {
                        ast::Id(p, Self::pos_name(prefix, env)?.1 + &n)
                    }
                };
                let alias = if alias.is_missing() {
                    let x = NAMESPACE_USE.find(&n).unwrap().as_str();
                    ast::Id(p.clone(), x.to_string())
                } else {
                    Self::pos_name(alias, env)?
                };
                let kind = if clause_kind.is_missing() {
                    kind
                } else {
                    Self::p_namespace_use_kind(clause_kind, env)
                }?;
                Ok((
                    kind,
                    ast::Id(
                        p,
                        if n.len() > 0 && n.chars().nth(0) == Some('\\') {
                            n
                        } else {
                            String::from("\\") + &n
                        },
                    ),
                    alias,
                ))
            }
            _ => Self::missing_syntax("namespace use clause", node, env),
        }
    }

    fn check_effect_polymorphic_memoized(
        contexts: &Option<ast::Contexts>,
        user_attributes: &[aast::UserAttribute<Pos, (), (), ()>],
        kind: &str,
        env: &mut Env<'a, TF>,
    ) {
        if Self::has_polymorphic_context(contexts) {
            if let Some(u) = user_attributes
                .iter()
                .find(|u| u.name.1 == naming_special_names_rust::user_attributes::MEMOIZE)
            {
                Self::raise_parsing_error_pos(
                    &u.name.0,
                    env,
                    &syntax_error::effect_polymorphic_memoized(kind),
                )
            }
        }
    }

    fn p_def(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<Vec<ast::Def>> {
        let doc_comment_opt = Self::extract_docblock(node, env);
        match &node.children {
            FunctionDeclaration(FunctionDeclarationChildren {
                attribute_spec,
                declaration_header,
                body,
            }) => {
                let mut env = Env::clone_and_unset_toplevel_if_toplevel(env);
                let env = env.as_mut();
                let hdr = Self::p_fun_hdr(declaration_header, env)?;
                let is_external = body.is_external();
                let (block, yield_) = if is_external {
                    (vec![], false)
                } else {
                    Self::mp_yielding(&Self::p_function_body, body, env)?
                };
                let user_attributes = Self::p_user_attributes(attribute_spec, env)?;
                Self::check_effect_polymorphic_memoized(
                    &hdr.contexts,
                    &user_attributes,
                    "function",
                    env,
                );
                let variadic = Self::determine_variadicity(&hdr.parameters);
                let ret = ast::TypeHint((), hdr.return_type);
                Ok(vec![ast::Def::mk_fun(ast::Fun_ {
                    span: Self::p_fun_pos(node, env),
                    annotation: (),
                    mode: env.file_mode(),
                    ret,
                    name: hdr.name,
                    tparams: hdr.type_parameters,
                    where_constraints: hdr.constrs,
                    params: hdr.parameters,
                    ctxs: hdr.contexts,
                    unsafe_ctxs: hdr.unsafe_contexts,
                    body: ast::FuncBody {
                        ast: block,
                        annotation: (),
                    },
                    fun_kind: Self::mk_fun_kind(hdr.suspension_kind, yield_),
                    variadic,
                    user_attributes,
                    file_attributes: vec![],
                    external: is_external,
                    namespace: Self::mk_empty_ns_env(env),
                    doc_comment: doc_comment_opt,
                    static_: false,
                })])
            }
            ClassishDeclaration(c) if Self::contains_class_body(c) => {
                let mut env = Env::clone_and_unset_toplevel_if_toplevel(env);
                let env = env.as_mut();
                let mode = env.file_mode();
                let user_attributes = Self::p_user_attributes(&c.attribute, env)?;
                let kinds = Self::p_kinds(&c.modifiers, env)?;
                let final_ = kinds.has(modifier::FINAL);
                let is_xhp = matches!(
                    Self::token_kind(&c.name),
                    Some(TK::XHPElementName) | Some(TK::XHPClassName)
                );
                let has_xhp_keyword = matches!(Self::token_kind(&c.xhp), Some(TK::XHP));
                let name = Self::pos_name(&c.name, env)?;
                *env.cls_reified_generics() = HashSet::new();
                let tparams = Self::p_tparam_l(true, &c.type_parameters, env)?;
                let class_kind = match Self::token_kind(&c.keyword) {
                    Some(TK::Class) if kinds.has(modifier::ABSTRACT) => ast::ClassKind::Cabstract,
                    Some(TK::Class) => ast::ClassKind::Cnormal,
                    Some(TK::Interface) => ast::ClassKind::Cinterface,
                    Some(TK::Trait) => ast::ClassKind::Ctrait,
                    Some(TK::Enum) => ast::ClassKind::Cenum,
                    _ => Self::missing_syntax("class kind", &c.keyword, env)?,
                };
                let filter_dynamic =
                    |node: S<'a, T, V>, env: &mut Env<'a, TF>| -> Result<Option<ast::Hint>> {
                        match Self::p_hint(node, env) {
                            Err(e) => Err(e),
                            Ok(h) => match &*h.1 {
                                oxidized::aast_defs::Hint_::Happly(oxidized::ast::Id(_, id), _) => {
                                    if id == "dynamic" {
                                        Ok(None)
                                    } else {
                                        Ok(Some(h))
                                    }
                                }
                                _ => Ok(Some(h)),
                            },
                        }
                    };
                let (extends, extends_dynamic) = match class_kind {
                    ast::ClassKind::Cinterface if env.parser_options.tco_enable_sound_dynamic => {
                        Self::could_map_filter(filter_dynamic, &c.extends_list, env)?
                    }
                    _ => (Self::could_map(Self::p_hint, &c.extends_list, env)?, false),
                };
                *env.parent_maybe_reified() = match extends.first().map(|h| h.1.as_ref()) {
                    Some(ast::Hint_::Happly(_, hl)) => !hl.is_empty(),
                    _ => false,
                };
                let (implements, implements_dynamic) =
                    if env.parser_options.tco_enable_sound_dynamic {
                        Self::could_map_filter(filter_dynamic, &c.implements_list, env)?
                    } else {
                        (
                            Self::could_map(Self::p_hint, &c.implements_list, env)?,
                            false,
                        )
                    };

                let where_constraints = Self::p_where_constraint(true, node, &c.where_clause, env)?;
                let namespace = Self::mk_empty_ns_env(env);
                let span = Self::p_pos(node, env);
                let mut class_ = ast::Class_ {
                    span,
                    annotation: (),
                    mode,
                    final_,
                    is_xhp,
                    has_xhp_keyword,
                    kind: class_kind,
                    name,
                    tparams,
                    extends,
                    uses: vec![],
                    use_as_alias: vec![],
                    insteadof_alias: vec![],
                    xhp_attr_uses: vec![],
                    xhp_category: None,
                    reqs: vec![],
                    implements,
                    implements_dynamic: implements_dynamic || extends_dynamic,
                    where_constraints,
                    consts: vec![],
                    typeconsts: vec![],
                    vars: vec![],
                    methods: vec![],
                    // TODO: what is this attbiute? check ast_to_aast
                    attributes: vec![],
                    xhp_children: vec![],
                    xhp_attrs: vec![],
                    namespace,
                    user_attributes,
                    file_attributes: vec![],
                    enum_: None,
                    doc_comment: doc_comment_opt,
                    emit_id: None,
                };
                match &c.body.children {
                    ClassishBody(c1) => {
                        for elt in c1.elements.syntax_node_to_list_skip_separator() {
                            Self::p_class_elt(&mut class_, elt, env)?;
                        }
                    }
                    _ => Self::missing_syntax("classish body", &c.body, env)?,
                }
                Ok(vec![ast::Def::mk_class(class_)])
            }
            ConstDeclaration(c) => {
                let ty = &c.type_specifier;
                let decls = c.declarators.syntax_node_to_list_skip_separator();
                let mut defs = vec![];
                for decl in decls {
                    let def = match &decl.children {
                        ConstantDeclarator(c) => {
                            let name = &c.name;
                            let init = &c.initializer;
                            let gconst = ast::Gconst {
                                annotation: (),
                                mode: env.file_mode(),
                                name: Self::pos_name(name, env)?,
                                type_: Self::mp_optional(Self::p_hint, ty, env)?,
                                value: Self::p_simple_initializer(init, env)?,
                                namespace: Self::mk_empty_ns_env(env),
                                span: Self::p_pos(node, env),
                                emit_id: None,
                            };
                            ast::Def::mk_constant(gconst)
                        }
                        _ => Self::missing_syntax("constant declaration", decl, env)?,
                    };
                    defs.push(def);
                }
                Ok(defs)
            }
            AliasDeclaration(c) => {
                let tparams = Self::p_tparam_l(false, &c.generic_parameter, env)?;
                for tparam in tparams.iter() {
                    if tparam.reified != ast::ReifyKind::Erased {
                        Self::raise_parsing_error(node, env, &syntax_error::invalid_reified)
                    }
                }
                Ok(vec![ast::Def::mk_typedef(ast::Typedef {
                    annotation: (),
                    name: Self::pos_name(&c.name, env)?,
                    tparams,
                    constraint: Self::mp_optional(Self::p_tconstraint, &c.constraint, env)?
                        .map(|x| x.1),
                    user_attributes: itertools::concat(
                        c.attribute_spec
                            .syntax_node_to_list_skip_separator()
                            .map(|attr| Self::p_user_attribute(attr, env))
                            .collect::<std::result::Result<Vec<Vec<_>>, _>>()?,
                    ),
                    namespace: Self::mk_empty_ns_env(env),
                    mode: env.file_mode(),
                    vis: match Self::token_kind(&c.keyword) {
                        Some(TK::Type) => ast::TypedefVisibility::Transparent,
                        Some(TK::Newtype) => ast::TypedefVisibility::Opaque,
                        _ => Self::missing_syntax("kind", &c.keyword, env)?,
                    },
                    kind: Self::p_hint(&c.type_, env)?,
                    span: Self::p_pos(node, env),
                    emit_id: None,
                })])
            }
            EnumDeclaration(c) => {
                let p_enumerator =
                    |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<ast::ClassConst> {
                        match &n.children {
                            Enumerator(c) => Ok(ast::ClassConst {
                                type_: None,
                                id: Self::pos_name(&c.name, e)?,
                                expr: Some(Self::p_expr(&c.value, e)?),
                                doc_comment: None,
                            }),
                            _ => Self::missing_syntax("enumerator", n, e),
                        }
                    };

                let mut includes = vec![];

                let mut p_enum_use = |n: S<'a, T, V>, e: &mut Env<'a, TF>| -> Result<()> {
                    match &n.children {
                        EnumUse(c) => {
                            let mut uses = Self::could_map(Self::p_hint, &c.names, e)?;
                            Ok(includes.append(&mut uses))
                        }
                        _ => Self::missing_syntax("enum_use", node, e),
                    }
                };

                for elt in c.use_clauses.syntax_node_to_list_skip_separator() {
                    p_enum_use(elt, env)?;
                }

                Ok(vec![ast::Def::mk_class(ast::Class_ {
                    annotation: (),
                    mode: env.file_mode(),
                    user_attributes: Self::p_user_attributes(&c.attribute_spec, env)?,
                    file_attributes: vec![],
                    final_: false,
                    kind: ast::ClassKind::Cenum,
                    is_xhp: false,
                    has_xhp_keyword: false,
                    name: Self::pos_name(&c.name, env)?,
                    tparams: vec![],
                    extends: vec![],
                    implements: vec![],
                    implements_dynamic: false,
                    where_constraints: vec![],
                    consts: Self::could_map(p_enumerator, &c.enumerators, env)?,
                    namespace: Self::mk_empty_ns_env(env),
                    span: Self::p_pos(node, env),
                    enum_: Some(ast::Enum_ {
                        base: Self::p_hint(&c.base, env)?,
                        constraint: Self::mp_optional(Self::p_tconstraint_ty, &c.type_, env)?,
                        includes,
                        enum_class: false,
                    }),
                    doc_comment: doc_comment_opt,
                    uses: vec![],
                    use_as_alias: vec![],
                    insteadof_alias: vec![],
                    xhp_attr_uses: vec![],
                    xhp_category: None,
                    reqs: vec![],
                    vars: vec![],
                    typeconsts: vec![],
                    methods: vec![],
                    attributes: vec![],
                    xhp_children: vec![],
                    xhp_attrs: vec![],
                    emit_id: None,
                })])
            }

            EnumClassDeclaration(c) => {
                let name = Self::pos_name(&c.name, env)?;
                // Adding __EnumClass
                let mut user_attributes = Self::p_user_attributes(&c.attribute_spec, env)?;
                let enum_class_attribute = ast::UserAttribute {
                    name: ast::Id(name.0.clone(), special_attrs::ENUM_CLASS.to_string()),
                    params: vec![],
                };
                user_attributes.push(enum_class_attribute);
                // During lowering we store the base type as is. It will be updated during
                // the naming phase
                let base_type = Self::p_hint(&c.base, env)?;

                let name_s = name.1.clone(); // TODO: can I avoid this clone ?

                // Helper to build X -> HH\MemberOf<enum_name, X>
                let build_elt = |p: Pos, ty: ast::Hint| -> ast::Hint {
                    let enum_name = ast::Id(p.clone(), name_s.clone());
                    let enum_class = ast::Hint_::mk_happly(enum_name, vec![]);
                    let enum_class = ast::Hint::new(p.clone(), enum_class);
                    let elt_id = ast::Id(p.clone(), special_classes::MEMBER_OF.to_string());
                    let full_type = ast::Hint_::mk_happly(elt_id, vec![enum_class, ty]);
                    ast::Hint::new(p, full_type)
                };

                let extends = Self::could_map(Self::p_hint, &c.extends_list, env)?;

                let mut enum_class = ast::Class_ {
                    annotation: (),
                    mode: env.file_mode(),
                    user_attributes,
                    file_attributes: vec![],
                    final_: false, // TODO(T77095784): support final EDTs
                    kind: ast::ClassKind::Cenum,
                    is_xhp: false,
                    has_xhp_keyword: false,
                    name,
                    tparams: vec![],
                    extends: extends.clone(),
                    implements: vec![],
                    implements_dynamic: false,
                    where_constraints: vec![],
                    consts: vec![],
                    namespace: Self::mk_empty_ns_env(env),
                    span: Self::p_pos(node, env),
                    enum_: Some(ast::Enum_ {
                        base: base_type,
                        constraint: None,
                        includes: extends,
                        enum_class: true,
                    }),
                    doc_comment: doc_comment_opt,
                    uses: vec![],
                    use_as_alias: vec![],
                    insteadof_alias: vec![],
                    xhp_attr_uses: vec![],
                    xhp_category: None,
                    reqs: vec![],
                    vars: vec![],
                    typeconsts: vec![],
                    methods: vec![],
                    attributes: vec![],
                    xhp_children: vec![],
                    xhp_attrs: vec![],
                    emit_id: None,
                };

                for n in c.elements.syntax_node_to_list_skip_separator() {
                    match &n.children {
                        // TODO(T77095784): check pos and span usage
                        EnumClassEnumerator(c) => {
                            // we turn:
                            // - type name = expression;
                            // into
                            // - const MemberOf<enum_name, type> name = expression
                            let name = Self::pos_name(&c.name, env)?;
                            let pos = &name.0;
                            let elt_type = Self::p_hint(&c.type_, env)?;
                            let full_type = build_elt(pos.clone(), elt_type);
                            let initial_value = Self::p_expr(&c.initial_value, env)?;
                            let class_const = ast::ClassConst {
                                type_: Some(full_type),
                                id: name,
                                expr: Some(initial_value),
                                doc_comment: None,
                            };
                            enum_class.consts.push(class_const)
                        }
                        _ => {
                            let pos = Self::p_pos(n, env);
                            Self::raise_parsing_error_pos(
                                &pos,
                                env,
                                &syntax_error::invalid_enum_class_enumerator,
                            )
                        }
                    }
                }
                Ok(vec![ast::Def::mk_class(enum_class)])
            }
            RecordDeclaration(c) => {
                let p_field = |n: S<'a, T, V>, e: &mut Env<'a, TF>| match &n.children {
                    RecordField(c) => Ok((
                        Self::pos_name(&c.name, e)?,
                        Self::p_hint(&c.type_, e)?,
                        Self::mp_optional(Self::p_simple_initializer, &c.init, e)?,
                    )),
                    _ => Self::missing_syntax("record_field", n, e),
                };
                Ok(vec![ast::Def::mk_record_def(ast::RecordDef {
                    annotation: (),
                    name: Self::pos_name(&c.name, env)?,
                    extends: Self::could_map(Self::p_hint, &c.extends_opt, env)?
                        .into_iter()
                        .next(),
                    abstract_: Self::token_kind(&c.modifier) == Some(TK::Abstract),
                    user_attributes: Self::p_user_attributes(&c.attribute_spec, env)?,
                    fields: Self::could_map(p_field, &c.fields, env)?,
                    namespace: Self::mk_empty_ns_env(env),
                    span: Self::p_pos(node, env),
                    doc_comment: doc_comment_opt,
                    emit_id: None,
                })])
            }
            InclusionDirective(c) if env.file_mode() != file_info::Mode::Mdecl || env.codegen() => {
                let expr = Self::p_expr(&c.expression, env)?;
                Ok(vec![ast::Def::mk_stmt(ast::Stmt::new(
                    Self::p_pos(node, env),
                    ast::Stmt_::mk_expr(expr),
                ))])
            }
            NamespaceDeclaration(c) => {
                let name = if let NamespaceDeclarationHeader(h) = &c.header.children {
                    &h.name
                } else {
                    return Self::missing_syntax("namespace_declaration_header", node, env);
                };
                let defs = match &c.body.children {
                    NamespaceBody(c) => {
                        let mut env1 = Env::clone_and_unset_toplevel_if_toplevel(env);
                        let env1 = env1.as_mut();
                        itertools::concat(
                            c.declarations
                                .syntax_node_to_list_skip_separator()
                                .map(|n| Self::p_def(n, env1))
                                .collect::<std::result::Result<Vec<Vec<_>>, _>>()?,
                        )
                    }
                    _ => vec![],
                };
                Ok(vec![ast::Def::mk_namespace(
                    Self::pos_name(name, env)?,
                    defs,
                )])
            }
            NamespaceGroupUseDeclaration(c) => {
                let uses: std::result::Result<Vec<_>, _> = c
                    .clauses
                    .syntax_node_to_list_skip_separator()
                    .map(|n| {
                        Self::p_namespace_use_clause(
                            Some(&c.prefix),
                            Self::p_namespace_use_kind(&c.kind, env),
                            n,
                            env,
                        )
                    })
                    .collect();
                Ok(vec![ast::Def::mk_namespace_use(uses?)])
            }
            NamespaceUseDeclaration(c) => {
                let uses: std::result::Result<Vec<_>, _> = c
                    .clauses
                    .syntax_node_to_list_skip_separator()
                    .map(|n| {
                        Self::p_namespace_use_clause(
                            None,
                            Self::p_namespace_use_kind(&c.kind, env),
                            n,
                            env,
                        )
                    })
                    .collect();
                Ok(vec![ast::Def::mk_namespace_use(uses?)])
            }
            FileAttributeSpecification(_) => {
                Ok(vec![ast::Def::mk_file_attributes(ast::FileAttribute {
                    user_attributes: Self::p_user_attribute(node, env)?,
                    namespace: Self::mk_empty_ns_env(env),
                })])
            }
            _ if env.file_mode() == file_info::Mode::Mdecl => Ok(vec![]),
            _ => Ok(vec![ast::Def::mk_stmt(Self::p_stmt(node, env)?)]),
        }
    }

    fn post_process(env: &mut Env<'a, TF>, program: ast::Program, acc: &mut ast::Program) {
        use aast::{Def, Def::*, Stmt_::*};
        let mut saw_ns: Option<(ast::Sid, ast::Program)> = None;
        for def in program.into_iter() {
            if let Namespace(_) = &def {
                if let Some((n, ns_acc)) = saw_ns {
                    acc.push(Def::mk_namespace(n, ns_acc));
                    saw_ns = None;
                }
            }

            if let Namespace(ns) = def {
                let (n, defs) = *ns;
                if defs.is_empty() {
                    saw_ns = Some((n, vec![]));
                } else {
                    let mut acc_ = vec![];
                    Self::post_process(env, defs, &mut acc_);
                    acc.push(Def::mk_namespace(n, acc_));
                }

                continue;
            }

            if let Stmt(s) = &def {
                if s.1.is_noop() {
                    continue;
                }
                let raise_error = match &s.1 {
                    Markup(_) => false,
                    Expr(expr)
                        if expr.as_ref().is_import()
                            && !env.parser_options.po_disallow_toplevel_requires =>
                    {
                        false
                    }
                    _ => {
                        use file_info::Mode::*;
                        let mode = env.file_mode();
                        env.keep_errors
                            && env.is_typechecker()
                            && (mode == Mstrict
                                || (mode == Mpartial
                                    && env
                                        .parser_options
                                        .error_codes_treated_strictly
                                        .contains(&1002)))
                    }
                };
                if raise_error {
                    Self::raise_parsing_error_pos(&s.0, env, &syntax_error::toplevel_statements);
                }
            }

            if let Some((_, ns_acc)) = &mut saw_ns {
                ns_acc.push(def);
            } else {
                acc.push(def);
            };
        }
        if let Some((n, defs)) = saw_ns {
            acc.push(Def::mk_namespace(n, defs));
        }
    }

    fn p_program(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Program> {
        let nodes = node.syntax_node_to_list_skip_separator();
        let mut acc = vec![];
        for n in nodes {
            match &n.children {
                EndOfFile(_) => break,
                _ => match Self::p_def(n, env) {
                    Err(Error::MissingSyntax { .. }) if env.fail_open => {}
                    e @ Err(_) => return e,
                    Ok(mut def) => acc.append(&mut def),
                },
            }
        }
        let mut program = vec![];
        Self::post_process(env, acc, &mut program);
        Ok(program)
    }

    fn p_script(node: S<'a, T, V>, env: &mut Env<'a, TF>) -> Result<ast::Program> {
        match &node.children {
            Script(c) => Self::p_program(&c.declarations, env),
            _ => Self::missing_syntax("script", node, env),
        }
    }

    fn lower(
        env: &mut Env<'a, TF>,
        script: S<'a, T, V>,
    ) -> std::result::Result<ast::Program, String> {
        Self::p_script(script, env).map_err(|e| match e {
            Error::MissingSyntax {
                expecting,
                pos,
                node_name,
                kind,
            } => format!(
                "missing case in {:?}.\n - pos: {:?}\n - unexpected: '{:?}'\n - kind: {:?}\n",
                expecting.to_string(),
                pos,
                node_name.to_string(),
                kind,
            ),
            Error::Failwith(msg) => msg,
        })
    }
}

struct PositionedSyntaxLowerer;
impl<'a> Lowerer<'a, PositionedToken<'a>, PositionedValue<'a>, PositionedTokenFactory<'a>>
    for PositionedSyntaxLowerer
{
}

pub fn lower<'a>(
    env: &mut Env<'a, PositionedTokenFactory<'a>>,
    script: S<'a, PositionedToken<'a>, PositionedValue<'a>>,
) -> std::result::Result<ast::Program, String> {
    PositionedSyntaxLowerer::lower(env, script)
}
