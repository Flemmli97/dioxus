//! Virtual Node Support
//! VNodes represent lazily-constructed VDom trees that support diffing and event handlers.
//!
//! These VNodes should be *very* cheap and *very* fast to construct - building a full tree should be insanely quick.

use crate::{
    events::VirtualEvent,
    innerlude::{Context, Properties, Scope, ScopeIdx, FC},
    nodebuilder::text3,
    virtual_dom::{NodeCtx, RealDomNode},
};
use bumpalo::Bump;
use std::{
    any::Any,
    cell::{Cell, RefCell},
    fmt::{Arguments, Debug},
    marker::PhantomData,
    rc::Rc,
};

/// Tools for the base unit of the virtual dom - the VNode
/// VNodes are intended to be quickly-allocated, lightweight enum values.
///
/// Components will be generating a lot of these very quickly, so we want to
/// limit the amount of heap allocations / overly large enum sizes.
pub enum VNode<'src> {
    /// An element node (node type `ELEMENT_NODE`).
    Element(&'src VElement<'src>),

    /// A text node (node type `TEXT_NODE`).
    Text(VText<'src>),

    /// A fragment is a "virtual position" in the DOM
    /// Fragments may have children and keys
    Fragment(&'src VFragment<'src>),

    /// A "suspended component"
    /// This is a masqeurade over an underlying future that needs to complete
    /// When the future is completed, the VNode will then trigger a render
    Suspended,

    /// A User-defined componen node (node type COMPONENT_NODE)
    Component(&'src VComponent<'src>),
}

// it's okay to clone because vnodes are just references to places into the bump
impl<'a> Clone for VNode<'a> {
    fn clone(&self) -> Self {
        match self {
            VNode::Element(element) => VNode::Element(element),
            VNode::Text(old) => VNode::Text(old.clone()),
            VNode::Fragment(fragment) => VNode::Fragment(fragment),
            VNode::Component(component) => VNode::Component(component),
            VNode::Suspended => VNode::Suspended,
        }
    }
}

impl<'a> VNode<'a> {
    /// Low-level constructor for making a new `Node` of type element with given
    /// parts.
    ///
    /// This is primarily intended for JSX and templating proc-macros to compile
    /// down into. If you are building nodes by-hand, prefer using the
    /// `dodrio::builder::*` APIs.
    #[inline]
    pub fn element(
        bump: &'a Bump,
        key: NodeKey<'a>,
        tag_name: &'a str,
        listeners: &'a [Listener<'a>],
        attributes: &'a [Attribute<'a>],
        children: &'a [VNode<'a>],
        namespace: Option<&'a str>,
    ) -> VNode<'a> {
        let element = bump.alloc_with(|| VElement {
            key,
            tag_name,
            listeners,
            attributes,
            children,
            namespace,
            dom_id: Cell::new(RealDomNode::empty()),
        });
        VNode::Element(element)
    }

    /// Construct a new text node with the given text.
    #[inline]
    pub fn text(text: &'a str) -> VNode<'a> {
        VNode::Text(VText {
            text,
            dom_id: Cell::new(RealDomNode::empty()),
        })
    }

    pub fn text_args(bump: &'a Bump, args: Arguments) -> VNode<'a> {
        text3(bump, args)
    }

    #[inline]
    pub(crate) fn key(&self) -> NodeKey {
        match &self {
            VNode::Text { .. } => NodeKey::NONE,
            VNode::Element(e) => e.key,
            VNode::Fragment(frag) => frag.key,
            VNode::Component(c) => c.key,

            // todo suspend should be allowed to have keys
            VNode::Suspended => NodeKey::NONE,
        }
    }
}

#[derive(Clone)]
pub struct VText<'src> {
    pub text: &'src str,
    pub dom_id: Cell<RealDomNode>,
}

// ========================================================
//   VElement (div, h1, etc), attrs, keys, listener handle
// ========================================================
pub struct VElement<'a> {
    /// Elements have a tag name, zero or more attributes, and zero or more
    pub key: NodeKey<'a>,
    pub tag_name: &'a str,
    pub listeners: &'a [Listener<'a>],
    pub attributes: &'a [Attribute<'a>],
    pub children: &'a [VNode<'a>],
    pub namespace: Option<&'a str>,
    pub dom_id: Cell<RealDomNode>,
}

/// An attribute on a DOM node, such as `id="my-thing"` or
/// `href="https://example.com"`.
#[derive(Clone, Debug)]
pub struct Attribute<'a> {
    pub name: &'static str,
    pub value: &'a str,
}

impl<'a> Attribute<'a> {
    /// Get this attribute's name, such as `"id"` in `<div id="my-thing" />`.
    #[inline]
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// The attribute value, such as `"my-thing"` in `<div id="my-thing" />`.
    #[inline]
    pub fn value(&self) -> &'a str {
        self.value
    }

    /// Certain attributes are considered "volatile" and can change via user
    /// input that we can't see when diffing against the old virtual DOM. For
    /// these attributes, we want to always re-set the attribute on the physical
    /// DOM node, even if the old and new virtual DOM nodes have the same value.
    #[inline]
    pub(crate) fn is_volatile(&self) -> bool {
        match self.name {
            "value" | "checked" | "selected" => true,
            _ => false,
        }
    }
}

pub struct ListenerHandle {
    pub event: &'static str,
    pub scope: ScopeIdx,
    pub id: usize,
}

/// An event listener.
pub struct Listener<'bump> {
    /// The type of event to listen for.
    pub(crate) event: &'static str,

    pub scope: ScopeIdx,
    pub id: usize,

    /// The callback to invoke when the event happens.
    pub(crate) callback: &'bump (dyn Fn(VirtualEvent)),
}

/// The key for keyed children.
///
/// Keys must be unique among siblings.
///
/// If any sibling is keyed, then they all must be keyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeKey<'a>(pub(crate) Option<&'a str>);

impl<'a> Default for NodeKey<'a> {
    fn default() -> NodeKey<'a> {
        NodeKey::NONE
    }
}
impl<'a> NodeKey<'a> {
    /// The default, lack of a key.
    pub const NONE: NodeKey<'a> = NodeKey(None);

    /// Is this key `NodeKey::NONE`?
    #[inline]
    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }

    /// Is this key not `NodeKey::NONE`?
    #[inline]
    pub fn is_some(&self) -> bool {
        !self.is_none()
    }

    /// Create a new `NodeKey`.
    ///
    /// `key` must not be `u32::MAX`.
    #[inline]
    pub fn new(key: &'a str) -> Self {
        NodeKey(Some(key))
    }
}

// ==============================
//   Custom components
// ==============================

/// Virtual Components for custom user-defined components
/// Only supports the functional syntax
pub type StableScopeAddres = Option<u32>;
pub type VCompAssociatedScope = Option<ScopeIdx>;

pub struct VComponent<'src> {
    pub key: NodeKey<'src>,

    pub mounted_root: Cell<RealDomNode>,
    pub ass_scope: RefCell<VCompAssociatedScope>,

    // pub comparator: Rc<dyn Fn(&VComponent) -> bool + 'src>,
    pub caller: Rc<dyn Fn(&Scope) -> VNode>,

    pub children: &'src [VNode<'src>],

    pub comparator: Option<&'src dyn Fn(&VComponent) -> bool>,

    // a pointer into the bump arena (given by the 'src lifetime)
    // raw_props: Box<dyn Any>,
    raw_props: *const (),

    // a pointer to the raw fn typ
    pub user_fc: *const (),
}

impl<'a> VComponent<'a> {
    // use the type parameter on props creation and move it into a portable context
    // this lets us keep scope generic *and* downcast its props when we need to:
    // - perform comparisons when diffing (memoization)
    // TODO: lift the requirement that props need to be static
    // we want them to borrow references... maybe force implementing a "to_static_unsafe" trait

    pub fn new<P: Properties + 'a>(
        // bump: &'a Bump,
        ctx: &NodeCtx<'a>,
        component: FC<P>,
        // props: bumpalo::boxed::Box<'a, P>,
        props: P,
        key: Option<&'a str>,
        children: &'a [VNode<'a>],
    ) -> Self {
        // pub fn new<P: Properties + 'a>(component: FC<P>, props: P, key: Option<&'a str>) -> Self {
        // let bad_props = unsafe { transmogrify(props) };
        let bump = ctx.bump();
        let caller_ref = component as *const ();
        let props = bump.alloc(props);

        let raw_props = props as *const P as *const ();

        let comparator: Option<&dyn Fn(&VComponent) -> bool> = {
            if P::CAN_BE_MEMOIZED {
                Some(bump.alloc(move |other: &VComponent| {
                    // Safety:
                    // We are guaranteed that the props will be of the same type because
                    // there is no way to create a VComponent other than this `new` method.
                    //
                    // Therefore, if the render functions are identical (by address), then so will be
                    // props type paramter (because it is the same render function). Therefore, we can be
                    // sure
                    if caller_ref == other.user_fc {
                        // let g = other.raw_ctx.downcast_ref::<P>().unwrap();
                        let real_other = unsafe { &*(other.raw_props as *const _ as *const P) };
                        &props == &real_other
                    } else {
                        false
                    }
                }))
            } else {
                None
            }
        };

        // let prref: &'a P = props.as_ref();

        // let r = create_closure(component, raw_props);
        // let caller: Rc<dyn for<'g> Fn(&'g Scope) -> VNode<'g>> = Rc::new(move |scope| {
        //     // r(scope);
        //     //
        //     // let props2 = bad_props;
        //     // props.as_ref();
        //     // let ctx = Context {
        //     //     props: prref,
        //     //     scope,
        //     // };
        //     // let ctx: Context<'g, P> = todo!();
        //     // todo!()
        //     // let r = component(ctx);
        //     todo!()
        // });
        let caller = create_closure(component, raw_props);

        // let caller: Rc<dyn Fn(&Scope) -> VNode> = Rc::new(create_closure(component, raw_props));

        let key = match key {
            Some(key) => NodeKey::new(key),
            None => NodeKey(None),
        };

        // raw_props: Box::new(props),
        // comparator: Rc::new(props_comparator),
        Self {
            key,
            ass_scope: RefCell::new(None),
            user_fc: caller_ref,
            comparator,
            raw_props,
            children,
            caller,
            mounted_root: Cell::new(RealDomNode::empty()),
        }
    }
}

type Captured<'a> = Rc<dyn for<'r> Fn(&'r Scope) -> VNode<'r> + 'a>;

fn create_closure<'a, P: Properties + 'a>(
    component: FC<P>,
    raw_props: *const (),
) -> Rc<dyn for<'r> Fn(&'r Scope) -> VNode<'r>> {
    // ) -> impl for<'r> Fn(&'r Scope) -> VNode<'r> {
    let g: Captured = Rc::new(move |scp: &Scope| -> VNode {
        // cast back into the right lifetime
        let safe_props: &'_ P = unsafe { &*(raw_props as *const P) };
        // let ctx: Context<P2> = todo!();
        let ctx: Context<P> = Context {
            props: safe_props,
            scope: scp,
        };

        let g = component(ctx);
        let g2 = unsafe { std::mem::transmute(g) };
        g2
    });
    let r: Captured<'static> = unsafe { std::mem::transmute(g) };
    r
}

pub struct VFragment<'src> {
    pub key: NodeKey<'src>,
    pub children: &'src [VNode<'src>],
}

impl<'a> VFragment<'a> {
    pub fn new(key: Option<&'a str>, children: &'a [VNode<'a>]) -> Self {
        let key = match key {
            Some(key) => NodeKey::new(key),
            None => NodeKey(None),
        };

        Self { key, children }
    }
}
