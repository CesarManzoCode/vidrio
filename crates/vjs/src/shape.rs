//! Shapes (hidden classes): the ordered list of named properties shared by
//! objects that added the same keys with the same attributes in the same
//! order. Slot `i` of an object holds the value of `keys[i]` of its shape.
//!
//! Transitions are cached in the parent and held weakly: the collector
//! drops the entries of shapes no object uses any more. Objects leave the
//! shape tree for a per-object dictionary on delete, on reconfiguration of
//! an existing property, or past [`MAX_SHAPE_PROPERTIES`] properties.

use std::collections::HashMap;
use std::mem::size_of;

use crate::gc::{Arena, Shape, Trace, Tracer};
use crate::object::Attributes;
use crate::value::PropertyKey;

/// Objects with more named properties than this switch to dictionary mode.
pub const MAX_SHAPE_PROPERTIES: usize = 64;

pub struct ShapeData {
    pub(crate) parent: Option<Shape>,
    pub(crate) keys: Vec<(PropertyKey, Attributes)>,
    pub(crate) lookup: HashMap<PropertyKey, u32>,
    pub(crate) transitions: HashMap<(PropertyKey, Attributes), Shape>,
}

impl ShapeData {
    pub(crate) fn root() -> ShapeData {
        ShapeData {
            parent: None,
            keys: Vec::new(),
            lookup: HashMap::new(),
            transitions: HashMap::new(),
        }
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        self.keys.len()
            * (size_of::<(PropertyKey, Attributes)>() + 2 * size_of::<(PropertyKey, u32)>())
            + self.transitions.len() * 2 * size_of::<((PropertyKey, Attributes), Shape)>()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn parent(&self) -> Option<Shape> {
        self.parent
    }

    /// Slot and attributes of `key`.
    pub fn find(&self, key: PropertyKey) -> Option<(usize, Attributes)> {
        self.lookup
            .get(&key)
            .map(|&i| (i as usize, self.keys[i as usize].1))
    }

    pub fn keys(&self) -> &[(PropertyKey, Attributes)] {
        &self.keys
    }
}

impl Trace for ShapeData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(p) = self.parent {
            tracer.shape(p);
        }
        for (key, _) in &self.keys {
            tracer.key(*key);
        }
    }
}

/// The shape reached from `shape` by adding `key` with `attrs`, creating
/// and caching it if needed. Returns the shape and the bytes allocated.
pub(crate) fn shape_with(
    shapes: &mut Arena<ShapeData>,
    shape: Shape,
    key: PropertyKey,
    attrs: Attributes,
) -> (Shape, usize) {
    if let Some(&child) = shapes.get(shape).transitions.get(&(key, attrs)) {
        return (child, 0);
    }
    let parent = shapes.get(shape);
    let mut keys = parent.keys.clone();
    let mut lookup = parent.lookup.clone();
    lookup.insert(key, keys.len() as u32);
    keys.push((key, attrs));
    let bytes = size_of::<ShapeData>() + keys.len() * size_of::<(PropertyKey, Attributes)>() * 3;
    let child = shapes.alloc(ShapeData {
        parent: Some(shape),
        keys,
        lookup,
        transitions: HashMap::new(),
    });
    shapes
        .get_mut(shape)
        .transitions
        .insert((key, attrs), child);
    (child, bytes)
}
