// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.

use std::collections::BTreeMap;
use std::rc::Rc;

use bstr::BStr;
use bumpalo::{
    collections::{String, Vec},
    Bump,
};

use hh_autoimport_rust as hh_autoimport;
use naming_special_names_rust as naming_special_names;

use arena_collections::{AssocListMut, MultiSetMut};
use flatten_smart_constructors::{FlattenOp, FlattenSmartConstructors};
use namespaces::ElaborateKind;
use namespaces_rust as namespaces;
use oxidized_by_ref::{
    aast, aast_defs,
    ast_defs::{Bop, ClassKind, ConstraintKind, FunKind, Id, ShapeFieldName, Uop, Variance},
    decl_defs::MethodReactivity,
    direct_decl_parser::Decls,
    file_info::Mode,
    method_flags::MethodFlags,
    namespace_env::Env as NamespaceEnv,
    nast,
    pos::Pos,
    prop_flags::PropFlags,
    relative_path::RelativePath,
    s_map::SMap,
    shallow_decl_defs::{
        self, Decl, ShallowClassConst, ShallowMethod, ShallowProp, ShallowTypeconst,
    },
    shape_map::ShapeField,
    typing_defs::{
        self, Capability::*, ConstDecl, EnumType, FunArity, FunElt, FunImplicitParams, FunParam,
        FunParams, FunType, IfcFunDecl, ParamMode, ParamMutability, ParamRxAnnotation,
        PossiblyEnforcedTy, Reactivity, RecordFieldReq, ShapeFieldType, ShapeKind, TaccessType,
        Tparam, Ty, Ty_, TypeconstAbstractKind, TypedefType, WhereConstraint, XhpAttrTag,
    },
    typing_defs_flags::{FunParamFlags, FunTypeFlags},
    typing_reason::Reason,
};
use parser_core_types::{
    compact_token::CompactToken, indexed_source_text::IndexedSourceText, source_text::SourceText,
    syntax_kind::SyntaxKind, token_factory::SimpleTokenFactoryImpl, token_kind::TokenKind,
};

mod direct_decl_smart_constructors_generated;

type SK = SyntaxKind;

type SSet<'a> = arena_collections::SortedSet<'a, &'a str>;

type NamespaceMap = BTreeMap<std::string::String, std::string::String>;

#[derive(Clone)]
pub struct DirectDeclSmartConstructors<'a> {
    pub token_factory: SimpleTokenFactoryImpl<CompactToken>,

    pub source_text: IndexedSourceText<'a>,
    pub arena: &'a bumpalo::Bump,
    pub decls: Decls<'a>,
    pub disable_xhp_element_mangling: bool,
    filename: &'a RelativePath<'a>,
    file_mode: Mode,
    namespace_builder: Rc<NamespaceBuilder<'a>>,
    classish_name_builder: ClassishNameBuilder<'a>,
    type_parameters: Rc<Vec<'a, SSet<'a>>>,

    previous_token_kind: TokenKind,
}

impl<'a> DirectDeclSmartConstructors<'a> {
    pub fn new(
        src: &SourceText<'a>,
        file_mode: Mode,
        disable_xhp_element_mangling: bool,
        auto_namespace_map: &'a NamespaceMap,
        arena: &'a Bump,
    ) -> Self {
        let source_text = IndexedSourceText::new(src.clone());
        let path = source_text.source_text().file_path();
        let prefix = path.prefix();
        let path = String::from_str_in(path.path_str(), arena).into_bump_str();
        let filename = RelativePath::make(prefix, path);
        Self {
            token_factory: SimpleTokenFactoryImpl::new(),

            source_text,
            arena,
            filename: arena.alloc(filename),
            file_mode,
            disable_xhp_element_mangling,
            decls: Decls::empty(),
            namespace_builder: Rc::new(NamespaceBuilder::new_in(
                auto_namespace_map,
                disable_xhp_element_mangling,
                arena,
            )),
            classish_name_builder: ClassishNameBuilder::new(),
            type_parameters: Rc::new(Vec::new_in(arena)),
            // EndOfFile is used here as a None value (signifying "beginning of
            // file") to save space. There is no legitimate circumstance where
            // we would parse a token and the previous token kind would be
            // EndOfFile.
            previous_token_kind: TokenKind::EndOfFile,
        }
    }

    #[inline(always)]
    pub fn alloc<T>(&self, val: T) -> &'a T {
        self.arena.alloc(val)
    }

    fn qualified_name_from_parts(&self, parts: &'a [Node<'a>], pos: &'a Pos<'a>) -> Id<'a> {
        // Count the length of the qualified name, so that we can allocate
        // exactly the right amount of space for it in our arena.
        let mut len = 0;
        for part in parts {
            match part {
                Node::Name(&(name, _)) => len += name.len(),
                Node::Token(t) if t.kind() == TokenKind::Backslash => len += 1,
                Node::ListItem(&(Node::Name(&(name, _)), _backslash)) => len += name.len() + 1,
                Node::ListItem(&(Node::Token(t), _backslash))
                    if t.kind() == TokenKind::Namespace =>
                {
                    len += t.width() + 1;
                }
                _ => {}
            }
        }
        // If there's no internal trivia, then we can just reference the
        // qualified name in the original source text instead of copying it.
        let source_len = pos.end_cnum() - pos.start_cnum();
        if source_len == len {
            let qualified_name = self.str_from_utf8(self.source_text_at_pos(pos));
            return Id(pos, qualified_name);
        }
        // Allocate `len` bytes and fill them with the fully qualified name.
        let mut qualified_name = String::with_capacity_in(len, self.arena);
        for part in parts {
            match part {
                Node::Name(&(name, _pos)) => qualified_name.push_str(&name),
                Node::Token(t) if t.kind() == TokenKind::Backslash => qualified_name.push('\\'),
                &Node::ListItem(&(Node::Name(&(name, _)), _backslash)) => {
                    qualified_name.push_str(&name);
                    qualified_name.push_str("\\");
                }
                &Node::ListItem(&(Node::Token(t), _backslash))
                    if t.kind() == TokenKind::Namespace =>
                {
                    qualified_name.push_str("namespace\\");
                }
                Node::ListItem(listitem) => {
                    panic!(
                        "Expected ListItem with name and backslash, but got {:?}",
                        listitem
                    );
                }
                n => {
                    panic!("Expected a name, backslash, or list item, but got {:?}", n);
                }
            }
        }
        debug_assert_eq!(len, qualified_name.len());
        debug_assert_eq!(len, qualified_name.capacity());
        Id(pos, qualified_name.into_bump_str())
    }

    /// If the given node is an identifier, XHP name, or qualified name,
    /// elaborate it in the current namespace and return Some. To be used for
    /// the name of a decl in its definition (e.g., "C" in `class C {}` or "f"
    /// in `function f() {}`).
    fn elaborate_defined_id(&self, name: Node<'a>) -> Option<Id<'a>> {
        let id = match name {
            Node::Name(&(name, pos)) => Id(pos, name),
            Node::XhpName(&(name, pos)) => Id(pos, name),
            Node::QualifiedName(&(parts, pos)) => self.qualified_name_from_parts(parts, pos),
            _ => return None,
        };
        Some(self.namespace_builder.elaborate_defined_id(id))
    }

    /// If the given node is a name (i.e., an identifier or a qualified name),
    /// return Some. No namespace elaboration is performed.
    fn expect_name(&self, name: Node<'a>) -> Option<Id<'a>> {
        // If it's a simple identifier, return it.
        if let id @ Some(_) = name.as_id() {
            return id;
        }
        match name {
            Node::QualifiedName(&(parts, pos)) => Some(self.qualified_name_from_parts(parts, pos)),
            Node::Token(t) if t.kind() == TokenKind::XHP => {
                let pos = self.token_pos(t);
                let text = self.str_from_utf8(self.source_text_at_pos(pos));
                Some(Id(pos, text))
            }
            _ => None,
        }
    }

    /// Fully qualify the given identifier as a type name (with consideration
    /// to `use` statements in scope).
    fn elaborate_id(&self, id: Id<'a>) -> Id<'a> {
        let Id(pos, name) = id;
        Id(pos, self.elaborate_raw_id(name))
    }

    /// Fully qualify the given identifier as a type name (with consideration
    /// to `use` statements in scope).
    fn elaborate_raw_id(&self, id: &'a str) -> &'a str {
        self.namespace_builder
            .elaborate_raw_id(ElaborateKind::Class, id)
    }

    /// Fully qualify the given identifier as a constant name (with
    /// consideration to `use` statements in scope).
    fn elaborate_const_id(&self, id: Id<'a>) -> Id<'a> {
        let Id(pos, name) = id;
        Id(
            pos,
            self.namespace_builder
                .elaborate_raw_id(ElaborateKind::Const, name),
        )
    }

    fn slice<T>(&self, iter: impl Iterator<Item = T>) -> &'a [T] {
        let mut result = match iter.size_hint().1 {
            Some(upper_bound) => Vec::with_capacity_in(upper_bound, self.arena),
            None => Vec::new_in(self.arena),
        };
        for item in iter {
            result.push(item);
        }
        result.into_bump_slice()
    }

    fn unwrap_mutability(hint: Node<'a>) -> (Node<'a>, Option<ParamMutability>) {
        match hint {
            Node::Ty(Ty(_, Ty_::Tapply((hn, [t])))) if hn.1 == "\\Mutable" => {
                (Node::Ty(t), Some(ParamMutability::ParamBorrowedMutable))
            }
            Node::Ty(Ty(_, Ty_::Tapply((hn, [t])))) if hn.1 == "\\OwnedMutable" => {
                (Node::Ty(t), Some(ParamMutability::ParamOwnedMutable))
            }
            Node::Ty(Ty(_, Ty_::Tapply((hn, [t])))) if hn.1 == "\\MaybeMutable" => {
                (Node::Ty(t), Some(ParamMutability::ParamMaybeMutable))
            }
            _ => (hint, None),
        }
    }
}

fn prefix_slash<'a>(arena: &'a Bump, name: &str) -> &'a str {
    let mut s = String::with_capacity_in(1 + name.len(), arena);
    s.push('\\');
    s.push_str(name);
    s.into_bump_str()
}

fn prefix_colon<'a>(arena: &'a Bump, name: &str) -> &'a str {
    let mut s = String::with_capacity_in(1 + name.len(), arena);
    s.push(':');
    s.push_str(name);
    s.into_bump_str()
}

fn concat<'a>(arena: &'a Bump, str1: &str, str2: &str) -> &'a str {
    let mut result = String::with_capacity_in(str1.len() + str2.len(), arena);
    result.push_str(str1);
    result.push_str(str2);
    result.into_bump_str()
}

fn str_from_utf8<'a>(arena: &'a Bump, slice: &'a [u8]) -> &'a str {
    if let Ok(s) = std::str::from_utf8(slice) {
        s
    } else {
        String::from_utf8_lossy_in(slice, arena).into_bump_str()
    }
}

fn strip_dollar_prefix<'a>(name: &'a str) -> &'a str {
    name.trim_start_matches("$")
}

const TANY_: Ty_<'_> = Ty_::Tany(oxidized_by_ref::tany_sentinel::TanySentinel);
const TANY: &Ty<'_> = &Ty(Reason::none(), TANY_);

fn tany() -> &'static Ty<'static> {
    TANY
}

fn default_ifc_fun_decl<'a>() -> IfcFunDecl<'a> {
    IfcFunDecl::FDPolicied(Some("PUBLIC"))
}

#[derive(Debug)]
struct Modifiers {
    is_static: bool,
    visibility: aast::Visibility,
    is_abstract: bool,
    is_final: bool,
}

fn read_member_modifiers<'a: 'b, 'b>(modifiers: impl Iterator<Item = &'b Node<'a>>) -> Modifiers {
    let mut ret = Modifiers {
        is_static: false,
        visibility: aast::Visibility::Public,
        is_abstract: false,
        is_final: false,
    };
    for modifier in modifiers {
        if let Some(vis) = modifier.as_visibility() {
            ret.visibility = vis;
        }
        match modifier.token_kind() {
            Some(TokenKind::Static) => ret.is_static = true,
            Some(TokenKind::Abstract) => ret.is_abstract = true,
            Some(TokenKind::Final) => ret.is_final = true,
            _ => {}
        }
    }
    ret
}

#[derive(Clone, Debug)]
struct NamespaceBuilder<'a> {
    arena: &'a Bump,
    stack: Vec<'a, NamespaceEnv<'a>>,
    auto_ns_map: &'a [(&'a str, &'a str)],
}

impl<'a> NamespaceBuilder<'a> {
    fn new_in(
        auto_ns_map: &'a NamespaceMap,
        disable_xhp_element_mangling: bool,
        arena: &'a Bump,
    ) -> Self {
        let mut arena_ns_map = Vec::with_capacity_in(auto_ns_map.len(), arena);
        for (k, v) in auto_ns_map.iter() {
            arena_ns_map.push((k.as_str(), v.as_str()));
        }
        let auto_ns_map = arena_ns_map.into_bump_slice();

        let mut ns_uses = SMap::empty();
        for &alias in hh_autoimport::NAMESPACES {
            ns_uses = ns_uses.add(arena, alias, concat(arena, "HH\\", alias));
        }
        for (alias, ns) in auto_ns_map.iter() {
            ns_uses = ns_uses.add(arena, alias, ns);
        }

        let mut class_uses = SMap::empty();
        for &alias in hh_autoimport::TYPES {
            class_uses = class_uses.add(arena, alias, concat(arena, "HH\\", alias));
        }

        NamespaceBuilder {
            arena,
            stack: bumpalo::vec![in arena; NamespaceEnv {
                ns_uses,
                class_uses,
                fun_uses: SMap::empty(),
                const_uses: SMap::empty(),
                record_def_uses: SMap::empty(),
                name: None,
                auto_ns_map,
                is_codegen: false,
                disable_xhp_element_mangling,
            }],
            auto_ns_map,
        }
    }

    fn empty_with_ns_in(ns: &'a str, arena: &'a Bump) -> Self {
        NamespaceBuilder {
            arena,
            stack: bumpalo::vec![in arena; NamespaceEnv {
                ns_uses: SMap::empty(),
                class_uses: SMap::empty(),
                fun_uses: SMap::empty(),
                const_uses: SMap::empty(),
                record_def_uses: SMap::empty(),
                name: Some(ns),
                auto_ns_map: &[],
                is_codegen: false,
                disable_xhp_element_mangling: false,
            }],
            auto_ns_map: &[],
        }
    }

    fn push_namespace(&mut self, name: Option<&str>) {
        let current = self.current_namespace();
        let nsenv = self.stack.last().unwrap().clone(); // shallow clone
        if let Some(name) = name {
            let mut fully_qualified = match current {
                None => String::with_capacity_in(name.len(), self.arena),
                Some(current) => {
                    let mut fully_qualified =
                        String::with_capacity_in(current.len() + name.len() + 1, self.arena);
                    fully_qualified.push_str(current);
                    fully_qualified.push('\\');
                    fully_qualified
                }
            };
            fully_qualified.push_str(name);
            self.stack.push(NamespaceEnv {
                name: Some(fully_qualified.into_bump_str()),
                ..nsenv
            });
        } else {
            self.stack.push(NamespaceEnv {
                name: current,
                ..nsenv
            });
        }
    }

    fn pop_namespace(&mut self) {
        // We'll never push a namespace for a declaration of items in the global
        // namespace (e.g., `namespace { ... }`), so only pop if we are in some
        // namespace other than the global one.
        if self.stack.len() > 1 {
            self.stack.pop().unwrap();
        }
    }

    // push_namespace(Y) + pop_namespace() + push_namespace(X) should be equivalent to
    // push_namespace(Y) + push_namespace(X) + pop_previous_namespace()
    fn pop_previous_namespace(&mut self) {
        if self.stack.len() > 2 {
            let last = self.stack.pop().unwrap().name.unwrap_or("\\");
            let previous = self.stack.pop().unwrap().name.unwrap_or("\\");
            assert!(last.starts_with(previous));
            let name = &last[previous.len() + 1..last.len()];
            self.push_namespace(Some(name));
        }
    }

    fn current_namespace(&self) -> Option<&'a str> {
        self.stack.last().and_then(|nsenv| nsenv.name)
    }

    fn add_import(&mut self, kind: TokenKind, name: &'a str, aliased_name: Option<&'a str>) {
        let stack_top = &mut self
            .stack
            .last_mut()
            .expect("Attempted to get the current import map, but namespace stack was empty");
        let aliased_name = aliased_name.unwrap_or_else(|| {
            name.rsplit_terminator('\\')
                .nth(0)
                .expect("Expected at least one entry in import name")
        });
        let name = name.trim_end_matches('\\');
        let name = if name.starts_with('\\') {
            name
        } else {
            prefix_slash(self.arena, name)
        };
        match kind {
            TokenKind::Type => {
                stack_top.class_uses = stack_top.class_uses.add(self.arena, aliased_name, name);
            }
            TokenKind::Namespace => {
                stack_top.ns_uses = stack_top.ns_uses.add(self.arena, aliased_name, name);
            }
            TokenKind::Mixed => {
                stack_top.class_uses = stack_top.class_uses.add(self.arena, aliased_name, name);
                stack_top.ns_uses = stack_top.ns_uses.add(self.arena, aliased_name, name);
            }
            _ => panic!("Unexpected import kind: {:?}", kind),
        }
    }

    fn elaborate_raw_id(&self, kind: ElaborateKind, name: &'a str) -> &'a str {
        if name.starts_with('\\') {
            return name;
        }
        let env = self.stack.last().unwrap();
        namespaces::elaborate_raw_id_in(self.arena, env, kind, name)
    }

    fn elaborate_defined_id(&self, id: Id<'a>) -> Id<'a> {
        let Id(pos, name) = id;
        let env = self.stack.last().unwrap();
        let name = if env.disable_xhp_element_mangling && name.contains(':') {
            let xhp_name_opt = namespaces::elaborate_xhp_namespace(name);
            let name = xhp_name_opt.map_or(name, |s| self.arena.alloc_str(&s));
            if !name.starts_with('\\') {
                namespaces::elaborate_into_current_ns_in(self.arena, env, name)
            } else {
                name
            }
        } else {
            namespaces::elaborate_into_current_ns_in(self.arena, env, name)
        };
        Id(pos, name)
    }
}

#[derive(Clone, Debug)]
enum ClassishNameBuilder<'a> {
    /// We are not in a classish declaration.
    NotInClassish,

    /// We saw a classish keyword token followed by a Name, so we make it
    /// available as the name of the containing class declaration.
    InClassish(&'a (&'a str, &'a Pos<'a>, TokenKind)),
}

impl<'a> ClassishNameBuilder<'a> {
    fn new() -> Self {
        ClassishNameBuilder::NotInClassish
    }

    fn lexed_name_after_classish_keyword(
        &mut self,
        arena: &'a Bump,
        name: &'a str,
        pos: &'a Pos<'a>,
        token_kind: TokenKind,
    ) {
        use ClassishNameBuilder::*;
        match self {
            NotInClassish => {
                let name = if name.starts_with(':') {
                    prefix_slash(arena, name)
                } else {
                    name
                };
                *self = InClassish(arena.alloc((name, pos, token_kind)))
            }
            InClassish(_) => {}
        }
    }

    fn parsed_classish_declaration(&mut self) {
        *self = ClassishNameBuilder::NotInClassish;
    }

    fn get_current_classish_name(&self) -> Option<(&'a str, &'a Pos<'a>)> {
        use ClassishNameBuilder::*;
        match self {
            NotInClassish => None,
            InClassish((name, pos, _)) => Some((name, pos)),
        }
    }

    fn in_interface(&self) -> bool {
        use ClassishNameBuilder::*;
        match self {
            InClassish((_, _, TokenKind::Interface)) => true,
            InClassish((_, _, _)) | NotInClassish => false,
        }
    }
}

#[derive(Debug)]
pub struct FunParamDecl<'a> {
    attributes: Node<'a>,
    visibility: Node<'a>,
    kind: ParamMode,
    hint: Node<'a>,
    pos: &'a Pos<'a>,
    name: Option<&'a str>,
    variadic: bool,
    initializer: Node<'a>,
}

#[derive(Debug)]
pub struct FunctionHeader<'a> {
    name: Node<'a>,
    modifiers: Node<'a>,
    type_params: Node<'a>,
    param_list: Node<'a>,
    capability: Node<'a>,
    ret_hint: Node<'a>,
    where_constraints: Node<'a>,
}

#[derive(Debug)]
pub struct RequireClause<'a> {
    require_type: Node<'a>,
    name: Node<'a>,
}

#[derive(Debug)]
pub struct TypeParameterDecl<'a> {
    name: Node<'a>,
    reified: aast::ReifyKind,
    variance: Variance,
    constraints: &'a [(ConstraintKind, Node<'a>)],
    tparam_params: &'a [&'a Tparam<'a>],
    user_attributes: &'a [&'a UserAttributeNode<'a>],
}

#[derive(Debug)]
pub struct ClosureTypeHint<'a> {
    args: Node<'a>,
    ret_hint: Node<'a>,
}

#[derive(Debug)]
pub struct NamespaceUseClause<'a> {
    kind: TokenKind,
    id: Id<'a>,
    as_: Option<&'a str>,
}

#[derive(Debug)]
pub struct ConstructorNode<'a> {
    method: &'a ShallowMethod<'a>,
    properties: &'a [ShallowProp<'a>],
}

#[derive(Debug)]
pub struct MethodNode<'a> {
    method: &'a ShallowMethod<'a>,
    is_static: bool,
}

#[derive(Debug)]
pub struct PropertyNode<'a> {
    decls: &'a [ShallowProp<'a>],
    is_static: bool,
}

#[derive(Debug)]
pub struct XhpClassAttributeDeclarationNode<'a> {
    xhp_attr_decls: &'a [ShallowProp<'a>],
    xhp_attr_uses_decls: &'a [Node<'a>],
}

#[derive(Debug)]
pub struct XhpClassAttributeNode<'a> {
    name: Id<'a>,
    tag: Option<XhpAttrTag>,
    needs_init: bool,
    nullable: bool,
    hint: Node<'a>,
}

#[derive(Debug)]
pub struct ShapeFieldNode<'a> {
    name: &'a ShapeField<'a>,
    type_: &'a ShapeFieldType<'a>,
}

#[derive(Copy, Clone, Debug)]
struct ClassNameParam<'a> {
    name: Id<'a>,
    full_pos: &'a Pos<'a>, // Position of the full expression `Foo::class`
}

#[derive(Debug)]
pub struct UserAttributeNode<'a> {
    name: Id<'a>,
    classname_params: &'a [ClassNameParam<'a>],
    string_literal_params: &'a [&'a BStr], // this is only used for __Deprecated attribute message and Cipp parameters
}

mod fixed_width_token {
    use parser_core_types::token_kind::TokenKind;
    use std::convert::TryInto;

    #[derive(Copy, Clone)]
    pub struct FixedWidthToken(u64); // { offset: u56, kind: TokenKind }

    const KIND_BITS: u8 = 8;
    const KIND_MASK: u64 = u8::MAX as u64;
    const MAX_OFFSET: u64 = !(KIND_MASK << (64 - KIND_BITS));

    impl FixedWidthToken {
        pub fn new(kind: TokenKind, offset: usize) -> Self {
            // We don't want to spend bits tracking the width of fixed-width
            // tokens. Since we don't track width, verify that this token kind
            // is in fact a fixed-width kind.
            debug_assert!(kind.fixed_width().is_some());

            let offset: u64 = offset.try_into().unwrap();
            if offset > MAX_OFFSET {
                panic!("FixedWidthToken: offset too large");
            }
            Self(offset << KIND_BITS | kind as u8 as u64)
        }

        pub fn offset(self) -> usize {
            (self.0 >> KIND_BITS).try_into().unwrap()
        }

        pub fn kind(self) -> TokenKind {
            TokenKind::try_from_u8(self.0 as u8).unwrap()
        }

        pub fn width(self) -> usize {
            self.kind().fixed_width().unwrap().get()
        }
    }

    impl std::fmt::Debug for FixedWidthToken {
        fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
            fmt.debug_struct("FixedWidthToken")
                .field("kind", &self.kind())
                .field("offset", &self.offset())
                .finish()
        }
    }
}
use fixed_width_token::FixedWidthToken;

#[derive(Copy, Clone, Debug)]
pub enum Node<'a> {
    // Nodes which are not useful in constructing a decl are ignored. We keep
    // track of the SyntaxKind for two reasons.
    //
    // One is that the parser needs to know the SyntaxKind of a parsed node in
    // some circumstances (this information is exposed to the parser via an
    // implementation of `smart_constructors::NodeType`). An adapter called
    // WithKind exists to provide a `NodeType` implementation for arbitrary
    // nodes by pairing each node with a SyntaxKind, but in the direct decl
    // parser, we want to avoid the extra 8 bytes of overhead on each node.
    //
    // The second reason is that debugging is difficult when nodes are silently
    // ignored, and providing at least the SyntaxKind of an ignored node helps
    // in tracking down the reason it was ignored.
    Ignored(SyntaxKind),

    List(&'a &'a [Node<'a>]),
    BracketedList(&'a (&'a Pos<'a>, &'a [Node<'a>], &'a Pos<'a>)),
    Name(&'a (&'a str, &'a Pos<'a>)),
    XhpName(&'a (&'a str, &'a Pos<'a>)),
    QualifiedName(&'a (&'a [Node<'a>], &'a Pos<'a>)),
    StringLiteral(&'a (&'a BStr, &'a Pos<'a>)), // For shape keys and const expressions.
    IntLiteral(&'a (&'a str, &'a Pos<'a>)),     // For const expressions.
    FloatingLiteral(&'a (&'a str, &'a Pos<'a>)), // For const expressions.
    BooleanLiteral(&'a (&'a str, &'a Pos<'a>)), // For const expressions.
    Ty(&'a Ty<'a>),
    ListItem(&'a (Node<'a>, Node<'a>)),
    Const(&'a ShallowClassConst<'a>),
    ConstInitializer(&'a (Node<'a>, Node<'a>)), // Name, initializer expression
    FunParam(&'a FunParamDecl<'a>),
    Attribute(&'a UserAttributeNode<'a>),
    FunctionHeader(&'a FunctionHeader<'a>),
    Constructor(&'a ConstructorNode<'a>),
    Method(&'a MethodNode<'a>),
    Property(&'a PropertyNode<'a>),
    TraitUse(&'a Node<'a>),
    XhpClassAttributeDeclaration(&'a XhpClassAttributeDeclarationNode<'a>),
    XhpClassAttribute(&'a XhpClassAttributeNode<'a>),
    XhpAttributeUse(&'a Node<'a>),
    TypeConstant(&'a ShallowTypeconst<'a>),
    RequireClause(&'a RequireClause<'a>),
    ClassishBody(&'a &'a [Node<'a>]),
    TypeParameter(&'a TypeParameterDecl<'a>),
    TypeConstraint(&'a (ConstraintKind, Node<'a>)),
    ShapeFieldSpecifier(&'a ShapeFieldNode<'a>),
    NamespaceUseClause(&'a NamespaceUseClause<'a>),
    Expr(&'a nast::Expr<'a>),
    TypeParameters(&'a &'a [&'a Tparam<'a>]),
    WhereConstraint(&'a WhereConstraint<'a>),
    RecordField(&'a (Id<'a>, RecordFieldReq)),

    // Non-ignored, fixed-width tokens (e.g., keywords, operators, braces, etc.).
    Token(FixedWidthToken),
}

impl<'a> smart_constructors::NodeType for Node<'a> {
    type R = Node<'a>;

    fn extract(self) -> Self::R {
        self
    }

    fn is_abstract(&self) -> bool {
        self.is_token(TokenKind::Abstract)
            || matches!(self, Node::Ignored(SK::Token(TokenKind::Abstract)))
    }
    fn is_name(&self) -> bool {
        matches!(self, Node::Name(..)) || matches!(self, Node::Ignored(SK::Token(TokenKind::Name)))
    }
    fn is_qualified_name(&self) -> bool {
        matches!(self, Node::QualifiedName(..)) || matches!(self, Node::Ignored(SK::QualifiedName))
    }
    fn is_prefix_unary_expression(&self) -> bool {
        matches!(self, Node::Expr(aast::Expr(_, aast::Expr_::Unop(..))))
            || matches!(self, Node::Ignored(SK::PrefixUnaryExpression))
    }
    fn is_scope_resolution_expression(&self) -> bool {
        matches!(self, Node::Expr(aast::Expr(_, aast::Expr_::ClassConst(..))))
            || matches!(self, Node::Ignored(SK::ScopeResolutionExpression))
    }
    fn is_missing(&self) -> bool {
        matches!(self, Node::Ignored(SK::Missing))
    }
    fn is_variable_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::VariableExpression))
    }
    fn is_subscript_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::SubscriptExpression))
    }
    fn is_member_selection_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::MemberSelectionExpression))
    }
    fn is_object_creation_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::ObjectCreationExpression))
    }
    fn is_safe_member_selection_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::SafeMemberSelectionExpression))
    }
    fn is_function_call_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::FunctionCallExpression))
    }
    fn is_list_expression(&self) -> bool {
        matches!(self, Node::Ignored(SK::ListExpression))
    }
}

impl<'a> Node<'a> {
    fn is_token(self, kind: TokenKind) -> bool {
        self.token_kind() == Some(kind)
    }

    fn token_kind(self) -> Option<TokenKind> {
        match self {
            Node::Token(token) => Some(token.kind()),
            _ => None,
        }
    }

    fn as_slice(self, b: &'a Bump) -> &'a [Self] {
        match self {
            Node::List(&items) | Node::BracketedList(&(_, items, _)) => items,
            n if n.is_ignored() => &[],
            n => std::slice::from_ref(b.alloc(n)),
        }
    }

    fn iter<'b>(&'b self) -> NodeIterHelper<'a, 'b>
    where
        'a: 'b,
    {
        match self {
            &Node::List(&items) | Node::BracketedList(&(_, items, _)) => {
                NodeIterHelper::Vec(items.iter())
            }
            n if n.is_ignored() => NodeIterHelper::Empty,
            n => NodeIterHelper::Single(n),
        }
    }

    // The number of elements which would be yielded by `self.iter()`.
    // Must return the upper bound returned by NodeIterHelper::size_hint.
    fn len(&self) -> usize {
        match self {
            &Node::List(&items) | Node::BracketedList(&(_, items, _)) => items.len(),
            n if n.is_ignored() => 0,
            _ => 1,
        }
    }

    fn as_visibility(&self) -> Option<aast::Visibility> {
        match self.token_kind() {
            Some(TokenKind::Private) => Some(aast::Visibility::Private),
            Some(TokenKind::Protected) => Some(aast::Visibility::Protected),
            Some(TokenKind::Public) => Some(aast::Visibility::Public),
            _ => None,
        }
    }

    // If this node is a simple unqualified identifier, return its position and text.
    fn as_id(&self) -> Option<Id<'a>> {
        match self {
            Node::Name(&(name, pos)) | Node::XhpName(&(name, pos)) => Some(Id(pos, name)),
            _ => None,
        }
    }

    fn is_ignored(&self) -> bool {
        matches!(self, Node::Ignored(..))
    }

    fn is_present(&self) -> bool {
        !self.is_ignored()
    }
}

struct Attributes<'a> {
    reactivity: Reactivity<'a>,
    reactivity_condition_type: Option<&'a Ty<'a>>,
    param_mutability: Option<ParamMutability>,
    deprecated: Option<&'a str>,
    reifiable: Option<&'a Pos<'a>>,
    returns_mutable: bool,
    late_init: bool,
    const_: bool,
    lsb: bool,
    memoizelsb: bool,
    override_: bool,
    at_most_rx_as_func: bool,
    enforceable: Option<&'a Pos<'a>>,
    returns_void_to_rx: bool,
    accept_disposable: bool,
    dynamically_callable: bool,
    returns_disposable: bool,
    php_std_lib: bool,
    ifc_attribute: IfcFunDecl<'a>,
    external: bool,
    can_call: bool,
    atom: bool,
}

impl<'a> DirectDeclSmartConstructors<'a> {
    fn add_class(&mut self, name: &'a str, decl: &'a shallow_decl_defs::ShallowClass<'a>) {
        self.decls.add(name, Decl::Class(decl), self.arena);
    }
    fn add_fun(&mut self, name: &'a str, decl: &'a typing_defs::FunElt<'a>) {
        self.decls.add(name, Decl::Fun(decl), self.arena);
    }
    fn add_typedef(&mut self, name: &'a str, decl: &'a typing_defs::TypedefType<'a>) {
        self.decls.add(name, Decl::Typedef(decl), self.arena);
    }
    fn add_const(&mut self, name: &'a str, decl: &'a typing_defs::ConstDecl<'a>) {
        self.decls.add(name, Decl::Const(decl), self.arena);
    }
    fn add_record(&mut self, name: &'a str, decl: &'a typing_defs::RecordDefType<'a>) {
        self.decls.add(name, Decl::Record(decl), self.arena);
    }

    fn token_bytes(&self, token: &CompactToken) -> &'a [u8] {
        self.source_text
            .source_text()
            .sub(token.start_offset(), token.width())
    }

    // Check that the slice is valid UTF-8. If it is, return a &str referencing
    // the same data. Otherwise, copy the slice into our arena using
    // String::from_utf8_lossy_in, and return a reference to the arena str.
    fn str_from_utf8(&self, slice: &'a [u8]) -> &'a str {
        str_from_utf8(self.arena, slice)
    }

    fn merge(
        &self,
        pos1: impl Into<Option<&'a Pos<'a>>>,
        pos2: impl Into<Option<&'a Pos<'a>>>,
    ) -> &'a Pos<'a> {
        match (pos1.into(), pos2.into()) {
            (None, None) => Pos::none(),
            (Some(pos), None) | (None, Some(pos)) => pos,
            (Some(pos1), Some(pos2)) => match (pos1.is_none(), pos2.is_none()) {
                (true, true) => Pos::none(),
                (true, false) => pos2,
                (false, true) => pos1,
                (false, false) => Pos::merge_without_checking_filename(self.arena, pos1, pos2),
            },
        }
    }

    fn merge_positions(&self, node1: Node<'a>, node2: Node<'a>) -> &'a Pos<'a> {
        self.merge(self.get_pos(node1), self.get_pos(node2))
    }

    fn pos_from_slice(&self, nodes: &[Node<'a>]) -> &'a Pos<'a> {
        nodes.iter().fold(Pos::none(), |acc, &node| {
            self.merge(acc, self.get_pos(node))
        })
    }

    fn get_pos(&self, node: Node<'a>) -> &'a Pos<'a> {
        self.get_pos_opt(node).unwrap_or(Pos::none())
    }

    fn get_pos_opt(&self, node: Node<'a>) -> Option<&'a Pos<'a>> {
        let pos = match node {
            Node::Name(&(_, pos)) => pos,
            Node::Ty(ty) => return ty.get_pos(),
            Node::XhpName(&(_, pos)) => pos,
            Node::QualifiedName(&(_, pos)) => pos,
            Node::IntLiteral(&(_, pos))
            | Node::FloatingLiteral(&(_, pos))
            | Node::StringLiteral(&(_, pos))
            | Node::BooleanLiteral(&(_, pos)) => pos,
            Node::ListItem(&(fst, snd)) => self.merge_positions(fst, snd),
            Node::List(items) => self.pos_from_slice(&items),
            Node::BracketedList(&(first_pos, inner_list, second_pos)) => self.merge(
                first_pos,
                self.merge(self.pos_from_slice(inner_list), second_pos),
            ),
            Node::Expr(&aast::Expr(pos, _)) => pos,
            Node::Token(token) => self.token_pos(token),
            _ => return None,
        };
        if pos.is_none() { None } else { Some(pos) }
    }

    fn token_pos(&self, token: FixedWidthToken) -> &'a Pos<'a> {
        let start = token.offset();
        let end = start + token.width();
        let start = self.source_text.offset_to_file_pos_triple(start);
        let end = self.source_text.offset_to_file_pos_triple(end);
        Pos::from_lnum_bol_cnum(self.arena, self.filename, start, end)
    }

    fn node_to_expr(&self, node: Node<'a>) -> Option<&'a nast::Expr<'a>> {
        let expr_ = match node {
            Node::Expr(expr) => return Some(expr),
            Node::IntLiteral(&(s, _)) => aast::Expr_::Int(s),
            Node::FloatingLiteral(&(s, _)) => aast::Expr_::Float(s),
            Node::StringLiteral(&(s, _)) => aast::Expr_::String(s),
            Node::BooleanLiteral((s, _)) => {
                if s.eq_ignore_ascii_case("true") {
                    aast::Expr_::True
                } else {
                    aast::Expr_::False
                }
            }
            Node::Token(t) if t.kind() == TokenKind::NullLiteral => aast::Expr_::Null,
            Node::Name(..) | Node::QualifiedName(..) => {
                aast::Expr_::Id(self.alloc(self.elaborate_const_id(self.expect_name(node)?)))
            }
            _ => return None,
        };
        let pos = self.get_pos(node);
        Some(self.alloc(aast::Expr(pos, expr_)))
    }

    fn node_to_non_ret_ty(&self, node: Node<'a>) -> Option<&'a Ty<'a>> {
        self.node_to_ty_(node, false)
    }

    fn node_to_ty(&self, node: Node<'a>) -> Option<&'a Ty<'a>> {
        self.node_to_ty_(node, true)
    }

    fn node_to_ty_(&self, node: Node<'a>, allow_non_ret_ty: bool) -> Option<&'a Ty<'a>> {
        match node {
            Node::Ty(Ty(reason, Ty_::Tprim(aast::Tprim::Tvoid))) if !allow_non_ret_ty => {
                Some(self.alloc(Ty(reason, Ty_::Terr)))
            }
            Node::Ty(Ty(reason, Ty_::Tprim(aast::Tprim::Tnoreturn))) if !allow_non_ret_ty => {
                Some(self.alloc(Ty(reason, Ty_::Terr)))
            }
            Node::Ty(ty) => Some(ty),
            Node::Expr(expr) => {
                fn expr_to_ty<'a>(arena: &'a Bump, expr: &'a nast::Expr<'a>) -> Option<Ty_<'a>> {
                    use aast::Expr_::*;
                    match expr.1 {
                        Null => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tnull))),
                        This => Some(Ty_::Tthis),
                        True | False => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tbool))),
                        Int(_) => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tint))),
                        Float(_) => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tfloat))),
                        String(_) => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tstring))),
                        String2(_) => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tstring))),
                        PrefixedString(_) => Some(Ty_::Tprim(arena.alloc(aast::Tprim::Tstring))),
                        Unop(&(_op, expr)) => expr_to_ty(arena, expr),
                        Any => Some(TANY_),

                        ArrayGet(_) | As(_) | Await(_) | Binop(_) | Call(_) | Callconv(_)
                        | Cast(_) | ClassConst(_) | ClassGet(_) | Clone(_) | Collection(_)
                        | Darray(_) | Dollardollar(_) | Efun(_) | Eif(_) | EnumAtom(_)
                        | ETSplice(_) | ExpressionTree(_) | FunctionPointer(_) | FunId(_)
                        | Id(_) | Import(_) | Is(_) | KeyValCollection(_) | Lfun(_) | List(_)
                        | Lplaceholder(_) | Lvar(_) | MethodCaller(_) | MethodId(_) | New(_)
                        | ObjGet(_) | Omitted | Pair(_) | Pipe(_) | Record(_) | Shape(_)
                        | SmethodId(_) | ValCollection(_) | Varray(_) | Xml(_) | Yield(_) => None,
                    }
                }

                Some(self.alloc(Ty(
                    self.alloc(Reason::witness(expr.0)),
                    expr_to_ty(self.arena, expr)?,
                )))
            }
            Node::IntLiteral((_, pos)) => Some(self.alloc(Ty(
                self.alloc(Reason::witness(pos)),
                Ty_::Tprim(self.alloc(aast::Tprim::Tint)),
            ))),
            Node::FloatingLiteral((_, pos)) => Some(self.alloc(Ty(
                self.alloc(Reason::witness(pos)),
                Ty_::Tprim(self.alloc(aast::Tprim::Tfloat)),
            ))),
            Node::StringLiteral((_, pos)) => Some(self.alloc(Ty(
                self.alloc(Reason::witness(pos)),
                Ty_::Tprim(self.alloc(aast::Tprim::Tstring)),
            ))),
            Node::BooleanLiteral((_, pos)) => Some(self.alloc(Ty(
                self.alloc(Reason::witness(pos)),
                Ty_::Tprim(self.alloc(aast::Tprim::Tbool)),
            ))),
            Node::Token(t) if t.kind() == TokenKind::Varray => {
                let pos = self.token_pos(t);
                let tany = self.alloc(Ty(self.alloc(Reason::hint(pos)), TANY_));
                Some(self.alloc(Ty(self.alloc(Reason::hint(pos)), Ty_::Tvarray(tany))))
            }
            Node::Token(t) if t.kind() == TokenKind::Darray => {
                let pos = self.token_pos(t);
                let tany = self.alloc(Ty(self.alloc(Reason::hint(pos)), TANY_));
                Some(self.alloc(Ty(
                    self.alloc(Reason::hint(pos)),
                    Ty_::Tdarray(self.alloc((tany, tany))),
                )))
            }
            Node::Token(t) if t.kind() == TokenKind::This => {
                Some(self.alloc(Ty(self.alloc(Reason::hint(self.token_pos(t))), Ty_::Tthis)))
            }
            Node::Token(t) if t.kind() == TokenKind::NullLiteral => {
                let pos = self.token_pos(t);
                Some(self.alloc(Ty(
                    self.alloc(Reason::hint(pos)),
                    Ty_::Tprim(self.alloc(aast::Tprim::Tnull)),
                )))
            }
            node => {
                let Id(pos, name) = self.expect_name(node)?;
                let reason = self.alloc(Reason::hint(pos));
                let ty_ = if self.is_type_param_in_scope(name) {
                    // TODO (T69662957) must fill type args of Tgeneric
                    Ty_::Tgeneric(self.alloc((name, &[])))
                } else {
                    match name.as_ref() {
                        "nothing" => Ty_::Tunion(&[]),
                        "nonnull" => Ty_::Tnonnull,
                        "dynamic" => Ty_::Tdynamic,
                        "varray_or_darray" => {
                            let key_type = self.varray_or_darray_key(pos);
                            let value_type = self.alloc(Ty(self.alloc(Reason::hint(pos)), TANY_));
                            Ty_::TvarrayOrDarray(self.alloc((key_type, value_type)))
                        }
                        "_" => Ty_::Terr,
                        _ => {
                            let name = self.elaborate_raw_id(name);
                            Ty_::Tapply(self.alloc((Id(pos, name), &[][..])))
                        }
                    }
                };
                Some(self.alloc(Ty(reason, ty_)))
            }
        }
    }

    fn to_attributes(&self, node: Node<'a>) -> Attributes<'a> {
        let mut attributes = Attributes {
            reactivity: Reactivity::Nonreactive,
            reactivity_condition_type: None,
            param_mutability: None,
            deprecated: None,
            reifiable: None,
            returns_mutable: false,
            late_init: false,
            const_: false,
            lsb: false,
            memoizelsb: false,
            override_: false,
            at_most_rx_as_func: false,
            enforceable: None,
            returns_void_to_rx: false,
            accept_disposable: false,
            dynamically_callable: false,
            returns_disposable: false,
            php_std_lib: false,
            ifc_attribute: default_ifc_fun_decl(),
            external: false,
            can_call: false,
            atom: false,
        };

        let nodes = match node {
            Node::List(&nodes) | Node::BracketedList(&(_, nodes, _)) => nodes,
            _ => return attributes,
        };

        // If we see the attribute `__OnlyRxIfImpl(Foo::class)`, set
        // `reactivity_condition_type` to `Foo`.
        attributes.reactivity_condition_type = nodes.iter().find_map(|attr| match attr {
            Node::Attribute(UserAttributeNode {
                name: Id(_, "__OnlyRxIfImpl"),
                classname_params: &[param],
                ..
            }) => Some(self.alloc(Ty(
                self.alloc(Reason::hint(param.full_pos)),
                Ty_::Tapply(self.alloc((param.name, &[][..]))),
            ))),
            _ => None,
        });

        let string_or_classname_arg = |attribute: &'a UserAttributeNode| {
            attribute
                .string_literal_params
                .first()
                .map(|&x| self.str_from_utf8(x))
                .or_else(|| attribute.classname_params.first().map(|x| x.name.1))
        };
        let mut ifc_already_policied = false;

        // Iterate in reverse, to match the behavior of OCaml decl in error conditions.
        for attribute in nodes.iter().rev() {
            if let Node::Attribute(attribute) = attribute {
                match attribute.name.1.as_ref() {
                    // NB: It is an error to specify more than one of __Rx,
                    // __RxShallow, and __RxLocal, so to avoid cloning the
                    // condition type, we use Option::take here.
                    "__Rx" => {
                        attributes.reactivity =
                            Reactivity::Reactive(attributes.reactivity_condition_type)
                    }
                    "__RxShallow" => {
                        attributes.reactivity =
                            Reactivity::Shallow(attributes.reactivity_condition_type)
                    }
                    "__RxLocal" => {
                        attributes.reactivity =
                            Reactivity::Local(attributes.reactivity_condition_type)
                    }
                    "__Pure" => {
                        attributes.reactivity =
                            Reactivity::Pure(attributes.reactivity_condition_type);
                    }
                    "__Cipp" => {
                        attributes.reactivity = Reactivity::Cipp(string_or_classname_arg(attribute))
                    }
                    "__CippGlobal" => {
                        attributes.reactivity = Reactivity::CippGlobal;
                    }
                    "__CippLocal" => {
                        attributes.reactivity =
                            Reactivity::CippLocal(string_or_classname_arg(attribute))
                    }
                    "__CippRx" => {
                        attributes.reactivity = Reactivity::CippRx;
                    }
                    "__Mutable" => {
                        attributes.param_mutability = Some(ParamMutability::ParamBorrowedMutable)
                    }
                    "__MaybeMutable" => {
                        attributes.param_mutability = Some(ParamMutability::ParamMaybeMutable)
                    }
                    "__OwnedMutable" => {
                        attributes.param_mutability = Some(ParamMutability::ParamOwnedMutable)
                    }
                    "__MutableReturn" => attributes.returns_mutable = true,
                    "__ReturnsVoidToRx" => attributes.returns_void_to_rx = true,
                    "__Deprecated" => {
                        attributes.deprecated = attribute
                            .string_literal_params
                            .first()
                            .map(|&x| self.str_from_utf8(x));
                    }
                    "__Reifiable" => attributes.reifiable = Some(attribute.name.0),
                    "__LateInit" => {
                        attributes.late_init = true;
                    }
                    "__Const" => {
                        attributes.const_ = true;
                    }
                    "__LSB" => {
                        attributes.lsb = true;
                    }
                    "__MemoizeLSB" => {
                        attributes.memoizelsb = true;
                    }
                    "__Override" => {
                        attributes.override_ = true;
                    }
                    "__AtMostRxAsFunc" => {
                        attributes.at_most_rx_as_func = true;
                    }
                    "__Enforceable" => {
                        attributes.enforceable = Some(attribute.name.0);
                    }
                    "__AcceptDisposable" => {
                        attributes.accept_disposable = true;
                    }
                    "__DynamicallyCallable" => {
                        attributes.dynamically_callable = true;
                    }
                    "__ReturnDisposable" => {
                        attributes.returns_disposable = true;
                    }
                    "__PHPStdLib" => {
                        attributes.php_std_lib = true;
                    }
                    "__Policied" => {
                        let string_literal_params = || {
                            attribute
                                .string_literal_params
                                .first()
                                .map(|&x| self.str_from_utf8(x))
                        };
                        // Take the classname param by default
                        attributes.ifc_attribute =
                            IfcFunDecl::FDPolicied(attribute.classname_params.first().map_or_else(
                                string_literal_params, // default
                                |&x| Some(x.name.1),   // f
                            ));
                        ifc_already_policied = true;
                    }
                    "__InferFlows" => {
                        if !ifc_already_policied {
                            attributes.ifc_attribute = IfcFunDecl::FDInferFlows;
                        }
                    }
                    "__External" => {
                        attributes.external = true;
                    }
                    "__CanCall" => {
                        attributes.can_call = true;
                    }
                    "__Atom" => {
                        attributes.atom = true;
                    }
                    _ => {}
                }
            } else {
                panic!("Expected an attribute, but was {:?}", node);
            }
        }

        attributes
    }

    // Limited version of node_to_ty that matches behavior of Decl_utils.infer_const
    fn infer_const(&self, name: Node<'a>, node: Node<'a>) -> Option<&'a Ty<'a>> {
        match node {
            Node::StringLiteral(_)
            | Node::BooleanLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatingLiteral(_)
            | Node::Expr(aast::Expr(_, aast::Expr_::Unop(&(Uop::Uminus, _))))
            | Node::Expr(aast::Expr(_, aast::Expr_::Unop(&(Uop::Uplus, _))))
            | Node::Expr(aast::Expr(_, aast::Expr_::String(..))) => self.node_to_ty(node),
            Node::Token(t) if t.kind() == TokenKind::NullLiteral => {
                let pos = self.token_pos(t);
                Some(self.alloc(Ty(
                    self.alloc(Reason::witness(pos)),
                    Ty_::Tprim(self.alloc(aast::Tprim::Tnull)),
                )))
            }
            _ => Some(self.tany_with_pos(self.get_pos(name))),
        }
    }

    fn pop_type_params(&mut self, node: Node<'a>) -> &'a [&'a Tparam<'a>] {
        match node {
            Node::TypeParameters(tparams) => {
                Rc::make_mut(&mut self.type_parameters).pop().unwrap();
                tparams
            }
            _ => &[],
        }
    }

    fn ret_from_fun_kind(&self, kind: FunKind, type_: &'a Ty<'a>) -> &'a Ty<'a> {
        let pos = type_.get_pos().unwrap_or_else(|| Pos::none());
        match kind {
            FunKind::FAsyncGenerator => self.alloc(Ty(
                self.alloc(Reason::RretFunKind(self.alloc((pos, kind)))),
                Ty_::Tapply(self.alloc((
                    Id(pos, naming_special_names::classes::ASYNC_GENERATOR),
                    self.alloc([type_, type_, type_]),
                ))),
            )),
            FunKind::FGenerator => self.alloc(Ty(
                self.alloc(Reason::RretFunKind(self.alloc((pos, kind)))),
                Ty_::Tapply(self.alloc((
                    Id(pos, naming_special_names::classes::GENERATOR),
                    self.alloc([type_, type_, type_]),
                ))),
            )),
            FunKind::FAsync => self.alloc(Ty(
                self.alloc(Reason::RretFunKind(self.alloc((pos, kind)))),
                Ty_::Tapply(self.alloc((
                    Id(pos, naming_special_names::classes::AWAITABLE),
                    self.alloc([type_]),
                ))),
            )),
            _ => type_,
        }
    }

    fn is_type_param_in_scope(&self, name: &str) -> bool {
        self.type_parameters.iter().any(|tps| tps.contains(name))
    }

    fn param_mutability_to_fun_type_flags(
        param_mutability: Option<ParamMutability>,
    ) -> FunTypeFlags {
        match param_mutability {
            Some(ParamMutability::ParamBorrowedMutable) => FunTypeFlags::MUTABLE_FLAGS_BORROWED,
            Some(ParamMutability::ParamOwnedMutable) => FunTypeFlags::MUTABLE_FLAGS_OWNED,
            Some(ParamMutability::ParamMaybeMutable) => FunTypeFlags::MUTABLE_FLAGS_MAYBE,
            None => FunTypeFlags::empty(),
        }
    }

    fn param_mutability_to_fun_param_flags(
        param_mutability: Option<ParamMutability>,
    ) -> FunParamFlags {
        match param_mutability {
            Some(ParamMutability::ParamBorrowedMutable) => FunParamFlags::MUTABLE_FLAGS_BORROWED,
            Some(ParamMutability::ParamOwnedMutable) => FunParamFlags::MUTABLE_FLAGS_OWNED,
            Some(ParamMutability::ParamMaybeMutable) => FunParamFlags::MUTABLE_FLAGS_MAYBE,
            None => FunParamFlags::empty(),
        }
    }

    fn as_fun_implicit_params(
        &mut self,
        capability: Node<'a>,
        default_pos: &'a Pos<'a>,
    ) -> &'a FunImplicitParams<'a> {
        let capability = match self.node_to_ty(capability) {
            Some(ty) => CapTy(ty),
            None => CapDefaults(default_pos),
        };
        self.alloc(FunImplicitParams { capability })
    }

    fn function_to_ty(
        &mut self,
        is_method: bool,
        attributes: Node<'a>,
        header: &'a FunctionHeader<'a>,
        body: Node,
    ) -> Option<(Id<'a>, &'a Ty<'a>, &'a [ShallowProp<'a>])> {
        let id_opt = match (is_method, header.name) {
            (true, Node::Token(t)) if t.kind() == TokenKind::Construct => {
                let pos = self.token_pos(t);
                Some(Id(pos, naming_special_names::members::__CONSTRUCT))
            }
            (true, _) => self.expect_name(header.name),
            (false, _) => self.elaborate_defined_id(header.name),
        };
        let id = id_opt.unwrap_or(Id(self.get_pos(header.name), ""));
        let (params, properties, arity) = self.as_fun_params(header.param_list)?;
        let f_pos = self.get_pos(header.name);
        let implicit_params = self.as_fun_implicit_params(header.capability, f_pos);

        let type_ = match header.name {
            Node::Token(t) if t.kind() == TokenKind::Construct => {
                let pos = self.token_pos(t);
                self.alloc(Ty(
                    self.alloc(Reason::witness(pos)),
                    Ty_::Tprim(self.alloc(aast::Tprim::Tvoid)),
                ))
            }
            _ => self
                .node_to_ty(header.ret_hint)
                .unwrap_or_else(|| self.tany_with_pos(f_pos)),
        };
        let async_ = header
            .modifiers
            .iter()
            .any(|n| n.is_token(TokenKind::Async));
        let fun_kind = if body.iter().any(|node| node.is_token(TokenKind::Yield)) {
            if async_ {
                FunKind::FAsyncGenerator
            } else {
                FunKind::FGenerator
            }
        } else {
            if async_ {
                FunKind::FAsync
            } else {
                FunKind::FSync
            }
        };
        let type_ = if !header.ret_hint.is_present() {
            self.ret_from_fun_kind(fun_kind, type_)
        } else {
            type_
        };
        let attributes = self.to_attributes(attributes);
        // TODO(hrust) Put this in a helper. Possibly do this for all flags.
        let mut flags = match fun_kind {
            FunKind::FSync => FunTypeFlags::empty(),
            FunKind::FAsync => FunTypeFlags::ASYNC,
            FunKind::FGenerator => FunTypeFlags::GENERATOR,
            FunKind::FAsyncGenerator => FunTypeFlags::ASYNC | FunTypeFlags::GENERATOR,
        };
        if attributes.returns_mutable {
            flags |= FunTypeFlags::RETURNS_MUTABLE;
        }
        if attributes.returns_disposable {
            flags |= FunTypeFlags::RETURN_DISPOSABLE;
        }
        if attributes.returns_void_to_rx {
            flags |= FunTypeFlags::RETURNS_VOID_TO_RX;
        }
        let ifc_decl = attributes.ifc_attribute;

        flags |= Self::param_mutability_to_fun_type_flags(attributes.param_mutability);
        // Pop the type params stack only after creating all inner types.
        let tparams = self.pop_type_params(header.type_params);

        let where_constraints =
            self.slice(header.where_constraints.iter().filter_map(|&x| match x {
                Node::WhereConstraint(x) => Some(x),
                _ => None,
            }));

        let ft = self.alloc(FunType {
            arity,
            tparams,
            where_constraints,
            params,
            implicit_params,
            ret: self.alloc(PossiblyEnforcedTy {
                enforced: false,
                type_,
            }),
            reactive: attributes.reactivity,
            flags,
            ifc_decl,
        });

        let ty = self.alloc(Ty(self.alloc(Reason::witness(id.0)), Ty_::Tfun(ft)));
        Some((id, ty, properties))
    }

    fn as_fun_params(
        &self,
        list: Node<'a>,
    ) -> Option<(&'a FunParams<'a>, &'a [ShallowProp<'a>], FunArity<'a>)> {
        match list {
            Node::List(nodes) => {
                let mut params = Vec::with_capacity_in(nodes.len(), self.arena);
                let mut properties = Vec::new_in(self.arena);
                let mut arity = FunArity::Fstandard;
                for node in nodes.iter() {
                    match node {
                        Node::FunParam(&FunParamDecl {
                            attributes,
                            visibility,
                            kind,
                            hint,
                            pos,
                            name,
                            variadic,
                            initializer,
                        }) => {
                            let attributes = self.to_attributes(attributes);

                            if let Some(visibility) = visibility.as_visibility() {
                                let name = name.unwrap_or("");
                                let name = strip_dollar_prefix(name);
                                let mut flags = PropFlags::empty();
                                flags.set(PropFlags::CONST, attributes.const_);
                                flags.set(PropFlags::NEEDS_INIT, self.file_mode != Mode::Mdecl);
                                flags.set(PropFlags::PHP_STD_LIB, attributes.php_std_lib);
                                properties.push(ShallowProp {
                                    xhp_attr: None,
                                    name: Id(pos, name),
                                    type_: self.node_to_ty(hint),
                                    visibility,
                                    flags,
                                });
                            }

                            let type_ = if hint.is_ignored() {
                                self.tany_with_pos(pos)
                            } else {
                                self.node_to_ty(hint).map(|ty| match ty {
                                    &Ty(r, Ty_::Tfun(fun_type))
                                        if attributes.at_most_rx_as_func =>
                                    {
                                        let fun_type = self.alloc(FunType {
                                            reactive: Reactivity::RxVar(None),
                                            ..*fun_type
                                        });
                                        self.alloc(Ty(r, Ty_::Tfun(fun_type)))
                                    }
                                    &Ty(r, Ty_::Toption(&Ty(r1, Ty_::Tfun(fun_type))))
                                        if attributes.at_most_rx_as_func =>
                                    {
                                        let fun_type = self.alloc(FunType {
                                            reactive: Reactivity::RxVar(None),
                                            ..*fun_type
                                        });
                                        self.alloc(Ty(
                                            r,
                                            Ty_::Toption(self.alloc(Ty(r1, Ty_::Tfun(fun_type)))),
                                        ))
                                    }
                                    ty => ty,
                                })?
                            };
                            // These are illegal here--they can only be used on
                            // parameters in a function type hint (see
                            // make_closure_type_specifier and unwrap_mutability).
                            // Unwrap them here anyway for better error recovery.
                            let type_ = match type_ {
                                Ty(_, Ty_::Tapply((Id(_, "\\Mutable"), [t]))) => t,
                                Ty(_, Ty_::Tapply((Id(_, "\\OwnedMutable"), [t]))) => t,
                                Ty(_, Ty_::Tapply((Id(_, "\\MaybeMutable"), [t]))) => t,
                                _ => type_,
                            };
                            let mut flags = match attributes.param_mutability {
                                Some(ParamMutability::ParamBorrowedMutable) => {
                                    FunParamFlags::MUTABLE_FLAGS_BORROWED
                                }
                                Some(ParamMutability::ParamOwnedMutable) => {
                                    FunParamFlags::MUTABLE_FLAGS_OWNED
                                }
                                Some(ParamMutability::ParamMaybeMutable) => {
                                    FunParamFlags::MUTABLE_FLAGS_MAYBE
                                }
                                None => FunParamFlags::empty(),
                            };
                            if attributes.accept_disposable {
                                flags |= FunParamFlags::ACCEPT_DISPOSABLE
                            }
                            if attributes.external {
                                flags |= FunParamFlags::IFC_EXTERNAL
                            }
                            if attributes.can_call {
                                flags |= FunParamFlags::IFC_CAN_CALL
                            }
                            if attributes.atom {
                                flags |= FunParamFlags::ATOM
                            }
                            match kind {
                                ParamMode::FPinout => {
                                    flags |= FunParamFlags::INOUT;
                                }
                                ParamMode::FPnormal => {}
                            };
                            if initializer.is_present() {
                                flags |= FunParamFlags::HAS_DEFAULT;
                            }
                            let variadic = initializer.is_ignored() && variadic;
                            let type_ = if variadic {
                                self.alloc(Ty(
                                    self.alloc(if name.is_some() {
                                        Reason::RvarParam(pos)
                                    } else {
                                        Reason::witness(pos)
                                    }),
                                    type_.1,
                                ))
                            } else {
                                type_
                            };
                            let rx_annotation = if attributes.at_most_rx_as_func {
                                Some(ParamRxAnnotation::ParamRxVar)
                            } else {
                                attributes
                                    .reactivity_condition_type
                                    .map(|ty| ParamRxAnnotation::ParamRxIfImpl(ty))
                            };
                            let param = self.alloc(FunParam {
                                pos,
                                name,
                                type_: self.alloc(PossiblyEnforcedTy {
                                    enforced: false,
                                    type_,
                                }),
                                flags,
                                rx_annotation,
                            });
                            arity = match arity {
                                FunArity::Fstandard if variadic => FunArity::Fvariadic(param),
                                arity => {
                                    params.push(param);
                                    arity
                                }
                            };
                        }
                        n => panic!("Expected a function parameter, but got {:?}", n),
                    }
                }
                Some((
                    params.into_bump_slice(),
                    properties.into_bump_slice(),
                    arity,
                ))
            }
            n if n.is_ignored() => Some((&[], &[], FunArity::Fstandard)),
            n => panic!("Expected a list of function parameters, but got {:?}", n),
        }
    }

    fn make_shape_field_name(&self, name: Node<'a>) -> Option<ShapeFieldName<'a>> {
        Some(match name {
            Node::StringLiteral(&(s, pos)) => ShapeFieldName::SFlitStr(self.alloc((pos, s))),
            // TODO: OCaml decl produces SFlitStr here instead of SFlitInt, so
            // we must also. Looks like int literal keys have become a parse
            // error--perhaps that's why.
            Node::IntLiteral(&(s, pos)) => ShapeFieldName::SFlitStr(self.alloc((pos, s.into()))),
            Node::Expr(aast::Expr(
                _,
                aast::Expr_::ClassConst(&(
                    aast::ClassId(_, aast::ClassId_::CI(&class_name)),
                    const_name,
                )),
            )) => ShapeFieldName::SFclassConst(self.alloc((class_name, const_name))),
            Node::Expr(aast::Expr(
                _,
                aast::Expr_::ClassConst(&(aast::ClassId(pos, aast::ClassId_::CIself), const_name)),
            )) => ShapeFieldName::SFclassConst(self.alloc((
                Id(
                    pos,
                    self.classish_name_builder.get_current_classish_name()?.0,
                ),
                const_name,
            ))),
            _ => return None,
        })
    }

    fn make_apply(
        &self,
        base_ty: Id<'a>,
        type_arguments: Node<'a>,
        pos_to_merge: &'a Pos<'a>,
    ) -> Node<'a> {
        let type_arguments = self.slice(
            type_arguments
                .iter()
                .filter_map(|&node| self.node_to_ty(node)),
        );

        let pos = self.merge(base_ty.0, pos_to_merge);

        // OCaml decl creates a capability with a hint pointing to the entire
        // type (i.e., pointing to `Rx<(function(): void)>` rather than just
        // `(function(): void)`), so we extend the hint position similarly here.
        let extend_capability_pos = |implicit_params: &'a FunImplicitParams| {
            let capability = match implicit_params.capability {
                CapTy(ty) => {
                    let ty = self.alloc(Ty(self.alloc(Reason::hint(pos)), ty.1));
                    CapTy(ty)
                }
                CapDefaults(_) => CapDefaults(pos),
            };
            self.alloc(FunImplicitParams {
                capability,
                ..*implicit_params
            })
        };

        let ty_ = match (base_ty, type_arguments) {
            (Id(_, name), &[&Ty(_, Ty_::Tfun(f))]) if name == "\\Pure" => {
                Ty_::Tfun(self.alloc(FunType {
                    reactive: Reactivity::Pure(None),
                    implicit_params: extend_capability_pos(f.implicit_params),
                    ..*f
                }))
            }
            (Id(_, name), &[&Ty(_, Ty_::Tfun(f))]) if name == "\\Rx" => {
                Ty_::Tfun(self.alloc(FunType {
                    reactive: Reactivity::Reactive(None),
                    implicit_params: extend_capability_pos(f.implicit_params),
                    ..*f
                }))
            }
            (Id(_, name), &[&Ty(_, Ty_::Tfun(f))]) if name == "\\RxShallow" => {
                Ty_::Tfun(self.alloc(FunType {
                    reactive: Reactivity::Shallow(None),
                    implicit_params: extend_capability_pos(f.implicit_params),
                    ..*f
                }))
            }
            (Id(_, name), &[&Ty(_, Ty_::Tfun(f))]) if name == "\\RxLocal" => {
                Ty_::Tfun(self.alloc(FunType {
                    reactive: Reactivity::Local(None),
                    implicit_params: extend_capability_pos(f.implicit_params),
                    ..*f
                }))
            }
            _ => Ty_::Tapply(self.alloc((base_ty, type_arguments))),
        };

        self.hint_ty(pos, ty_)
    }

    fn hint_ty(&self, pos: &'a Pos<'a>, ty_: Ty_<'a>) -> Node<'a> {
        Node::Ty(self.alloc(Ty(self.alloc(Reason::hint(pos)), ty_)))
    }

    fn prim_ty(&self, tprim: aast::Tprim, pos: &'a Pos<'a>) -> Node<'a> {
        self.hint_ty(pos, Ty_::Tprim(self.alloc(tprim)))
    }

    fn tany_with_pos(&self, pos: &'a Pos<'a>) -> &'a Ty<'a> {
        self.alloc(Ty(self.alloc(Reason::witness(pos)), TANY_))
    }

    /// The type used when a `varray_or_darray` typehint is missing its key type argument.
    fn varray_or_darray_key(&self, pos: &'a Pos<'a>) -> &'a Ty<'a> {
        self.alloc(Ty(
            self.alloc(Reason::RvarrayOrDarrayKey(pos)),
            Ty_::Tprim(self.alloc(aast::Tprim::Tarraykey)),
        ))
    }

    fn source_text_at_pos(&self, pos: &'a Pos<'a>) -> &'a [u8] {
        let start = pos.start_cnum();
        let end = pos.end_cnum();
        self.source_text.source_text().sub(start, end - start)
    }

    // While we usually can tell whether to allocate a Tapply or Tgeneric based
    // on our type_parameters stack, *constraints* on type parameters may
    // reference type parameters which we have not parsed yet. When constructing
    // a type parameter list, we use this function to rewrite the type of each
    // constraint, considering the full list of type parameters to be in scope.
    fn convert_tapply_to_tgeneric(&self, ty: &'a Ty<'a>) -> &'a Ty<'a> {
        let ty_ = match ty.1 {
            Ty_::Tapply(&(id, targs)) => {
                let converted_targs = self.slice(
                    targs
                        .iter()
                        .map(|&targ| self.convert_tapply_to_tgeneric(targ)),
                );
                match self.tapply_should_be_tgeneric(ty.0, id) {
                    Some(name) => Ty_::Tgeneric(self.alloc((name, converted_targs))),
                    None => Ty_::Tapply(self.alloc((id, converted_targs))),
                }
            }
            Ty_::Tlike(ty) => Ty_::Tlike(self.convert_tapply_to_tgeneric(ty)),
            Ty_::Toption(ty) => Ty_::Toption(self.convert_tapply_to_tgeneric(ty)),
            Ty_::Tfun(fun_type) => {
                let convert_param = |param: &'a FunParam<'a>| {
                    self.alloc(FunParam {
                        type_: self.alloc(PossiblyEnforcedTy {
                            enforced: param.type_.enforced,
                            type_: self.convert_tapply_to_tgeneric(param.type_.type_),
                        }),
                        ..*param
                    })
                };
                let arity = match fun_type.arity {
                    FunArity::Fstandard => FunArity::Fstandard,
                    FunArity::Fvariadic(param) => FunArity::Fvariadic(convert_param(param)),
                };
                let params = self.slice(fun_type.params.iter().copied().map(convert_param));
                let implicit_params = fun_type.implicit_params;
                let ret = self.alloc(PossiblyEnforcedTy {
                    enforced: fun_type.ret.enforced,
                    type_: self.convert_tapply_to_tgeneric(fun_type.ret.type_),
                });
                Ty_::Tfun(self.alloc(FunType {
                    arity,
                    params,
                    implicit_params,
                    ret,
                    ..*fun_type
                }))
            }
            Ty_::Tshape(&(kind, fields)) => {
                let mut converted_fields = AssocListMut::with_capacity_in(fields.len(), self.arena);
                for (&name, ty) in fields.iter() {
                    converted_fields.insert(
                        name,
                        self.alloc(ShapeFieldType {
                            optional: ty.optional,
                            ty: self.convert_tapply_to_tgeneric(ty.ty),
                        }),
                    );
                }
                Ty_::Tshape(self.alloc((kind, converted_fields.into())))
            }
            Ty_::Tdarray(&(tk, tv)) => Ty_::Tdarray(self.alloc((
                self.convert_tapply_to_tgeneric(tk),
                self.convert_tapply_to_tgeneric(tv),
            ))),
            Ty_::Tvarray(ty) => Ty_::Tvarray(self.convert_tapply_to_tgeneric(ty)),
            Ty_::TvarrayOrDarray(&(tk, tv)) => Ty_::TvarrayOrDarray(self.alloc((
                self.convert_tapply_to_tgeneric(tk),
                self.convert_tapply_to_tgeneric(tv),
            ))),
            Ty_::Ttuple(tys) => Ty_::Ttuple(
                self.slice(
                    tys.iter()
                        .map(|&targ| self.convert_tapply_to_tgeneric(targ)),
                ),
            ),
            _ => return ty,
        };
        self.alloc(Ty(ty.0, ty_))
    }

    // This is the logic for determining if convert_tapply_to_tgeneric should turn
    // a Tapply into a Tgeneric
    fn tapply_should_be_tgeneric(
        &self,
        reason: &'a Reason<'a>,
        id: nast::Sid<'a>,
    ) -> Option<&'a str> {
        match reason.pos() {
            // If the name contained a namespace delimiter in the original
            // source text, then it can't have referred to a type parameter
            // (since type parameters cannot be namespaced).
            Some(pos) => {
                if self.source_text_at_pos(pos).contains(&b'\\') {
                    return None;
                }
            }
            None => return None,
        }
        // However, the direct decl parser will unconditionally prefix
        // the name with the current namespace (as it does for any
        // Tapply). We need to remove it.
        match id.1.rsplit('\\').next() {
            Some(name) if self.is_type_param_in_scope(name) => return Some(name),
            _ => return None,
        }
    }

    fn rewrite_taccess_reasons(&self, ty: &'a Ty<'a>, r: &'a Reason<'a>) -> &'a Ty<'a> {
        let ty_ = match ty.1 {
            Ty_::Taccess(&TaccessType(ty, id)) => {
                Ty_::Taccess(self.alloc(TaccessType(self.rewrite_taccess_reasons(ty, r), id)))
            }
            ty_ => ty_,
        };
        self.alloc(Ty(r, ty_))
    }

    fn user_attribute_to_decl(
        &self,
        attr: &UserAttributeNode<'a>,
    ) -> &'a shallow_decl_defs::UserAttribute<'a> {
        self.alloc(shallow_decl_defs::UserAttribute {
            name: attr.name,
            classname_params: self.slice(attr.classname_params.iter().map(|p| p.name.1)),
        })
    }

    fn namespace_use_kind(use_kind: &Node) -> Option<TokenKind> {
        match use_kind.token_kind() {
            Some(TokenKind::Const) => None,
            Some(TokenKind::Function) => None,
            Some(TokenKind::Type) => Some(TokenKind::Type),
            Some(TokenKind::Namespace) => Some(TokenKind::Namespace),
            _ if !use_kind.is_present() => Some(TokenKind::Mixed),
            x => panic!("Unexpected namespace use kind: {:?}", x),
        }
    }
}

enum NodeIterHelper<'a: 'b, 'b> {
    Empty,
    Single(&'b Node<'a>),
    Vec(std::slice::Iter<'b, Node<'a>>),
}

impl<'a, 'b> Iterator for NodeIterHelper<'a, 'b> {
    type Item = &'b Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            NodeIterHelper::Empty => None,
            NodeIterHelper::Single(node) => {
                let node = *node;
                *self = NodeIterHelper::Empty;
                Some(node)
            }
            NodeIterHelper::Vec(ref mut iter) => iter.next(),
        }
    }

    // Must return the upper bound returned by Node::len.
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            NodeIterHelper::Empty => (0, Some(0)),
            NodeIterHelper::Single(_) => (1, Some(1)),
            NodeIterHelper::Vec(iter) => iter.size_hint(),
        }
    }
}

impl<'a, 'b> DoubleEndedIterator for NodeIterHelper<'a, 'b> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            NodeIterHelper::Empty => None,
            NodeIterHelper::Single(_) => self.next(),
            NodeIterHelper::Vec(ref mut iter) => iter.next_back(),
        }
    }
}

impl<'a> FlattenOp for DirectDeclSmartConstructors<'a> {
    type S = Node<'a>;

    fn flatten(&self, kind: SyntaxKind, lst: std::vec::Vec<Self::S>) -> Self::S {
        let size = lst
            .iter()
            .map(|s| match s {
                Node::List(children) => children.len(),
                x => {
                    if Self::is_zero(x) {
                        0
                    } else {
                        1
                    }
                }
            })
            .sum();
        let mut r = Vec::with_capacity_in(size, self.arena);
        for s in lst.into_iter() {
            match s {
                Node::List(children) => r.extend(children.iter().copied()),
                x => {
                    if !Self::is_zero(&x) {
                        r.push(x)
                    }
                }
            }
        }
        match r.into_bump_slice() {
            [] => Node::Ignored(kind),
            [node] => *node,
            slice => Node::List(self.alloc(slice)),
        }
    }

    fn zero(kind: SyntaxKind) -> Self::S {
        Node::Ignored(kind)
    }

    fn is_zero(s: &Self::S) -> bool {
        match s {
            Node::Token(token) => match token.kind() {
                TokenKind::Yield | TokenKind::Required | TokenKind::Lateinit => false,
                _ => true,
            },
            Node::List(inner) => inner.iter().all(Self::is_zero),
            _ => true,
        }
    }
}

impl<'a> FlattenSmartConstructors<'a, DirectDeclSmartConstructors<'a>>
    for DirectDeclSmartConstructors<'a>
{
    fn make_token(&mut self, token: CompactToken) -> Self::R {
        let token_text = |this: &Self| this.str_from_utf8(this.token_bytes(&token));
        let token_pos = |this: &Self| {
            let start = this
                .source_text
                .offset_to_file_pos_triple(token.start_offset());
            let end = this
                .source_text
                .offset_to_file_pos_triple(token.end_offset());
            Pos::from_lnum_bol_cnum(this.arena, this.filename, start, end)
        };
        let kind = token.kind();

        let result = match kind {
            TokenKind::Name | TokenKind::XHPClassName => {
                let text = token_text(self);
                let pos = token_pos(self);

                let name = if kind == TokenKind::XHPClassName {
                    Node::XhpName(self.alloc((text, pos)))
                } else {
                    Node::Name(self.alloc((text, pos)))
                };

                if self.previous_token_kind == TokenKind::Class
                    || self.previous_token_kind == TokenKind::Trait
                    || self.previous_token_kind == TokenKind::Interface
                {
                    if let Some(current_class_name) = self.elaborate_defined_id(name) {
                        self.classish_name_builder
                            .lexed_name_after_classish_keyword(
                                self.arena,
                                current_class_name.1,
                                pos,
                                self.previous_token_kind,
                            );
                    }
                }
                name
            }
            TokenKind::Class => Node::Name(self.alloc((token_text(self), token_pos(self)))),
            // There are a few types whose string representations we have to
            // grab anyway, so just go ahead and treat them as generic names.
            TokenKind::Variable
            | TokenKind::Vec
            | TokenKind::Dict
            | TokenKind::Keyset
            | TokenKind::Tuple
            | TokenKind::Classname
            | TokenKind::SelfToken => Node::Name(self.alloc((token_text(self), token_pos(self)))),
            TokenKind::XHPElementName => {
                Node::XhpName(self.alloc((token_text(self), token_pos(self))))
            }
            TokenKind::SingleQuotedStringLiteral => match escaper::unescape_single_in(
                self.str_from_utf8(escaper::unquote_slice(self.token_bytes(&token))),
                self.arena,
            ) {
                Ok(text) => Node::StringLiteral(self.alloc((text.into(), token_pos(self)))),
                Err(_) => Node::Ignored(SK::Token(kind)),
            },
            TokenKind::DoubleQuotedStringLiteral => match escaper::unescape_double_in(
                self.str_from_utf8(escaper::unquote_slice(self.token_bytes(&token))),
                self.arena,
            ) {
                Ok(text) => Node::StringLiteral(self.alloc((text.into(), token_pos(self)))),
                Err(_) => Node::Ignored(SK::Token(kind)),
            },
            TokenKind::HeredocStringLiteral => match escaper::unescape_heredoc_in(
                self.str_from_utf8(escaper::unquote_slice(self.token_bytes(&token))),
                self.arena,
            ) {
                Ok(text) => Node::StringLiteral(self.alloc((text.into(), token_pos(self)))),
                Err(_) => Node::Ignored(SK::Token(kind)),
            },
            TokenKind::NowdocStringLiteral => match escaper::unescape_nowdoc_in(
                self.str_from_utf8(escaper::unquote_slice(self.token_bytes(&token))),
                self.arena,
            ) {
                Ok(text) => Node::StringLiteral(self.alloc((text.into(), token_pos(self)))),
                Err(_) => Node::Ignored(SK::Token(kind)),
            },
            TokenKind::DecimalLiteral
            | TokenKind::OctalLiteral
            | TokenKind::HexadecimalLiteral
            | TokenKind::BinaryLiteral => {
                Node::IntLiteral(self.alloc((token_text(self), token_pos(self))))
            }
            TokenKind::FloatingLiteral => {
                Node::FloatingLiteral(self.alloc((token_text(self), token_pos(self))))
            }
            TokenKind::BooleanLiteral => {
                Node::BooleanLiteral(self.alloc((token_text(self), token_pos(self))))
            }
            TokenKind::String => self.prim_ty(aast::Tprim::Tstring, token_pos(self)),
            TokenKind::Int => self.prim_ty(aast::Tprim::Tint, token_pos(self)),
            TokenKind::Float => self.prim_ty(aast::Tprim::Tfloat, token_pos(self)),
            // "double" and "boolean" are parse errors--they should be written
            // "float" and "bool". The decl-parser treats the incorrect names as
            // type names rather than primitives.
            TokenKind::Double | TokenKind::Boolean => self.hint_ty(
                token_pos(self),
                Ty_::Tapply(self.alloc((Id(token_pos(self), token_text(self)), &[][..]))),
            ),
            TokenKind::Num => self.prim_ty(aast::Tprim::Tnum, token_pos(self)),
            TokenKind::Bool => self.prim_ty(aast::Tprim::Tbool, token_pos(self)),
            TokenKind::Mixed => {
                Node::Ty(self.alloc(Ty(self.alloc(Reason::hint(token_pos(self))), Ty_::Tmixed)))
            }
            TokenKind::Void => self.prim_ty(aast::Tprim::Tvoid, token_pos(self)),
            TokenKind::Arraykey => self.prim_ty(aast::Tprim::Tarraykey, token_pos(self)),
            TokenKind::Noreturn => self.prim_ty(aast::Tprim::Tnoreturn, token_pos(self)),
            TokenKind::Resource => self.prim_ty(aast::Tprim::Tresource, token_pos(self)),
            TokenKind::NullLiteral
            | TokenKind::Darray
            | TokenKind::Varray
            | TokenKind::Backslash
            | TokenKind::Construct
            | TokenKind::LeftParen
            | TokenKind::RightParen
            | TokenKind::LeftBracket
            | TokenKind::RightBracket
            | TokenKind::Shape
            | TokenKind::Question
            | TokenKind::This
            | TokenKind::Tilde
            | TokenKind::Exclamation
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::PlusPlus
            | TokenKind::MinusMinus
            | TokenKind::At
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::EqualEqual
            | TokenKind::EqualEqualEqual
            | TokenKind::StarStar
            | TokenKind::AmpersandAmpersand
            | TokenKind::BarBar
            | TokenKind::LessThan
            | TokenKind::LessThanEqual
            | TokenKind::GreaterThan
            | TokenKind::GreaterThanEqual
            | TokenKind::Dot
            | TokenKind::Ampersand
            | TokenKind::Bar
            | TokenKind::LessThanLessThan
            | TokenKind::GreaterThanGreaterThan
            | TokenKind::Percent
            | TokenKind::QuestionQuestion
            | TokenKind::Equal
            | TokenKind::Abstract
            | TokenKind::As
            | TokenKind::Super
            | TokenKind::Async
            | TokenKind::DotDotDot
            | TokenKind::Extends
            | TokenKind::Final
            | TokenKind::Implements
            | TokenKind::Inout
            | TokenKind::Interface
            | TokenKind::Newtype
            | TokenKind::Type
            | TokenKind::Yield
            | TokenKind::Semicolon
            | TokenKind::Private
            | TokenKind::Protected
            | TokenKind::Public
            | TokenKind::Reify
            | TokenKind::Static
            | TokenKind::Trait
            | TokenKind::Lateinit
            | TokenKind::RecordDec
            | TokenKind::RightBrace
            | TokenKind::Enum
            | TokenKind::Const
            | TokenKind::Function
            | TokenKind::Namespace
            | TokenKind::XHP
            | TokenKind::Required
            | TokenKind::Ctx => Node::Token(FixedWidthToken::new(kind, token.start_offset())),
            TokenKind::EndOfFile
            | TokenKind::Attribute
            | TokenKind::Await
            | TokenKind::Binary
            | TokenKind::Break
            | TokenKind::Case
            | TokenKind::Catch
            | TokenKind::Category
            | TokenKind::Children
            | TokenKind::Clone
            | TokenKind::Continue
            | TokenKind::Default
            | TokenKind::Define
            | TokenKind::Do
            | TokenKind::Echo
            | TokenKind::Else
            | TokenKind::Elseif
            | TokenKind::Empty
            | TokenKind::Endfor
            | TokenKind::Endforeach
            | TokenKind::Endif
            | TokenKind::Endswitch
            | TokenKind::Endwhile
            | TokenKind::Eval
            | TokenKind::Fallthrough
            | TokenKind::File
            | TokenKind::Finally
            | TokenKind::For
            | TokenKind::Foreach
            | TokenKind::From
            | TokenKind::Global
            | TokenKind::Concurrent
            | TokenKind::If
            | TokenKind::Include
            | TokenKind::Include_once
            | TokenKind::Instanceof
            | TokenKind::Insteadof
            | TokenKind::Integer
            | TokenKind::Is
            | TokenKind::Isset
            | TokenKind::List
            | TokenKind::New
            | TokenKind::Object
            | TokenKind::Parent
            | TokenKind::Print
            | TokenKind::Real
            | TokenKind::Record
            | TokenKind::Require
            | TokenKind::Require_once
            | TokenKind::Return
            | TokenKind::Switch
            | TokenKind::Throw
            | TokenKind::Try
            | TokenKind::Unset
            | TokenKind::Use
            | TokenKind::Using
            | TokenKind::Var
            | TokenKind::Where
            | TokenKind::While
            | TokenKind::LeftBrace
            | TokenKind::MinusGreaterThan
            | TokenKind::Dollar
            | TokenKind::LessThanEqualGreaterThan
            | TokenKind::ExclamationEqual
            | TokenKind::ExclamationEqualEqual
            | TokenKind::Carat
            | TokenKind::QuestionAs
            | TokenKind::QuestionColon
            | TokenKind::QuestionQuestionEqual
            | TokenKind::Colon
            | TokenKind::StarStarEqual
            | TokenKind::StarEqual
            | TokenKind::SlashEqual
            | TokenKind::PercentEqual
            | TokenKind::PlusEqual
            | TokenKind::MinusEqual
            | TokenKind::DotEqual
            | TokenKind::LessThanLessThanEqual
            | TokenKind::GreaterThanGreaterThanEqual
            | TokenKind::AmpersandEqual
            | TokenKind::CaratEqual
            | TokenKind::BarEqual
            | TokenKind::Comma
            | TokenKind::ColonColon
            | TokenKind::EqualGreaterThan
            | TokenKind::EqualEqualGreaterThan
            | TokenKind::QuestionMinusGreaterThan
            | TokenKind::DollarDollar
            | TokenKind::BarGreaterThan
            | TokenKind::SlashGreaterThan
            | TokenKind::LessThanSlash
            | TokenKind::LessThanQuestion
            | TokenKind::Backtick
            | TokenKind::ErrorToken
            | TokenKind::DoubleQuotedStringLiteralHead
            | TokenKind::StringLiteralBody
            | TokenKind::DoubleQuotedStringLiteralTail
            | TokenKind::HeredocStringLiteralHead
            | TokenKind::HeredocStringLiteralTail
            | TokenKind::XHPCategoryName
            | TokenKind::XHPStringLiteral
            | TokenKind::XHPBody
            | TokenKind::XHPComment
            | TokenKind::Hash
            | TokenKind::Hashbang => Node::Ignored(SK::Token(kind)),
        };
        self.previous_token_kind = kind;
        result
    }

    fn make_missing(&mut self, _: usize) -> Self::R {
        Node::Ignored(SK::Missing)
    }

    fn make_list(&mut self, items: std::vec::Vec<Self::R>, _: usize) -> Self::R {
        if let Some(&yield_) = items
            .iter()
            .flat_map(|node| node.iter())
            .find(|node| node.is_token(TokenKind::Yield))
        {
            yield_
        } else {
            let size = items.iter().filter(|node| node.is_present()).count();
            let items_iter = items.into_iter();
            let mut items = Vec::with_capacity_in(size, self.arena);
            for node in items_iter {
                if node.is_present() {
                    items.push(node);
                }
            }
            let items = items.into_bump_slice();
            if items.is_empty() {
                Node::Ignored(SK::SyntaxList)
            } else {
                Node::List(self.alloc(items))
            }
        }
    }

    fn make_qualified_name(&mut self, parts: Self::R) -> Self::R {
        let pos = self.get_pos(parts);
        match parts {
            Node::List(nodes) => Node::QualifiedName(self.alloc((nodes, pos))),
            node if node.is_ignored() => Node::Ignored(SK::QualifiedName),
            node => Node::QualifiedName(
                self.alloc((bumpalo::vec![in self.arena; node].into_bump_slice(), pos)),
            ),
        }
    }

    fn make_simple_type_specifier(&mut self, specifier: Self::R) -> Self::R {
        // Return this explicitly because flatten filters out zero nodes, and
        // we treat most non-error nodes as zeroes.
        specifier
    }

    fn make_literal_expression(&mut self, expression: Self::R) -> Self::R {
        expression
    }

    fn make_simple_initializer(&mut self, equals: Self::R, expr: Self::R) -> Self::R {
        // If the expr is Ignored, bubble up the assignment operator so that we
        // can tell that *some* initializer was here. Useful for class
        // properties, where we need to enforce that properties without default
        // values are initialized in the constructor.
        if expr.is_ignored() { equals } else { expr }
    }

    fn make_anonymous_function(
        &mut self,
        _attribute_spec: Self::R,
        _static_keyword: Self::R,
        _async_keyword: Self::R,
        _function_keyword: Self::R,
        _left_paren: Self::R,
        _parameters: Self::R,
        _right_paren: Self::R,
        _ctx_list: Self::R,
        _colon: Self::R,
        _type_: Self::R,
        _use_: Self::R,
        _body: Self::R,
    ) -> Self::R {
        // do not allow Yield to bubble up
        Node::Ignored(SK::AnonymousFunction)
    }

    fn make_lambda_expression(
        &mut self,
        _attribute_spec: Self::R,
        _async_: Self::R,
        _signature: Self::R,
        _arrow: Self::R,
        _body: Self::R,
    ) -> Self::R {
        // do not allow Yield to bubble up
        Node::Ignored(SK::LambdaExpression)
    }

    fn make_awaitable_creation_expression(
        &mut self,
        _attribute_spec: Self::R,
        _async_: Self::R,
        _compound_statement: Self::R,
    ) -> Self::R {
        // do not allow Yield to bubble up
        Node::Ignored(SK::AwaitableCreationExpression)
    }

    fn make_darray_intrinsic_expression(
        &mut self,
        darray: Self::R,
        _explicit_type: Self::R,
        _left_bracket: Self::R,
        fields: Self::R,
        right_bracket: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|node| match node {
            Node::ListItem(&(key, value)) => {
                let key = self.node_to_expr(key)?;
                let value = self.node_to_expr(value)?;
                Some((key, value))
            }
            n => panic!("Expected a ListItem but was {:?}", n),
        }));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(darray, right_bracket),
            nast::Expr_::Darray(self.alloc((None, fields))),
        )))
    }

    fn make_dictionary_intrinsic_expression(
        &mut self,
        dict: Self::R,
        _explicit_type: Self::R,
        _left_bracket: Self::R,
        fields: Self::R,
        right_bracket: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|node| match node {
            Node::ListItem(&(key, value)) => {
                let key = self.node_to_expr(key)?;
                let value = self.node_to_expr(value)?;
                Some(self.alloc(aast::Field(key, value)))
            }
            n => panic!("Expected a ListItem but was {:?}", n),
        }));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(dict, right_bracket),
            nast::Expr_::KeyValCollection(self.alloc((aast_defs::KvcKind::Dict, None, fields))),
        )))
    }

    fn make_keyset_intrinsic_expression(
        &mut self,
        keyset: Self::R,
        _explicit_type: Self::R,
        _left_bracket: Self::R,
        fields: Self::R,
        right_bracket: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|&node| self.node_to_expr(node)));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(keyset, right_bracket),
            nast::Expr_::ValCollection(self.alloc((aast_defs::VcKind::Keyset, None, fields))),
        )))
    }

    fn make_varray_intrinsic_expression(
        &mut self,
        varray: Self::R,
        _explicit_type: Self::R,
        _left_bracket: Self::R,
        fields: Self::R,
        right_bracket: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|&node| self.node_to_expr(node)));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(varray, right_bracket),
            nast::Expr_::Varray(self.alloc((None, fields))),
        )))
    }

    fn make_vector_intrinsic_expression(
        &mut self,
        vec: Self::R,
        _explicit_type: Self::R,
        _left_bracket: Self::R,
        fields: Self::R,
        right_bracket: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|&node| self.node_to_expr(node)));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(vec, right_bracket),
            nast::Expr_::ValCollection(self.alloc((aast_defs::VcKind::Vec, None, fields))),
        )))
    }

    fn make_element_initializer(
        &mut self,
        key: Self::R,
        _arrow: Self::R,
        value: Self::R,
    ) -> Self::R {
        Node::ListItem(self.alloc((key, value)))
    }

    fn make_prefix_unary_expression(&mut self, op: Self::R, value: Self::R) -> Self::R {
        let pos = self.merge_positions(op, value);
        let op = match op.token_kind() {
            Some(TokenKind::Tilde) => Uop::Utild,
            Some(TokenKind::Exclamation) => Uop::Unot,
            Some(TokenKind::Plus) => Uop::Uplus,
            Some(TokenKind::Minus) => Uop::Uminus,
            Some(TokenKind::PlusPlus) => Uop::Uincr,
            Some(TokenKind::MinusMinus) => Uop::Udecr,
            Some(TokenKind::At) => Uop::Usilence,
            _ => return Node::Ignored(SK::PrefixUnaryExpression),
        };
        let value = match self.node_to_expr(value) {
            Some(value) => value,
            None => return Node::Ignored(SK::PrefixUnaryExpression),
        };
        Node::Expr(self.alloc(aast::Expr(pos, aast::Expr_::Unop(self.alloc((op, value))))))
    }

    fn make_postfix_unary_expression(&mut self, value: Self::R, op: Self::R) -> Self::R {
        let pos = self.merge_positions(value, op);
        let op = match op.token_kind() {
            Some(TokenKind::PlusPlus) => Uop::Upincr,
            Some(TokenKind::MinusMinus) => Uop::Updecr,
            _ => return Node::Ignored(SK::PostfixUnaryExpression),
        };
        let value = match self.node_to_expr(value) {
            Some(value) => value,
            None => return Node::Ignored(SK::PostfixUnaryExpression),
        };
        Node::Expr(self.alloc(aast::Expr(pos, aast::Expr_::Unop(self.alloc((op, value))))))
    }

    fn make_binary_expression(&mut self, lhs: Self::R, op_node: Self::R, rhs: Self::R) -> Self::R {
        let op = match op_node.token_kind() {
            Some(TokenKind::Plus) => Bop::Plus,
            Some(TokenKind::Minus) => Bop::Minus,
            Some(TokenKind::Star) => Bop::Star,
            Some(TokenKind::Slash) => Bop::Slash,
            Some(TokenKind::Equal) => Bop::Eq(None),
            Some(TokenKind::EqualEqual) => Bop::Eqeq,
            Some(TokenKind::EqualEqualEqual) => Bop::Eqeqeq,
            Some(TokenKind::StarStar) => Bop::Starstar,
            Some(TokenKind::AmpersandAmpersand) => Bop::Ampamp,
            Some(TokenKind::BarBar) => Bop::Barbar,
            Some(TokenKind::LessThan) => Bop::Lt,
            Some(TokenKind::LessThanEqual) => Bop::Lte,
            Some(TokenKind::LessThanLessThan) => Bop::Ltlt,
            Some(TokenKind::GreaterThan) => Bop::Gt,
            Some(TokenKind::GreaterThanEqual) => Bop::Gte,
            Some(TokenKind::GreaterThanGreaterThan) => Bop::Gtgt,
            Some(TokenKind::Dot) => Bop::Dot,
            Some(TokenKind::Ampersand) => Bop::Amp,
            Some(TokenKind::Bar) => Bop::Bar,
            Some(TokenKind::Percent) => Bop::Percent,
            Some(TokenKind::QuestionQuestion) => Bop::QuestionQuestion,
            _ => return Node::Ignored(SK::BinaryExpression),
        };

        match (&op, rhs.is_token(TokenKind::Yield)) {
            (Bop::Eq(_), true) => return rhs,
            _ => {}
        }

        let pos = self.merge(self.merge_positions(lhs, op_node), self.get_pos(rhs));

        let lhs = match self.node_to_expr(lhs) {
            Some(lhs) => lhs,
            None => return Node::Ignored(SK::BinaryExpression),
        };
        let rhs = match self.node_to_expr(rhs) {
            Some(rhs) => rhs,
            None => return Node::Ignored(SK::BinaryExpression),
        };

        Node::Expr(self.alloc(aast::Expr(
            pos,
            aast::Expr_::Binop(self.alloc((op, lhs, rhs))),
        )))
    }

    fn make_parenthesized_expression(
        &mut self,
        _lparen: Self::R,
        expr: Self::R,
        _rparen: Self::R,
    ) -> Self::R {
        expr
    }

    fn make_list_item(&mut self, item: Self::R, sep: Self::R) -> Self::R {
        match (item.is_ignored(), sep.is_ignored()) {
            (true, true) => Node::Ignored(SK::ListItem),
            (false, true) => item,
            (true, false) => sep,
            (false, false) => Node::ListItem(self.alloc((item, sep))),
        }
    }

    fn make_type_arguments(
        &mut self,
        less_than: Self::R,
        arguments: Self::R,
        greater_than: Self::R,
    ) -> Self::R {
        Node::BracketedList(self.alloc((
            self.get_pos(less_than),
            arguments.as_slice(self.arena),
            self.get_pos(greater_than),
        )))
    }

    fn make_generic_type_specifier(
        &mut self,
        class_type: Self::R,
        type_arguments: Self::R,
    ) -> Self::R {
        let class_id = match self.expect_name(class_type) {
            Some(id) => id,
            None => return Node::Ignored(SK::GenericTypeSpecifier),
        };
        if class_id.1.trim_start_matches("\\") == "varray_or_darray" {
            let id_pos = class_id.0;
            let pos = self.merge(id_pos, self.get_pos(type_arguments));
            let type_arguments = type_arguments.as_slice(self.arena);
            let ty_ = match type_arguments {
                [tk, tv] => Ty_::TvarrayOrDarray(
                    self.alloc((
                        self.node_to_ty(*tk)
                            .unwrap_or_else(|| self.tany_with_pos(id_pos)),
                        self.node_to_ty(*tv)
                            .unwrap_or_else(|| self.tany_with_pos(id_pos)),
                    )),
                ),
                [tv] => Ty_::TvarrayOrDarray(
                    self.alloc((
                        self.varray_or_darray_key(pos),
                        self.node_to_ty(*tv)
                            .unwrap_or_else(|| self.tany_with_pos(id_pos)),
                    )),
                ),
                _ => TANY_,
            };
            self.hint_ty(pos, ty_)
        } else {
            let Id(pos, class_type) = class_id;
            match class_type.rsplit('\\').next() {
                Some(name) if self.is_type_param_in_scope(name) => {
                    let pos = self.merge(pos, self.get_pos(type_arguments));
                    let type_arguments = self.slice(
                        type_arguments
                            .iter()
                            .filter_map(|&node| self.node_to_ty(node)),
                    );
                    let ty_ = Ty_::Tgeneric(self.alloc((name, type_arguments)));
                    self.hint_ty(pos, ty_)
                }
                _ => {
                    let class_type = self.elaborate_raw_id(class_type);
                    self.make_apply(
                        Id(pos, class_type),
                        type_arguments,
                        self.get_pos(type_arguments),
                    )
                }
            }
        }
    }

    fn make_record_declaration(
        &mut self,
        attribute_spec: Self::R,
        modifier: Self::R,
        record_keyword: Self::R,
        name: Self::R,
        _extends_keyword: Self::R,
        extends_opt: Self::R,
        _left_brace: Self::R,
        fields: Self::R,
        right_brace: Self::R,
    ) -> Self::R {
        let name = match self.elaborate_defined_id(name) {
            Some(name) => name,
            None => return Node::Ignored(SK::RecordDeclaration),
        };
        self.add_record(
            name.1,
            self.alloc(typing_defs::RecordDefType {
                name,
                extends: self
                    .expect_name(extends_opt)
                    .map(|id| self.elaborate_id(id)),
                fields: self.slice(fields.iter().filter_map(|node| match node {
                    Node::RecordField(&field) => Some(field),
                    _ => None,
                })),
                abstract_: modifier.is_token(TokenKind::Abstract),
                pos: self.pos_from_slice(&[attribute_spec, modifier, record_keyword, right_brace]),
            }),
        );
        Node::Ignored(SK::RecordDeclaration)
    }

    fn make_record_field(
        &mut self,
        _type_: Self::R,
        name: Self::R,
        initializer: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        let name = match self.expect_name(name) {
            Some(name) => name,
            None => return Node::Ignored(SK::RecordField),
        };
        let field_req = if initializer.is_ignored() {
            RecordFieldReq::ValueRequired
        } else {
            RecordFieldReq::HasDefaultValue
        };
        Node::RecordField(self.alloc((name, field_req)))
    }

    fn make_alias_declaration(
        &mut self,
        _attributes: Self::R,
        keyword: Self::R,
        name: Self::R,
        generic_params: Self::R,
        constraint: Self::R,
        _equal: Self::R,
        aliased_type: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        if name.is_ignored() {
            return Node::Ignored(SK::AliasDeclaration);
        }
        let Id(pos, name) = match self.elaborate_defined_id(name) {
            Some(id) => id,
            None => return Node::Ignored(SK::AliasDeclaration),
        };
        let ty = match self.node_to_ty(aliased_type) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::AliasDeclaration),
        };
        let constraint = match constraint {
            Node::TypeConstraint(&(_kind, hint)) => self.node_to_ty(hint),
            _ => None,
        };
        // Pop the type params stack only after creating all inner types.
        let tparams = self.pop_type_params(generic_params);
        let typedef = self.alloc(TypedefType {
            pos,
            vis: match keyword.token_kind() {
                Some(TokenKind::Type) => aast::TypedefVisibility::Transparent,
                Some(TokenKind::Newtype) => aast::TypedefVisibility::Opaque,
                _ => aast::TypedefVisibility::Transparent,
            },
            tparams,
            constraint,
            type_: ty,
        });

        self.add_typedef(name, typedef);

        Node::Ignored(SK::AliasDeclaration)
    }

    fn make_type_constraint(&mut self, kind: Self::R, value: Self::R) -> Self::R {
        let kind = match kind.token_kind() {
            Some(TokenKind::As) => ConstraintKind::ConstraintAs,
            Some(TokenKind::Super) => ConstraintKind::ConstraintSuper,
            n => panic!("Expected either As or Super, but was {:?}", n),
        };
        Node::TypeConstraint(self.alloc((kind, value)))
    }

    fn make_type_parameter(
        &mut self,
        user_attributes: Self::R,
        reify: Self::R,
        variance: Self::R,
        name: Self::R,
        tparam_params: Self::R,
        constraints: Self::R,
    ) -> Self::R {
        let user_attributes = match user_attributes {
            Node::BracketedList((_, attributes, _)) => {
                self.slice(attributes.into_iter().filter_map(|x| match x {
                    Node::Attribute(a) => Some(*a),
                    _ => None,
                }))
            }
            _ => &[][..],
        };

        let constraints = self.slice(constraints.iter().filter_map(|node| match node {
            Node::TypeConstraint(&constraint) => Some(constraint),
            n if n.is_ignored() => None,
            n => panic!("Expected a type constraint, but was {:?}", n),
        }));

        // TODO(T70068435) Once we add support for constraints on higher-kinded types
        // (in particular, constraints on nested type parameters), we need to ensure
        // that we correctly handle the scoping of nested type parameters.
        // This includes making sure that the call to convert_type_appl_to_generic
        // in make_type_parameters handles nested constraints.
        // For now, we just make sure that the nested type parameters that make_type_parameters
        // added to the global list of in-scope type parameters are removed immediately:
        self.pop_type_params(tparam_params);

        let tparam_params = match tparam_params {
            Node::TypeParameters(&params) => params,
            _ => &[],
        };

        Node::TypeParameter(self.alloc(TypeParameterDecl {
            name,
            variance: match variance.token_kind() {
                Some(TokenKind::Minus) => Variance::Contravariant,
                Some(TokenKind::Plus) => Variance::Covariant,
                _ => Variance::Invariant,
            },
            reified: if reify.is_token(TokenKind::Reify) {
                if user_attributes.iter().any(|node| node.name.1 == "__Soft") {
                    aast::ReifyKind::SoftReified
                } else {
                    aast::ReifyKind::Reified
                }
            } else {
                aast::ReifyKind::Erased
            },
            constraints,
            tparam_params,
            user_attributes,
        }))
    }

    fn make_type_parameters(&mut self, _lt: Self::R, tparams: Self::R, _gt: Self::R) -> Self::R {
        let size = tparams.len();
        let mut tparams_with_name = Vec::with_capacity_in(size, self.arena);
        let mut tparam_names = MultiSetMut::with_capacity_in(size, self.arena);
        for node in tparams.iter() {
            match node {
                &Node::TypeParameter(decl) => {
                    let name = match decl.name.as_id() {
                        Some(name) => name,
                        None => return Node::Ignored(SK::TypeParameters),
                    };
                    tparam_names.insert(name.1);
                    tparams_with_name.push((decl, name));
                }
                n => panic!("Expected a type parameter, but got {:?}", n),
            }
        }
        Rc::make_mut(&mut self.type_parameters).push(tparam_names.into());
        let mut tparams = Vec::with_capacity_in(tparams_with_name.len(), self.arena);
        for (decl, name) in tparams_with_name.into_iter() {
            let &TypeParameterDecl {
                name: _,
                variance,
                reified,
                constraints,
                tparam_params,
                user_attributes,
            } = decl;
            let constraints = self.slice(constraints.iter().filter_map(|constraint| {
                let &(kind, ty) = constraint;
                let ty = self.node_to_ty(ty)?;
                let ty = self.convert_tapply_to_tgeneric(ty);
                Some((kind, ty))
            }));

            let user_attributes = self.slice(
                user_attributes
                    .iter()
                    .rev()
                    .map(|x| self.user_attribute_to_decl(x)),
            );
            tparams.push(self.alloc(Tparam {
                variance,
                name,
                constraints,
                reified,
                user_attributes,
                tparams: tparam_params,
            }));
        }
        Node::TypeParameters(self.alloc(tparams.into_bump_slice()))
    }

    fn make_parameter_declaration(
        &mut self,
        attributes: Self::R,
        visibility: Self::R,
        inout: Self::R,
        hint: Self::R,
        name: Self::R,
        initializer: Self::R,
    ) -> Self::R {
        let (variadic, pos, name) = match name {
            Node::ListItem(&(ellipsis, id)) => {
                let Id(pos, name) = match id.as_id() {
                    Some(id) => id,
                    None => return Node::Ignored(SK::ParameterDeclaration),
                };
                let variadic = ellipsis.is_token(TokenKind::DotDotDot);
                (variadic, pos, Some(name))
            }
            name => {
                let Id(pos, name) = match name.as_id() {
                    Some(id) => id,
                    None => return Node::Ignored(SK::ParameterDeclaration),
                };
                (false, pos, Some(name))
            }
        };
        let kind = if inout.is_token(TokenKind::Inout) {
            ParamMode::FPinout
        } else {
            ParamMode::FPnormal
        };
        Node::FunParam(self.alloc(FunParamDecl {
            attributes,
            visibility,
            kind,
            hint,
            pos,
            name,
            variadic,
            initializer,
        }))
    }

    fn make_variadic_parameter(&mut self, _: Self::R, hint: Self::R, ellipsis: Self::R) -> Self::R {
        Node::FunParam(
            self.alloc(FunParamDecl {
                attributes: Node::Ignored(SK::Missing),
                visibility: Node::Ignored(SK::Missing),
                kind: ParamMode::FPnormal,
                hint,
                pos: self
                    .get_pos_opt(hint)
                    .unwrap_or_else(|| self.get_pos(ellipsis)),
                name: None,
                variadic: true,
                initializer: Node::Ignored(SK::Missing),
            }),
        )
    }

    fn make_function_declaration(
        &mut self,
        attributes: Self::R,
        header: Self::R,
        body: Self::R,
    ) -> Self::R {
        let parsed_attributes = self.to_attributes(attributes);
        match header {
            Node::FunctionHeader(header) => {
                let is_method = false;
                let (Id(pos, name), type_, _) =
                    match self.function_to_ty(is_method, attributes, header, body) {
                        Some(x) => x,
                        None => return Node::Ignored(SK::FunctionDeclaration),
                    };
                let deprecated = parsed_attributes.deprecated.map(|msg| {
                    let mut s = String::new_in(self.arena);
                    s.push_str("The function ");
                    s.push_str(name.trim_start_matches("\\"));
                    s.push_str(" is deprecated: ");
                    s.push_str(msg);
                    s.into_bump_str()
                });
                let fun_elt = self.alloc(FunElt {
                    deprecated,
                    type_,
                    pos,
                    php_std_lib: parsed_attributes.php_std_lib,
                });
                self.add_fun(name, fun_elt);
                Node::Ignored(SK::FunctionDeclaration)
            }
            _ => Node::Ignored(SK::FunctionDeclaration),
        }
    }

    fn make_contexts(&mut self, lb: Self::R, tys: Self::R, rb: Self::R) -> Self::R {
        let mut namespace_builder = NamespaceBuilder::empty_with_ns_in("HH\\Contexts", self.arena);
        std::mem::swap(
            &mut namespace_builder,
            Rc::make_mut(&mut self.namespace_builder),
        );
        // Simulating Typing_make_type.intersection here
        let make_mixed = || {
            let pos = Reason::hint(self.merge_positions(lb, rb));
            Node::Ty(self.alloc(Ty(
                self.alloc(pos),
                Ty_::Toption(self.alloc(Ty(self.alloc(pos), Ty_::Tnonnull))),
            )))
        };
        let cap = match tys {
            Node::Ignored(_) => make_mixed(),
            Node::List(tys_list) => {
                if tys_list.is_empty() {
                    make_mixed()
                } else if tys_list.len() == 1 {
                    Node::Ty(self.node_to_ty(tys_list[0]).unwrap())
                } else {
                    self.make_intersection_type_specifier(lb, tys, rb)
                }
            }
            _ => self.make_intersection_type_specifier(lb, tys, rb),
        };
        std::mem::swap(
            &mut namespace_builder,
            Rc::make_mut(&mut self.namespace_builder),
        );
        cap
    }

    fn make_function_declaration_header(
        &mut self,
        modifiers: Self::R,
        _keyword: Self::R,
        name: Self::R,
        type_params: Self::R,
        left_paren: Self::R,
        param_list: Self::R,
        _right_paren: Self::R,
        capability: Self::R,
        _colon: Self::R,
        ret_hint: Self::R,
        where_constraints: Self::R,
    ) -> Self::R {
        // Use the position of the left paren if the name is missing.
        let name = if name.is_ignored() { left_paren } else { name };
        Node::FunctionHeader(self.alloc(FunctionHeader {
            name,
            modifiers,
            type_params,
            param_list,
            capability,
            ret_hint,
            where_constraints,
        }))
    }

    fn make_yield_expression(&mut self, keyword: Self::R, _operand: Self::R) -> Self::R {
        assert!(keyword.token_kind() == Some(TokenKind::Yield));
        keyword
    }

    fn make_const_declaration(
        &mut self,
        modifiers: Self::R,
        _const_keyword: Self::R,
        hint: Self::R,
        decls: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        match decls {
            // Class consts.
            Node::List(consts)
                if self
                    .classish_name_builder
                    .get_current_classish_name()
                    .is_some() =>
            {
                let ty = self.node_to_ty(hint);
                Node::List(
                    self.alloc(self.slice(consts.iter().filter_map(|cst| match cst {
                        Node::ConstInitializer(&(name, initializer)) => {
                            let id = name.as_id()?;
                            let ty = ty
                                .or_else(|| self.infer_const(name, initializer))
                                .unwrap_or_else(|| tany());
                            let modifiers = read_member_modifiers(modifiers.iter());
                            Some(Node::Const(self.alloc(
                                shallow_decl_defs::ShallowClassConst {
                                    abstract_: modifiers.is_abstract,
                                    name: id,
                                    type_: ty,
                                },
                            )))
                        }
                        _ => None,
                    }))),
                )
            }
            // Global consts.
            Node::List([Node::ConstInitializer(&(name, initializer))]) => {
                let Id(pos, id) = match self.elaborate_defined_id(name) {
                    Some(id) => id,
                    None => return Node::Ignored(SK::ConstDeclaration),
                };
                let ty = self
                    .node_to_ty(hint)
                    .or_else(|| self.infer_const(name, initializer))
                    .unwrap_or_else(|| self.tany_with_pos(pos));
                self.add_const(id, self.alloc(ConstDecl { pos, type_: ty }));
                Node::Ignored(SK::ConstDeclaration)
            }
            _ => Node::Ignored(SK::ConstDeclaration),
        }
    }

    fn make_constant_declarator(&mut self, name: Self::R, initializer: Self::R) -> Self::R {
        if name.is_ignored() {
            Node::Ignored(SK::ConstantDeclarator)
        } else {
            Node::ConstInitializer(self.alloc((name, initializer)))
        }
    }

    fn make_namespace_declaration(&mut self, _name: Self::R, body: Self::R) -> Self::R {
        if let Node::Ignored(SK::NamespaceBody) = body {
            Rc::make_mut(&mut self.namespace_builder).pop_namespace();
        }
        Node::Ignored(SK::NamespaceDeclaration)
    }

    fn make_namespace_declaration_header(&mut self, _keyword: Self::R, name: Self::R) -> Self::R {
        let name = self.expect_name(name).map(|Id(_, name)| name);
        // if this is header of semicolon-style (one with NamespaceEmptyBody) namespace, we should pop
        // the previous namespace first, but we don't have the body yet. We'll fix it retroactively in
        // make_namespace_empty_body
        Rc::make_mut(&mut self.namespace_builder).push_namespace(name);
        Node::Ignored(SK::NamespaceDeclarationHeader)
    }

    fn make_namespace_body(
        &mut self,
        _left_brace: Self::R,
        _declarations: Self::R,
        _right_brace: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::NamespaceBody)
    }

    fn make_namespace_empty_body(&mut self, _semicolon: Self::R) -> Self::R {
        Rc::make_mut(&mut self.namespace_builder).pop_previous_namespace();
        Node::Ignored(SK::NamespaceEmptyBody)
    }

    fn make_namespace_use_declaration(
        &mut self,
        _keyword: Self::R,
        namespace_use_kind: Self::R,
        clauses: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        if let Some(import_kind) = Self::namespace_use_kind(&namespace_use_kind) {
            for clause in clauses.iter() {
                if let Node::NamespaceUseClause(nuc) = clause {
                    Rc::make_mut(&mut self.namespace_builder).add_import(
                        import_kind,
                        nuc.id.1,
                        nuc.as_,
                    );
                }
            }
        }
        Node::Ignored(SK::NamespaceUseDeclaration)
    }

    fn make_namespace_group_use_declaration(
        &mut self,
        _keyword: Self::R,
        _kind: Self::R,
        prefix: Self::R,
        _left_brace: Self::R,
        clauses: Self::R,
        _right_brace: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        let Id(_, prefix) = match self.expect_name(prefix) {
            Some(id) => id,
            None => return Node::Ignored(SK::NamespaceGroupUseDeclaration),
        };
        for clause in clauses.iter() {
            if let Node::NamespaceUseClause(nuc) = clause {
                let mut id = String::new_in(self.arena);
                id.push_str(prefix);
                id.push_str(nuc.id.1);
                Rc::make_mut(&mut self.namespace_builder).add_import(
                    nuc.kind,
                    id.into_bump_str(),
                    nuc.as_,
                );
            }
        }
        Node::Ignored(SK::NamespaceGroupUseDeclaration)
    }

    fn make_namespace_use_clause(
        &mut self,
        clause_kind: Self::R,
        name: Self::R,
        as_: Self::R,
        aliased_name: Self::R,
    ) -> Self::R {
        let id = match self.expect_name(name) {
            Some(id) => id,
            None => return Node::Ignored(SK::NamespaceUseClause),
        };
        let as_ = if as_.is_token(TokenKind::As) {
            match aliased_name.as_id() {
                Some(name) => Some(name.1),
                None => return Node::Ignored(SK::NamespaceUseClause),
            }
        } else {
            None
        };
        if let Some(kind) = Self::namespace_use_kind(&clause_kind) {
            Node::NamespaceUseClause(self.alloc(NamespaceUseClause { kind, id, as_ }))
        } else {
            Node::Ignored(SK::NamespaceUseClause)
        }
    }

    fn make_where_clause(&mut self, _: Self::R, where_constraints: Self::R) -> Self::R {
        where_constraints
    }

    fn make_where_constraint(
        &mut self,
        left_type: Self::R,
        operator: Self::R,
        right_type: Self::R,
    ) -> Self::R {
        Node::WhereConstraint(self.alloc(WhereConstraint(
            self.node_to_ty(left_type).unwrap_or_else(|| tany()),
            match operator.token_kind() {
                Some(TokenKind::Equal) => ConstraintKind::ConstraintEq,
                Some(TokenKind::Super) => ConstraintKind::ConstraintSuper,
                _ => ConstraintKind::ConstraintAs,
            },
            self.node_to_ty(right_type).unwrap_or_else(|| tany()),
        )))
    }

    fn make_classish_declaration(
        &mut self,
        attributes: Self::R,
        modifiers: Self::R,
        xhp_keyword: Self::R,
        class_keyword: Self::R,
        name: Self::R,
        tparams: Self::R,
        _extends_keyword: Self::R,
        extends: Self::R,
        _implements_keyword: Self::R,
        implements: Self::R,
        where_clause: Self::R,
        body: Self::R,
    ) -> Self::R {
        let raw_name = match self.expect_name(name) {
            Some(Id(_, name)) => name,
            None => return Node::Ignored(SK::ClassishDeclaration),
        };
        let Id(pos, name) = match self.elaborate_defined_id(name) {
            Some(id) => id,
            None => return Node::Ignored(SK::ClassishDeclaration),
        };
        let is_xhp = raw_name.starts_with(':') || xhp_keyword.is_present();

        let mut class_kind = match class_keyword.token_kind() {
            Some(TokenKind::Interface) => ClassKind::Cinterface,
            Some(TokenKind::Trait) => ClassKind::Ctrait,
            _ => ClassKind::Cnormal,
        };
        let mut final_ = false;

        for modifier in modifiers.iter() {
            match modifier.token_kind() {
                Some(TokenKind::Abstract) => class_kind = ClassKind::Cabstract,
                Some(TokenKind::Final) => final_ = true,
                _ => {}
            }
        }

        let where_constraints = self.slice(where_clause.iter().filter_map(|&x| match x {
            Node::WhereConstraint(x) => Some(x),
            _ => None,
        }));

        let body = match body {
            Node::ClassishBody(body) => body,
            body => panic!("Expected a classish body, but was {:?}", body),
        };

        let mut uses_len = 0;
        let mut xhp_attr_uses_len = 0;
        let mut req_extends_len = 0;
        let mut req_implements_len = 0;
        let mut consts_len = 0;
        let mut typeconsts_len = 0;
        let mut props_len = 0;
        let mut sprops_len = 0;
        let mut static_methods_len = 0;
        let mut methods_len = 0;

        let mut user_attributes_len = 0;
        for attribute in attributes.iter() {
            match attribute {
                &Node::Attribute(..) => user_attributes_len += 1,
                _ => {}
            }
        }

        for element in body.iter().copied() {
            match element {
                Node::TraitUse(names) => uses_len += names.len(),
                Node::XhpClassAttributeDeclaration(&XhpClassAttributeDeclarationNode {
                    xhp_attr_decls,
                    xhp_attr_uses_decls,
                }) => {
                    props_len += xhp_attr_decls.len();
                    xhp_attr_uses_len += xhp_attr_uses_decls.len();
                }
                Node::TypeConstant(..) => typeconsts_len += 1,
                Node::RequireClause(require) => match require.require_type.token_kind() {
                    Some(TokenKind::Extends) => req_extends_len += 1,
                    Some(TokenKind::Implements) => req_implements_len += 1,
                    _ => {}
                },
                Node::List(consts @ [Node::Const(..), ..]) => consts_len += consts.len(),
                Node::Property(&PropertyNode { decls, is_static }) => {
                    if is_static {
                        sprops_len += decls.len()
                    } else {
                        props_len += decls.len()
                    }
                }
                Node::Constructor(&ConstructorNode { properties, .. }) => {
                    props_len += properties.len()
                }
                Node::Method(&MethodNode { is_static, .. }) => {
                    if is_static {
                        static_methods_len += 1
                    } else {
                        methods_len += 1
                    }
                }
                _ => {}
            }
        }

        let mut constructor = None;

        let mut uses = Vec::with_capacity_in(uses_len, self.arena);
        let mut xhp_attr_uses = Vec::with_capacity_in(xhp_attr_uses_len, self.arena);
        let mut req_extends = Vec::with_capacity_in(req_extends_len, self.arena);
        let mut req_implements = Vec::with_capacity_in(req_implements_len, self.arena);
        let mut consts = Vec::with_capacity_in(consts_len, self.arena);
        let mut typeconsts = Vec::with_capacity_in(typeconsts_len, self.arena);
        let mut props = Vec::with_capacity_in(props_len, self.arena);
        let mut sprops = Vec::with_capacity_in(sprops_len, self.arena);
        let mut static_methods = Vec::with_capacity_in(static_methods_len, self.arena);
        let mut methods = Vec::with_capacity_in(methods_len, self.arena);

        let mut user_attributes = Vec::with_capacity_in(user_attributes_len, self.arena);
        for attribute in attributes.iter() {
            match attribute {
                Node::Attribute(attr) => user_attributes.push(self.user_attribute_to_decl(&attr)),
                _ => {}
            }
        }
        // Match ordering of attributes produced by the OCaml decl parser (even
        // though it's the reverse of the syntactic ordering).
        user_attributes.reverse();

        // xhp props go after regular props, regardless of their order in file
        let mut xhp_props = vec![];

        for element in body.iter().copied() {
            match element {
                Node::TraitUse(names) => {
                    uses.extend(names.iter().filter_map(|&name| self.node_to_ty(name)))
                }
                Node::XhpClassAttributeDeclaration(&XhpClassAttributeDeclarationNode {
                    xhp_attr_decls,
                    xhp_attr_uses_decls,
                }) => {
                    xhp_props.extend(xhp_attr_decls);
                    xhp_attr_uses.extend(
                        xhp_attr_uses_decls
                            .iter()
                            .filter_map(|&node| self.node_to_ty(node)),
                    )
                }
                Node::TypeConstant(constant) => typeconsts.push(constant),
                Node::RequireClause(require) => match require.require_type.token_kind() {
                    Some(TokenKind::Extends) => {
                        req_extends.extend(self.node_to_ty(require.name).iter())
                    }
                    Some(TokenKind::Implements) => {
                        req_implements.extend(self.node_to_ty(require.name).iter())
                    }
                    _ => {}
                },
                Node::List(&const_nodes @ [Node::Const(..), ..]) => {
                    for node in const_nodes {
                        if let &Node::Const(decl) = node {
                            consts.push(decl)
                        }
                    }
                }
                Node::Property(&PropertyNode { decls, is_static }) => {
                    for property in decls {
                        if is_static {
                            sprops.push(property)
                        } else {
                            props.push(property)
                        }
                    }
                }
                Node::Constructor(&ConstructorNode { method, properties }) => {
                    constructor = Some(method);
                    for property in properties {
                        props.push(property)
                    }
                }
                Node::Method(&MethodNode { method, is_static }) => {
                    if is_static {
                        static_methods.push(method);
                    } else {
                        methods.push(method);
                    }
                }
                _ => {} // It's not our job to report errors here.
            }
        }

        props.extend(xhp_props.into_iter());

        let class_attributes = self.to_attributes(attributes);
        if class_attributes.const_ {
            for prop in props.iter_mut() {
                if !prop.flags.contains(PropFlags::CONST) {
                    *prop = self.alloc(ShallowProp {
                        flags: prop.flags | PropFlags::CONST,
                        ..**prop
                    })
                }
            }
        }

        let uses = uses.into_bump_slice();
        let xhp_attr_uses = xhp_attr_uses.into_bump_slice();
        let req_extends = req_extends.into_bump_slice();
        let req_implements = req_implements.into_bump_slice();
        let consts = consts.into_bump_slice();
        let typeconsts = typeconsts.into_bump_slice();
        let props = props.into_bump_slice();
        let sprops = sprops.into_bump_slice();
        let static_methods = static_methods.into_bump_slice();
        let methods = methods.into_bump_slice();
        let user_attributes = user_attributes.into_bump_slice();

        let extends = self.slice(extends.iter().filter_map(|&node| self.node_to_ty(node)));

        let mut implements_dynamic = false;
        let implements = self.slice(implements.iter().filter_map(
            |&node| match self.node_to_ty(node) {
                Some(Ty(_, Ty_::Tdynamic)) => {
                    implements_dynamic = true;
                    None
                }
                x => x,
            },
        ));

        // Pop the type params stack only after creating all inner types.
        let tparams = self.pop_type_params(tparams);

        let cls = self.alloc(shallow_decl_defs::ShallowClass {
            mode: self.file_mode,
            final_,
            is_xhp,
            has_xhp_keyword: xhp_keyword.is_token(TokenKind::XHP),
            kind: class_kind,
            name: Id(pos, name),
            tparams,
            where_constraints,
            extends,
            uses,
            xhp_attr_uses,
            req_extends,
            req_implements,
            implements,
            implements_dynamic,
            consts,
            typeconsts,
            props,
            sprops,
            constructor,
            static_methods,
            methods,
            user_attributes,
            enum_type: None,
        });
        self.add_class(name, cls);

        self.classish_name_builder.parsed_classish_declaration();

        Node::Ignored(SK::ClassishDeclaration)
    }

    fn make_property_declaration(
        &mut self,
        attrs: Self::R,
        modifiers: Self::R,
        hint: Self::R,
        declarators: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        let (attrs, modifiers, hint) = (attrs, modifiers, hint);
        let modifiers = read_member_modifiers(modifiers.iter());
        let declarators = self.slice(declarators.iter().filter_map(
            |declarator| match declarator {
                Node::ListItem(&(name, initializer)) => {
                    let attributes = self.to_attributes(attrs);
                    let Id(pos, name) = name.as_id()?;
                    let name = if modifiers.is_static {
                        name
                    } else {
                        strip_dollar_prefix(name)
                    };
                    let ty = self.node_to_non_ret_ty(hint);
                    let needs_init = if self.file_mode == Mode::Mdecl {
                        false
                    } else {
                        initializer.is_ignored()
                    };
                    let mut flags = PropFlags::empty();
                    flags.set(PropFlags::CONST, attributes.const_);
                    flags.set(PropFlags::LATEINIT, attributes.late_init);
                    flags.set(PropFlags::LSB, attributes.lsb);
                    flags.set(PropFlags::NEEDS_INIT, needs_init);
                    flags.set(PropFlags::ABSTRACT, modifiers.is_abstract);
                    flags.set(PropFlags::PHP_STD_LIB, attributes.php_std_lib);
                    Some(ShallowProp {
                        xhp_attr: None,
                        name: Id(pos, name),
                        type_: ty,
                        visibility: modifiers.visibility,
                        flags,
                    })
                }
                n => panic!("Expected a ListItem, but was {:?}", n),
            },
        ));
        Node::Property(self.alloc(PropertyNode {
            decls: declarators,
            is_static: modifiers.is_static,
        }))
    }

    fn make_xhp_class_attribute_declaration(
        &mut self,
        _keyword: Self::R,
        attributes: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        let xhp_attr_decls = self.slice(attributes.iter().filter_map(|node| {
            let node = match node {
                Node::XhpClassAttribute(x) => x,
                _ => return None,
            };
            let Id(pos, name) = node.name;
            let name = prefix_colon(self.arena, name);

            let type_ = self.node_to_ty(node.hint);
            let type_ = if node.nullable && node.tag.is_none() {
                type_.and_then(|x| match x {
                    // already nullable
                    Ty(_, Ty_::Toption(_)) | Ty(_, Ty_::Tmixed) => type_,
                    // make nullable
                    _ => self.node_to_ty(self.hint_ty(x.get_pos()?, Ty_::Toption(x))),
                })
            } else {
                type_
            };
            let mut flags = PropFlags::empty();
            flags.set(PropFlags::NEEDS_INIT, node.needs_init);
            Some(ShallowProp {
                name: Id(pos, name),
                visibility: aast::Visibility::Public,
                type_,
                xhp_attr: Some(shallow_decl_defs::XhpAttr {
                    tag: node.tag,
                    has_default: !node.needs_init,
                }),
                flags,
            })
        }));

        let xhp_attr_uses_decls = self.slice(attributes.iter().filter_map(|x| match x {
            Node::XhpAttributeUse(&name) => Some(name),
            _ => None,
        }));

        Node::XhpClassAttributeDeclaration(self.alloc(XhpClassAttributeDeclarationNode {
            xhp_attr_decls,
            xhp_attr_uses_decls,
        }))
    }

    fn make_xhp_enum_type(
        &mut self,
        enum_keyword: Self::R,
        _left_brace: Self::R,
        xhp_enum_values: Self::R,
        right_brace: Self::R,
    ) -> Self::R {
        let ty_opt = xhp_enum_values
            .iter()
            .next()
            .and_then(|x| self.node_to_ty(*x));
        match ty_opt {
            Some(ty) => self.hint_ty(self.merge_positions(enum_keyword, right_brace), ty.1),
            None => Node::Ignored(SK::XHPEnumType),
        }
    }

    fn make_xhp_class_attribute(
        &mut self,
        type_: Self::R,
        name: Self::R,
        initializer: Self::R,
        tag: Self::R,
    ) -> Self::R {
        let name = match name.as_id() {
            Some(name) => name,
            None => return Node::Ignored(SK::XHPClassAttribute),
        };
        Node::XhpClassAttribute(self.alloc(XhpClassAttributeNode {
            name,
            hint: type_,
            needs_init: !initializer.is_present(),
            tag: match tag.token_kind() {
                Some(TokenKind::Required) => Some(XhpAttrTag::Required),
                Some(TokenKind::Lateinit) => Some(XhpAttrTag::Lateinit),
                _ => None,
            },
            nullable: initializer.is_token(TokenKind::NullLiteral) || !initializer.is_present(),
        }))
    }

    fn make_xhp_simple_class_attribute(&mut self, name: Self::R) -> Self::R {
        Node::XhpAttributeUse(self.alloc(name))
    }

    fn make_property_declarator(&mut self, name: Self::R, initializer: Self::R) -> Self::R {
        Node::ListItem(self.alloc((name, initializer)))
    }

    fn make_methodish_declaration(
        &mut self,
        attributes: Self::R,
        header: Self::R,
        body: Self::R,
        closer: Self::R,
    ) -> Self::R {
        let header = match header {
            Node::FunctionHeader(header) => header,
            n => panic!("Expected a FunctionDecl header, but was {:?}", n),
        };
        // If we don't have a body, use the closing token. A closing token of
        // '}' indicates a regular function, while a closing token of ';'
        // indicates an abstract function.
        let body = if body.is_ignored() { closer } else { body };
        let modifiers = read_member_modifiers(header.modifiers.iter());
        let is_constructor = header.name.is_token(TokenKind::Construct);
        let is_method = true;
        let (id, ty, properties) = match self.function_to_ty(is_method, attributes, header, body) {
            Some(tuple) => tuple,
            None => return Node::Ignored(SK::MethodishDeclaration),
        };
        let attributes = self.to_attributes(attributes);
        let deprecated = attributes.deprecated.map(|msg| {
            let mut s = String::new_in(self.arena);
            s.push_str("The method ");
            s.push_str(id.1);
            s.push_str(" is deprecated: ");
            s.push_str(msg);
            s.into_bump_str()
        });
        fn get_condition_type_name<'a>(ty_opt: Option<&'a Ty<'a>>) -> Option<&'a str> {
            ty_opt.and_then(|ty| {
                let Ty(_, ty_) = ty;
                match *ty_ {
                    Ty_::Tapply(&(Id(_, class_name), _)) => Some(class_name),
                    _ => None,
                }
            })
        }
        let mut flags = MethodFlags::empty();
        flags.set(
            MethodFlags::ABSTRACT,
            self.classish_name_builder.in_interface() || modifiers.is_abstract,
        );
        flags.set(MethodFlags::FINAL, modifiers.is_final);
        flags.set(MethodFlags::OVERRIDE, attributes.override_);
        flags.set(
            MethodFlags::DYNAMICALLYCALLABLE,
            attributes.dynamically_callable,
        );
        flags.set(MethodFlags::PHP_STD_LIB, attributes.php_std_lib);
        let method = self.alloc(ShallowMethod {
            name: id,
            reactivity: match attributes.reactivity {
                Reactivity::Local(condition_type) => Some(MethodReactivity::MethodLocal(
                    get_condition_type_name(condition_type),
                )),
                Reactivity::Shallow(condition_type) => Some(MethodReactivity::MethodShallow(
                    get_condition_type_name(condition_type),
                )),
                Reactivity::Reactive(condition_type) => Some(MethodReactivity::MethodReactive(
                    get_condition_type_name(condition_type),
                )),
                Reactivity::Pure(condition_type) => Some(MethodReactivity::MethodPure(
                    get_condition_type_name(condition_type),
                )),
                Reactivity::Nonreactive
                | Reactivity::MaybeReactive(_)
                | Reactivity::RxVar(_)
                | Reactivity::Cipp(_)
                | Reactivity::CippLocal(_)
                | Reactivity::CippGlobal
                | Reactivity::CippRx => None,
            },
            type_: ty,
            visibility: modifiers.visibility,
            deprecated,
            flags,
        });
        if is_constructor {
            Node::Constructor(self.alloc(ConstructorNode { method, properties }))
        } else {
            Node::Method(self.alloc(MethodNode {
                method,
                is_static: modifiers.is_static,
            }))
        }
    }

    fn make_classish_body(
        &mut self,
        _left_brace: Self::R,
        elements: Self::R,
        _right_brace: Self::R,
    ) -> Self::R {
        Node::ClassishBody(self.alloc(elements.as_slice(self.arena)))
    }

    fn make_enum_declaration(
        &mut self,
        attributes: Self::R,
        _keyword: Self::R,
        name: Self::R,
        _colon: Self::R,
        extends: Self::R,
        constraint: Self::R,
        _left_brace: Self::R,
        use_clauses: Self::R,
        enumerators: Self::R,
        _right_brace: Self::R,
    ) -> Self::R {
        let id = match self.elaborate_defined_id(name) {
            Some(id) => id,
            None => return Node::Ignored(SK::EnumDeclaration),
        };
        let hint = match self.node_to_ty(extends) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::EnumDeclaration),
        };
        let extends = match self.node_to_ty(self.make_apply(
            Id(self.get_pos(name), "\\HH\\BuiltinEnum"),
            name,
            Pos::none(),
        )) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::EnumDeclaration),
        };
        let key = id.1;
        let consts = self.slice(enumerators.iter().filter_map(|node| match node {
            Node::ListItem(&(name, value)) => {
                let id = name.as_id()?;
                Some(
                    self.alloc(shallow_decl_defs::ShallowClassConst {
                        abstract_: false,
                        name: id,
                        type_: self
                            .infer_const(name, value)
                            .unwrap_or_else(|| self.tany_with_pos(id.0)),
                    }),
                )
            }
            n => panic!("Expected an enum case, got {:?}", n),
        }));

        let mut user_attributes = Vec::with_capacity_in(attributes.len(), self.arena);
        for attribute in attributes.iter() {
            match attribute {
                Node::Attribute(attr) => user_attributes.push(self.user_attribute_to_decl(attr)),
                _ => {}
            }
        }
        // Match ordering of attributes produced by the OCaml decl parser (even
        // though it's the reverse of the syntactic ordering).
        user_attributes.reverse();
        let user_attributes = user_attributes.into_bump_slice();

        let constraint = match constraint {
            Node::TypeConstraint(&(_kind, ty)) => self.node_to_ty(ty),
            _ => None,
        };

        let includes = self.slice(use_clauses.iter().filter_map(|&node| self.node_to_ty(node)));

        let cls = self.alloc(shallow_decl_defs::ShallowClass {
            mode: self.file_mode,
            final_: false,
            is_xhp: false,
            has_xhp_keyword: false,
            kind: ClassKind::Cenum,
            name: id,
            tparams: &[],
            where_constraints: &[],
            extends: bumpalo::vec![in self.arena; extends].into_bump_slice(),
            uses: &[],
            xhp_attr_uses: &[],
            req_extends: &[],
            req_implements: &[],
            implements: &[],
            implements_dynamic: false,
            consts,
            typeconsts: &[],
            props: &[],
            sprops: &[],
            constructor: None,
            static_methods: &[],
            methods: &[],
            user_attributes,
            enum_type: Some(self.alloc(EnumType {
                base: hint,
                constraint,
                includes,
                enum_class: false,
            })),
        });
        self.add_class(key, cls);

        self.classish_name_builder.parsed_classish_declaration();

        Node::Ignored(SK::EnumDeclaration)
    }

    fn make_enumerator(
        &mut self,
        name: Self::R,
        _equal: Self::R,
        value: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        Node::ListItem(self.alloc((name, value)))
    }

    fn make_enum_class_declaration(
        &mut self,
        attributes: Self::R,
        _enum_keyword: Self::R,
        _class_keyword: Self::R,
        name: Self::R,
        _colon: Self::R,
        base: Self::R,
        _extends_keyword: Self::R,
        extends_list: Self::R,
        _left_brace: Self::R,
        elements: Self::R,
        _right_brace: Self::R,
    ) -> Self::R {
        let name = match self.elaborate_defined_id(name) {
            Some(name) => name,
            None => return Node::Ignored(SyntaxKind::EnumClassDeclaration),
        };
        let base = self
            .node_to_ty(base)
            .unwrap_or_else(|| self.tany_with_pos(name.0));

        let builtin_enum_class_ty = {
            let pos = name.0;
            let enum_class_ty_ = Ty_::Tapply(self.alloc((name, &[])));
            let enum_class_ty = self.alloc(Ty(self.alloc(Reason::hint(pos)), enum_class_ty_));
            let elt_ty_ = Ty_::Tapply(self.alloc((
                Id(pos, "\\HH\\MemberOf"),
                bumpalo::vec![in self.arena; enum_class_ty, base].into_bump_slice(),
            )));
            let elt_ty = self.alloc(Ty(self.alloc(Reason::hint(pos)), elt_ty_));
            let builtin_enum_ty_ = Ty_::Tapply(self.alloc((
                Id(pos, "\\HH\\BuiltinEnumClass"),
                std::slice::from_ref(self.alloc(elt_ty)),
            )));
            self.alloc(Ty(self.alloc(Reason::hint(pos)), builtin_enum_ty_))
        };

        let consts = self.slice(elements.iter().filter_map(|node| match node {
            &Node::Const(const_) => Some(const_),
            _ => None,
        }));

        let mut extends = Vec::with_capacity_in(extends_list.len() + 1, self.arena);
        extends.push(builtin_enum_class_ty);
        extends.extend(extends_list.iter().filter_map(|&n| self.node_to_ty(n)));
        let extends = extends.into_bump_slice();
        let includes = &extends[1..];

        let mut user_attributes = Vec::with_capacity_in(attributes.len() + 1, self.arena);
        user_attributes.push(self.alloc(shallow_decl_defs::UserAttribute {
            name: Id(name.0, "__EnumClass"),
            classname_params: &[],
        }));
        for attribute in attributes.iter() {
            match attribute {
                Node::Attribute(attr) => user_attributes.push(self.user_attribute_to_decl(attr)),
                _ => {}
            }
        }
        // Match ordering of attributes produced by the OCaml decl parser (even
        // though it's the reverse of the syntactic ordering).
        user_attributes.reverse();
        let user_attributes = user_attributes.into_bump_slice();

        let cls = self.alloc(shallow_decl_defs::ShallowClass {
            mode: self.file_mode,
            final_: false,
            is_xhp: false,
            has_xhp_keyword: false,
            kind: ClassKind::Cenum,
            name,
            tparams: &[],
            where_constraints: &[],
            extends,
            uses: &[],
            xhp_attr_uses: &[],
            req_extends: &[],
            req_implements: &[],
            implements: &[],
            implements_dynamic: false,
            consts,
            typeconsts: &[],
            props: &[],
            sprops: &[],
            constructor: None,
            static_methods: &[],
            methods: &[],
            user_attributes,
            enum_type: Some(self.alloc(EnumType {
                base,
                constraint: None,
                includes,
                enum_class: true,
            })),
        });
        self.add_class(name.1, cls);

        self.classish_name_builder.parsed_classish_declaration();

        Node::Ignored(SyntaxKind::EnumClassDeclaration)
    }

    fn make_enum_class_enumerator(
        &mut self,
        type_: Self::R,
        name: Self::R,
        _equal: Self::R,
        _initial_value: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        let name = match self.expect_name(name) {
            Some(name) => name,
            None => return Node::Ignored(SyntaxKind::EnumClassEnumerator),
        };
        let pos = name.0;
        let type_ = self
            .node_to_ty(type_)
            .unwrap_or_else(|| self.tany_with_pos(name.0));
        let class_name = match self.classish_name_builder.get_current_classish_name() {
            Some(name) => name,
            None => return Node::Ignored(SyntaxKind::EnumClassEnumerator),
        };
        let enum_class_ty_ = Ty_::Tapply(self.alloc((Id(pos, class_name.0), &[])));
        let enum_class_ty = self.alloc(Ty(self.alloc(Reason::hint(pos)), enum_class_ty_));
        let type_ = Ty_::Tapply(self.alloc((
            Id(pos, "\\HH\\MemberOf"),
            bumpalo::vec![in self.arena; enum_class_ty, type_].into_bump_slice(),
        )));
        let type_ = self.alloc(Ty(self.alloc(Reason::hint(pos)), type_));
        Node::Const(self.alloc(ShallowClassConst {
            abstract_: false,
            name,
            type_,
        }))
    }

    fn make_tuple_type_specifier(
        &mut self,
        left_paren: Self::R,
        tys: Self::R,
        right_paren: Self::R,
    ) -> Self::R {
        // We don't need to include the tys list in this position merging
        // because by definition it's already contained by the two brackets.
        let pos = self.merge_positions(left_paren, right_paren);
        let tys = self.slice(tys.iter().filter_map(|&node| self.node_to_ty(node)));
        self.hint_ty(pos, Ty_::Ttuple(tys))
    }

    fn make_tuple_type_explicit_specifier(
        &mut self,
        keyword: Self::R,
        _left_angle: Self::R,
        types: Self::R,
        right_angle: Self::R,
    ) -> Self::R {
        let id = Id(self.get_pos(keyword), "\\tuple");
        // This is an error--tuple syntax is (A, B), not tuple<A, B>.
        // OCaml decl makes a Tapply rather than a Ttuple here.
        self.make_apply(id, types, self.get_pos(right_angle))
    }

    fn make_intersection_type_specifier(
        &mut self,
        left_paren: Self::R,
        tys: Self::R,
        right_paren: Self::R,
    ) -> Self::R {
        let pos = self.merge_positions(left_paren, right_paren);
        let tys = self.slice(tys.iter().filter_map(|x| match x {
            Node::ListItem(&(ty, _ampersand)) => self.node_to_ty(ty),
            &x => self.node_to_ty(x),
        }));
        self.hint_ty(pos, Ty_::Tintersection(tys))
    }

    fn make_union_type_specifier(
        &mut self,
        left_paren: Self::R,
        tys: Self::R,
        right_paren: Self::R,
    ) -> Self::R {
        let pos = self.merge_positions(left_paren, right_paren);
        let tys = self.slice(tys.iter().filter_map(|x| match x {
            Node::ListItem(&(ty, _bar)) => self.node_to_ty(ty),
            &x => self.node_to_ty(x),
        }));
        self.hint_ty(pos, Ty_::Tunion(tys))
    }

    fn make_shape_type_specifier(
        &mut self,
        shape: Self::R,
        _lparen: Self::R,
        fields: Self::R,
        open: Self::R,
        rparen: Self::R,
    ) -> Self::R {
        let fields = fields;
        let fields_iter = fields.iter();
        let mut fields = AssocListMut::new_in(self.arena);
        for node in fields_iter {
            match node {
                &Node::ShapeFieldSpecifier(&ShapeFieldNode { name, type_ }) => {
                    fields.insert(*name, type_)
                }
                n => panic!("Expected a shape field specifier, but was {:?}", n),
            }
        }
        let kind = match open.token_kind() {
            Some(TokenKind::DotDotDot) => ShapeKind::OpenShape,
            _ => ShapeKind::ClosedShape,
        };
        let pos = self.merge_positions(shape, rparen);
        self.hint_ty(pos, Ty_::Tshape(self.alloc((kind, fields.into()))))
    }

    fn make_shape_expression(
        &mut self,
        shape: Self::R,
        _left_paren: Self::R,
        fields: Self::R,
        right_paren: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|node| match node {
            Node::ListItem(&(key, value)) => {
                let key = self.make_shape_field_name(key)?;
                let value = self.node_to_expr(value)?;
                Some((key, value))
            }
            n => panic!("Expected a ListItem but was {:?}", n),
        }));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(shape, right_paren),
            nast::Expr_::Shape(fields),
        )))
    }

    fn make_tuple_expression(
        &mut self,
        tuple: Self::R,
        _left_paren: Self::R,
        fields: Self::R,
        right_paren: Self::R,
    ) -> Self::R {
        let fields = self.slice(fields.iter().filter_map(|&field| self.node_to_expr(field)));
        Node::Expr(self.alloc(aast::Expr(
            self.merge_positions(tuple, right_paren),
            nast::Expr_::List(fields),
        )))
    }

    fn make_classname_type_specifier(
        &mut self,
        classname: Self::R,
        _lt: Self::R,
        targ: Self::R,
        _trailing_comma: Self::R,
        gt: Self::R,
    ) -> Self::R {
        let id = match classname.as_id() {
            Some(id) => id,
            None => return Node::Ignored(SK::ClassnameTypeSpecifier),
        };
        if gt.is_ignored() {
            self.prim_ty(aast::Tprim::Tstring, id.0)
        } else {
            self.make_apply(
                Id(id.0, self.elaborate_raw_id(id.1)),
                targ,
                self.merge_positions(classname, gt),
            )
        }
    }

    fn make_scope_resolution_expression(
        &mut self,
        class_name: Self::R,
        _operator: Self::R,
        value: Self::R,
    ) -> Self::R {
        let pos = self.merge_positions(class_name, value);
        let Id(class_name_pos, class_name_str) = match self.expect_name(class_name) {
            Some(id) => self.elaborate_id(id),
            None => return Node::Ignored(SK::ScopeResolutionExpression),
        };
        let class_id = self.alloc(aast::ClassId(
            class_name_pos,
            match class_name {
                Node::Name(("self", _)) => aast::ClassId_::CIself,
                _ => aast::ClassId_::CI(self.alloc(Id(class_name_pos, class_name_str))),
            },
        ));
        let value_id = match self.expect_name(value) {
            Some(id) => id,
            None => return Node::Ignored(SK::ScopeResolutionExpression),
        };
        Node::Expr(self.alloc(aast::Expr(
            pos,
            nast::Expr_::ClassConst(self.alloc((class_id, self.alloc((value_id.0, value_id.1))))),
        )))
    }

    fn make_field_specifier(
        &mut self,
        question_token: Self::R,
        name: Self::R,
        _arrow: Self::R,
        type_: Self::R,
    ) -> Self::R {
        let optional = question_token.is_present();
        let ty = match self.node_to_ty(type_) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::FieldSpecifier),
        };
        let name = match self.make_shape_field_name(name) {
            Some(name) => name,
            None => return Node::Ignored(SK::FieldSpecifier),
        };
        Node::ShapeFieldSpecifier(self.alloc(ShapeFieldNode {
            name: self.alloc(ShapeField(name)),
            type_: self.alloc(ShapeFieldType { optional, ty }),
        }))
    }

    fn make_field_initializer(&mut self, key: Self::R, _arrow: Self::R, value: Self::R) -> Self::R {
        Node::ListItem(self.alloc((key, value)))
    }

    fn make_varray_type_specifier(
        &mut self,
        varray_keyword: Self::R,
        _less_than: Self::R,
        tparam: Self::R,
        _trailing_comma: Self::R,
        greater_than: Self::R,
    ) -> Self::R {
        let tparam = match self.node_to_ty(tparam) {
            Some(ty) => ty,
            None => self.tany_with_pos(self.get_pos(varray_keyword)),
        };
        self.hint_ty(
            self.merge_positions(varray_keyword, greater_than),
            Ty_::Tvarray(tparam),
        )
    }

    fn make_darray_type_specifier(
        &mut self,
        darray: Self::R,
        _less_than: Self::R,
        key_type: Self::R,
        _comma: Self::R,
        value_type: Self::R,
        _trailing_comma: Self::R,
        greater_than: Self::R,
    ) -> Self::R {
        let pos = self.merge_positions(darray, greater_than);
        let key_type = self.node_to_ty(key_type).unwrap_or(TANY);
        let value_type = self.node_to_ty(value_type).unwrap_or(TANY);
        self.hint_ty(pos, Ty_::Tdarray(self.alloc((key_type, value_type))))
    }

    fn make_old_attribute_specification(
        &mut self,
        ltlt: Self::R,
        attrs: Self::R,
        gtgt: Self::R,
    ) -> Self::R {
        match attrs {
            Node::List(nodes) => {
                Node::BracketedList(self.alloc((self.get_pos(ltlt), nodes, self.get_pos(gtgt))))
            }
            node => panic!(
                "Expected List in old_attribute_specification, but got {:?}",
                node
            ),
        }
    }

    fn make_constructor_call(
        &mut self,
        name: Self::R,
        _left_paren: Self::R,
        args: Self::R,
        _right_paren: Self::R,
    ) -> Self::R {
        let unqualified_name = match self.expect_name(name) {
            Some(name) => name,
            None => return Node::Ignored(SK::ConstructorCall),
        };
        let name = if unqualified_name.1.starts_with("__") {
            unqualified_name
        } else {
            match self.expect_name(name) {
                Some(name) => self.elaborate_id(name),
                None => return Node::Ignored(SK::ConstructorCall),
            }
        };
        let classname_params = self.slice(args.iter().filter_map(|node| match node {
            Node::Expr(aast::Expr(
                full_pos,
                aast::Expr_::ClassConst(&(
                    aast::ClassId(_, aast::ClassId_::CI(&Id(pos, class_name))),
                    (_, "class"),
                )),
            )) => {
                let name = self.elaborate_id(Id(pos, class_name));
                Some(ClassNameParam { name, full_pos })
            }
            _ => None,
        }));

        let string_literal_params = if match name.1 {
            "__Deprecated" | "__Cipp" | "__CippLocal" | "__Policied" => true,
            _ => false,
        } {
            fn fold_string_concat<'a>(expr: &nast::Expr<'a>, acc: &mut Vec<'a, u8>) {
                match expr {
                    &aast::Expr(_, aast::Expr_::String(val)) => acc.extend_from_slice(val),
                    &aast::Expr(_, aast::Expr_::Binop(&(Bop::Dot, e1, e2))) => {
                        fold_string_concat(&e1, acc);
                        fold_string_concat(&e2, acc);
                    }
                    _ => {}
                }
            }

            self.slice(args.iter().filter_map(|expr| match expr {
                Node::StringLiteral((x, _)) => Some(*x),
                Node::Expr(e @ aast::Expr(_, aast::Expr_::Binop(_))) => {
                    let mut acc = Vec::new_in(self.arena);
                    fold_string_concat(e, &mut acc);
                    Some(acc.into_bump_slice().into())
                }
                _ => None,
            }))
        } else {
            &[]
        };

        Node::Attribute(self.alloc(UserAttributeNode {
            name,
            classname_params,
            string_literal_params,
        }))
    }

    fn make_trait_use(
        &mut self,
        _keyword: Self::R,
        names: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        Node::TraitUse(self.alloc(names))
    }

    fn make_trait_use_conflict_resolution(
        &mut self,
        _keyword: Self::R,
        names: Self::R,
        _left_brace: Self::R,
        _clauses: Self::R,
        _right_brace: Self::R,
    ) -> Self::R {
        Node::TraitUse(self.alloc(names))
    }

    fn make_require_clause(
        &mut self,
        _keyword: Self::R,
        require_type: Self::R,
        name: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        Node::RequireClause(self.alloc(RequireClause { require_type, name }))
    }

    fn make_nullable_type_specifier(&mut self, question_mark: Self::R, hint: Self::R) -> Self::R {
        let pos = self.merge_positions(question_mark, hint);
        let ty = match self.node_to_ty(hint) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::NullableTypeSpecifier),
        };
        self.hint_ty(pos, Ty_::Toption(ty))
    }

    fn make_like_type_specifier(&mut self, tilde: Self::R, hint: Self::R) -> Self::R {
        let pos = self.merge_positions(tilde, hint);
        let ty = match self.node_to_ty(hint) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::LikeTypeSpecifier),
        };
        self.hint_ty(pos, Ty_::Tlike(ty))
    }

    fn make_closure_type_specifier(
        &mut self,
        outer_left_paren: Self::R,
        _function_keyword: Self::R,
        _inner_left_paren: Self::R,
        parameter_list: Self::R,
        _inner_right_paren: Self::R,
        capability: Self::R,
        _colon: Self::R,
        return_type: Self::R,
        outer_right_paren: Self::R,
    ) -> Self::R {
        let make_param = |fp: &'a FunParamDecl<'a>| -> &'a FunParam<'a> {
            let mut flags = FunParamFlags::empty();
            let (hint, mutability) = Self::unwrap_mutability(fp.hint);
            flags |= Self::param_mutability_to_fun_param_flags(mutability);

            match fp.kind {
                ParamMode::FPinout => {
                    flags |= FunParamFlags::INOUT;
                }
                ParamMode::FPnormal => {}
            };

            self.alloc(FunParam {
                pos: self.get_pos(hint),
                name: None,
                type_: self.alloc(PossiblyEnforcedTy {
                    enforced: false,
                    type_: self.node_to_ty(hint).unwrap_or_else(|| tany()),
                }),
                flags,
                rx_annotation: None,
            })
        };

        let arity = parameter_list
            .iter()
            .find_map(|&node| match node {
                Node::FunParam(fp) if fp.variadic => Some(FunArity::Fvariadic(make_param(fp))),
                _ => None,
            })
            .unwrap_or(FunArity::Fstandard);

        let params = self.slice(parameter_list.iter().filter_map(|&node| match node {
            Node::FunParam(fp) if !fp.variadic => Some(make_param(fp)),
            _ => None,
        }));

        let (hint, mutability) = Self::unwrap_mutability(return_type);
        let ret = match self.node_to_ty(hint) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::ClosureTypeSpecifier),
        };
        let pos = self.merge_positions(outer_left_paren, outer_right_paren);
        let implicit_params = self.as_fun_implicit_params(capability, pos);

        let mut flags = FunTypeFlags::empty();
        if mutability.is_some() {
            flags |= FunTypeFlags::RETURNS_MUTABLE;
        }

        self.hint_ty(
            pos,
            Ty_::Tfun(self.alloc(FunType {
                arity,
                tparams: &[],
                where_constraints: &[],
                params,
                implicit_params,
                ret: self.alloc(PossiblyEnforcedTy {
                    enforced: false,
                    type_: ret,
                }),
                reactive: Reactivity::Nonreactive,
                flags,
                ifc_decl: default_ifc_fun_decl(),
            })),
        )
    }

    fn make_closure_parameter_type_specifier(&mut self, inout: Self::R, hint: Self::R) -> Self::R {
        let kind = if inout.is_token(TokenKind::Inout) {
            ParamMode::FPinout
        } else {
            ParamMode::FPnormal
        };
        Node::FunParam(self.alloc(FunParamDecl {
            attributes: Node::Ignored(SK::Missing),
            visibility: Node::Ignored(SK::Missing),
            kind,
            hint,
            pos: self.get_pos(hint),
            name: Some(""),
            variadic: false,
            initializer: Node::Ignored(SK::Missing),
        }))
    }

    fn make_type_const_declaration(
        &mut self,
        attributes: Self::R,
        modifiers: Self::R,
        _const_keyword: Self::R,
        _type_keyword: Self::R,
        name: Self::R,
        _type_parameters: Self::R,
        constraint: Self::R,
        _equal: Self::R,
        type_: Self::R,
        _semicolon: Self::R,
    ) -> Self::R {
        let attributes = self.to_attributes(attributes);
        let has_abstract_keyword = modifiers
            .iter()
            .any(|node| node.is_token(TokenKind::Abstract));
        let constraint = match constraint {
            Node::TypeConstraint(innards) => self.node_to_ty(innards.1),
            _ => None,
        };
        let type_ = self.node_to_ty(type_);
        let has_constraint = constraint.is_some();
        let has_type = type_.is_some();
        let (type_, abstract_) = match (has_abstract_keyword, has_constraint, has_type) {
            // Has no assigned type. Technically illegal, so if the constraint
            // is present, proceed as if the constraint was the assigned type.
            //     const type TFoo;
            //     const type TFoo as OtherType;
            (false, _, false) => (constraint, TypeconstAbstractKind::TCConcrete),
            // Has no constraint, but does have an assigned type.
            //     const type TFoo = SomeType;
            (false, false, true) => (type_, TypeconstAbstractKind::TCConcrete),
            // Has both a constraint and an assigned type.
            //     const type TFoo as OtherType = SomeType;
            (false, true, true) => (type_, TypeconstAbstractKind::TCPartiallyAbstract),
            // Has no default type.
            //     abstract const type TFoo;
            //     abstract const type TFoo as OtherType;
            (true, _, false) => (type_, TypeconstAbstractKind::TCAbstract(None)),
            // Has a default type.
            //     abstract const Type TFoo = SomeType;
            //     abstract const Type TFoo as OtherType = SomeType;
            (true, _, true) => (None, TypeconstAbstractKind::TCAbstract(type_)),
        };
        let name = match name.as_id() {
            Some(name) => name,
            None => return Node::Ignored(SK::TypeConstDeclaration),
        };
        Node::TypeConstant(self.alloc(ShallowTypeconst {
            abstract_,
            constraint,
            name,
            type_,
            enforceable: match attributes.enforceable {
                Some(pos) => (pos, true),
                None => (Pos::none(), false),
            },
            reifiable: attributes.reifiable,
        }))
    }

    fn make_decorated_expression(&mut self, decorator: Self::R, expr: Self::R) -> Self::R {
        Node::ListItem(self.alloc((decorator, expr)))
    }

    fn make_type_constant(
        &mut self,
        ty: Self::R,
        _coloncolon: Self::R,
        constant_name: Self::R,
    ) -> Self::R {
        let id = match self.expect_name(constant_name) {
            Some(id) => id,
            None => return Node::Ignored(SK::TypeConstant),
        };
        let pos = self.merge_positions(ty, constant_name);
        let ty = match (ty, self.classish_name_builder.get_current_classish_name()) {
            (Node::Name(("self", self_pos)), Some((name, class_name_pos))) => {
                // In classes, we modify the position when rewriting the
                // `self` keyword to point to the class name. In traits,
                // we don't (because traits are not types). We indicate
                // that the position shouldn't be rewritten with the
                // none Pos.
                let id_pos = if class_name_pos.is_none() {
                    self_pos
                } else {
                    class_name_pos
                };
                let reason = self.alloc(Reason::hint(self_pos));
                let ty_ = Ty_::Tapply(self.alloc((Id(id_pos, name), &[][..])));
                self.alloc(Ty(reason, ty_))
            }
            _ => match self.node_to_ty(ty) {
                Some(ty) => ty,
                None => return Node::Ignored(SK::TypeConstant),
            },
        };
        let reason = self.alloc(Reason::hint(pos));
        // The reason-rewriting here is only necessary to match the
        // behavior of OCaml decl (which flattens and then unflattens
        // Haccess hints, losing some position information).
        let ty = self.rewrite_taccess_reasons(ty, reason);
        Node::Ty(self.alloc(Ty(reason, Ty_::Taccess(self.alloc(TaccessType(ty, id))))))
    }

    fn make_soft_type_specifier(&mut self, at_token: Self::R, hint: Self::R) -> Self::R {
        let pos = self.merge_positions(at_token, hint);
        let hint = match self.node_to_ty(hint) {
            Some(ty) => ty,
            None => return Node::Ignored(SK::SoftTypeSpecifier),
        };
        // Use the type of the hint as-is (i.e., throw away the knowledge that
        // we had a soft type specifier here--the typechecker does not use it).
        // Replace its Reason with one including the position of the `@` token.
        self.hint_ty(pos, hint.1)
    }

    // A type specifier preceded by an attribute list. At the time of writing,
    // only the <<__Soft>> attribute is permitted here.
    fn make_attributized_specifier(&mut self, attributes: Self::R, hint: Self::R) -> Self::R {
        match attributes {
            Node::BracketedList((
                ltlt_pos,
                [Node::Attribute(UserAttributeNode {
                    name: Id(_, "__Soft"),
                    ..
                })],
                gtgt_pos,
            )) => {
                let attributes_pos = self.merge(*ltlt_pos, *gtgt_pos);
                let hint_pos = self.get_pos(hint);
                // Use the type of the hint as-is (i.e., throw away the
                // knowledge that we had a soft type specifier here--the
                // typechecker does not use it). Replace its Reason with one
                // including the position of the attribute list.
                let hint = match self.node_to_ty(hint) {
                    Some(ty) => ty,
                    None => return Node::Ignored(SK::AttributizedSpecifier),
                };
                self.hint_ty(self.merge(attributes_pos, hint_pos), hint.1)
            }
            _ => hint,
        }
    }

    fn make_vector_type_specifier(
        &mut self,
        vec: Self::R,
        _left_angle: Self::R,
        hint: Self::R,
        _trailing_comma: Self::R,
        right_angle: Self::R,
    ) -> Self::R {
        let id = match self.expect_name(vec) {
            Some(id) => id,
            None => return Node::Ignored(SK::VectorTypeSpecifier),
        };
        let id = Id(id.0, self.elaborate_raw_id(id.1));
        self.make_apply(id, hint, self.get_pos(right_angle))
    }

    fn make_dictionary_type_specifier(
        &mut self,
        dict: Self::R,
        _left_angle: Self::R,
        type_arguments: Self::R,
        right_angle: Self::R,
    ) -> Self::R {
        let id = match self.expect_name(dict) {
            Some(id) => id,
            None => return Node::Ignored(SK::DictionaryTypeSpecifier),
        };
        let id = Id(id.0, self.elaborate_raw_id(id.1));
        self.make_apply(id, type_arguments, self.get_pos(right_angle))
    }

    fn make_keyset_type_specifier(
        &mut self,
        keyset: Self::R,
        _left_angle: Self::R,
        hint: Self::R,
        _trailing_comma: Self::R,
        right_angle: Self::R,
    ) -> Self::R {
        let id = match self.expect_name(keyset) {
            Some(id) => id,
            None => return Node::Ignored(SK::KeysetTypeSpecifier),
        };
        let id = Id(id.0, self.elaborate_raw_id(id.1));
        self.make_apply(id, hint, self.get_pos(right_angle))
    }

    fn make_variable_expression(&mut self, _expression: Self::R) -> Self::R {
        Node::Ignored(SK::VariableExpression)
    }

    fn make_subscript_expression(
        &mut self,
        _receiver: Self::R,
        _left_bracket: Self::R,
        _index: Self::R,
        _right_bracket: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::SubscriptExpression)
    }

    fn make_member_selection_expression(
        &mut self,
        _object: Self::R,
        _operator: Self::R,
        _name: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::MemberSelectionExpression)
    }

    fn make_object_creation_expression(
        &mut self,
        _new_keyword: Self::R,
        _object: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::ObjectCreationExpression)
    }

    fn make_safe_member_selection_expression(
        &mut self,
        _object: Self::R,
        _operator: Self::R,
        _name: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::SafeMemberSelectionExpression)
    }

    fn make_function_call_expression(
        &mut self,
        _receiver: Self::R,
        _type_args: Self::R,
        _left_paren: Self::R,
        _argument_list: Self::R,
        _right_paren: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::FunctionCallExpression)
    }

    fn make_list_expression(
        &mut self,
        _keyword: Self::R,
        _left_paren: Self::R,
        _members: Self::R,
        _right_paren: Self::R,
    ) -> Self::R {
        Node::Ignored(SK::ListExpression)
    }
}
