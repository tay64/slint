/* LICENSE BEGIN
    This file is part of the SixtyFPS Project -- https://sixtyfps.io
    Copyright (c) 2020 Olivier Goffart <olivier.goffart@sixtyfps.io>
    Copyright (c) 2020 Simon Hausmann <simon.hausmann@sixtyfps.io>

    SPDX-License-Identifier: GPL-3.0-only
    This file is also available under commercial licensing terms.
    Please contact info@sixtyfps.io for more information.
LICENSE END */
use core::cell::RefCell;
use core::pin::Pin;
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

use sixtyfps_corelib::{
    component::ComponentRefPin, item_tree::VisitChildrenResult, items::ItemRef, Property,
};

type ModelPeerInner = dyn ViewAbstraction;

/// Represent a handle to a view that listen to change to a model. See [`Model::attach_peer`] and [`ModelNotify`]
pub struct ModelPeer {
    inner: Weak<RefCell<ModelPeerInner>>,
}

/// Dispatch notification from a [`Model`] to one or several [`ModelPeer`].
/// Typically, you would want to put this in the implementaiton of the Model
#[derive(Default)]
pub struct ModelNotify {
    inner: RefCell<weak_table::PtrWeakHashSet<Weak<RefCell<ModelPeerInner>>>>,
}

impl ModelNotify {
    /// Notify the peers that a specific row was changed
    pub fn row_changed(&self, row: usize) {
        for peer in self.inner.borrow().iter() {
            peer.borrow_mut().row_changed(row)
        }
    }
    /// Notify the peers that rows were added
    pub fn row_added(&self, index: usize, count: usize) {
        for peer in self.inner.borrow().iter() {
            peer.borrow_mut().row_added(index, count)
        }
    }
    /// Notify the peers that rows were removed
    pub fn row_removed(&self, index: usize, count: usize) {
        for peer in self.inner.borrow().iter() {
            peer.borrow_mut().row_removed(index, count)
        }
    }
    /// Attach one peer. The peer will be notified when the model changes
    pub fn attach(&self, peer: ModelPeer) {
        peer.inner.upgrade().map(|rc| self.inner.borrow_mut().insert(rc));
    }
}

/// A Model is providing Data for the Repeater or ListView elements of the `.60` language
pub trait Model {
    /// The model data: A model is a set of row and each row has this data
    type Data;
    /// The amount of row in the model
    fn row_count(&self) -> usize;
    /// Returns the data for a particular row. This function should be called with `row < row_count()`.
    fn row_data(&self, row: usize) -> Self::Data;
    /// Sets the data for a particular row. This function should be called with `row < row_count()`.
    /// If the model cannot support data changes, then it is ok to do nothing (default implementation).
    /// If the model can update the data, it should also call row_changed on its internal `ModelNotify`.
    fn set_row_data(&self, _row: usize, _data: Self::Data) {}
    /// Should forward to the internal [`ModelNotify::attach`]
    fn attach_peer(&self, peer: ModelPeer);
}

/// A model backed by an SharedArray
#[derive(Default)]
pub struct VecModel<T> {
    array: RefCell<Vec<T>>,
    notify: ModelNotify,
}

impl<T: 'static> VecModel<T> {
    /// Allocate a new model from a slice
    pub fn from_slice(slice: &[T]) -> ModelHandle<T>
    where
        T: Clone,
    {
        Some(Rc::<Self>::new(slice.iter().cloned().collect::<Vec<T>>().into()))
    }

    /// Add a row at the end of the model
    pub fn push(&self, value: T) {
        self.array.borrow_mut().push(value);
        self.notify.row_added(self.array.borrow().len() - 1, 1)
    }

    /// Remove the row at the given index from the model
    pub fn remove(&self, index: usize) {
        self.array.borrow_mut().remove(index);
        self.notify.row_removed(index, 1)
    }
}

impl<T> From<Vec<T>> for VecModel<T> {
    fn from(array: Vec<T>) -> Self {
        VecModel { array: RefCell::new(array), notify: Default::default() }
    }
}

impl<T: Clone> Model for VecModel<T> {
    type Data = T;

    fn row_count(&self) -> usize {
        self.array.borrow().len()
    }

    fn row_data(&self, row: usize) -> Self::Data {
        self.array.borrow()[row].clone()
    }

    fn set_row_data(&self, row: usize, data: Self::Data) {
        self.array.borrow_mut()[row] = data;
        self.notify.row_changed(row);
    }

    fn attach_peer(&self, peer: ModelPeer) {
        self.notify.attach(peer);
    }
}

impl Model for usize {
    type Data = i32;

    fn row_count(&self) -> usize {
        *self
    }

    fn row_data(&self, row: usize) -> Self::Data {
        row as i32
    }

    fn attach_peer(&self, _peer: ModelPeer) {
        // The model is read_only: nothing to do
    }
}

impl Model for bool {
    type Data = ();

    fn row_count(&self) -> usize {
        if *self {
            1
        } else {
            0
        }
    }

    fn row_data(&self, _row: usize) -> Self::Data {}

    fn attach_peer(&self, _peer: ModelPeer) {
        // The model is read_only: nothing to do
    }
}

/// Properties of type array in the .60 language are represented as
/// an [Option] of an [Rc] of somthing implemented the [Model] trait
pub type ModelHandle<T> = Option<Rc<dyn Model<Data = T>>>;

/// Component that can be instantiated by a repeater.
pub trait RepeatedComponent: sixtyfps_corelib::component::Component {
    /// The data corresponding to the model
    type Data: 'static;

    /// Update this component at the given index and the given data
    fn update(&self, index: usize, data: Self::Data);
}

#[derive(Clone, Copy, PartialEq)]
enum RepeatedComponentState {
    /// The item is in a clean state
    Clean,
    /// The model data is stale and needs to be refreshed
    Dirty,
}
struct RepeaterInner<C: RepeatedComponent> {
    components: Vec<(RepeatedComponentState, Option<Pin<Rc<C>>>)>,
    is_dirty: bool,
    /// The model row (index) of the first component in the `components` vector.
    /// Only used for ListView
    offset: usize,
}

impl<C: RepeatedComponent> Default for RepeaterInner<C> {
    fn default() -> Self {
        RepeaterInner { components: Default::default(), is_dirty: true, offset: 0 }
    }
}

impl<C: RepeatedComponent> Clone for RepeaterInner<C> {
    fn clone(&self) -> Self {
        panic!("Clone is there so we can make_mut the RepeaterInner, to dissociate the weaks, but there should only be one inner")
    }
}

trait ViewAbstraction {
    fn row_changed(&mut self, row: usize);
    fn row_added(&mut self, index: usize, count: usize);
    fn row_removed(&mut self, index: usize, count: usize);
}

impl<C: RepeatedComponent> ViewAbstraction for RepeaterInner<C> {
    /// Notify the peers that a specific row was changed
    fn row_changed(&mut self, row: usize) {
        self.is_dirty = true;
        if let Some(c) = self.components.get_mut(row.wrapping_sub(self.offset)) {
            c.0 = RepeatedComponentState::Dirty;
        }
    }
    /// Notify the peers that rows were added
    fn row_added(&mut self, mut index: usize, mut count: usize) {
        if index < self.offset {
            if index + count < self.offset {
                return;
            }
            count -= self.offset - index;
            index = 0;
        } else {
            index -= self.offset;
        }
        if count == 0 {
            return;
        }
        self.is_dirty = true;
        self.components.splice(
            index..index,
            core::iter::repeat((RepeatedComponentState::Dirty, None)).take(count),
        );
    }
    /// Notify the peers that rows were removed
    fn row_removed(&mut self, mut index: usize, mut count: usize) {
        if index < self.offset {
            if index + count < self.offset {
                return;
            }
            count -= self.offset - index;
            index = 0;
        } else {
            index -= self.offset;
        }
        if count == 0 {
            return;
        }
        self.is_dirty = true;
        self.components.drain(index..(index + count));
        for c in self.components[index..].iter_mut() {
            // Because all the indexes are dirty
            c.0 = RepeatedComponentState::Dirty;
        }
    }
}

/// This field is put in a component when using the `for` syntax
/// It helps instantiating the components `C`
pub struct Repeater<C: RepeatedComponent> {
    /// The Rc is shared between ModelPeer. The outer RefCell make it possible to re-initialize a new Rc when
    /// The model is changed. The inner RefCell make it possible to change the RepeaterInner when shared
    inner: RefCell<Rc<RefCell<RepeaterInner<C>>>>,
    model: Property<ModelHandle<C::Data>>,
    /// Only used for the list view to track if the scrollbar has changed and item needs to be re_layouted
    listview_geometry_tracker: sixtyfps_corelib::properties::PropertyTracker,
}

impl<C: RepeatedComponent> Default for Repeater<C> {
    fn default() -> Self {
        Repeater {
            inner: Default::default(),
            model: Default::default(),
            listview_geometry_tracker: Default::default(),
        }
    }
}

impl<C: RepeatedComponent + 'static> Repeater<C> {
    /// Set the model binding
    pub fn set_model_binding(&self, binding: impl Fn() -> ModelHandle<C::Data> + 'static) {
        self.model.set_binding(binding);
    }

    fn model(self: Pin<&Self>) -> ModelHandle<C::Data> {
        // Safety: Repeater does not implement drop and never let access model as mutable
        #[allow(unsafe_code)]
        let model = unsafe { self.map_unchecked(|s| &s.model) };

        if model.is_dirty() {
            // Invalidate previuos weeks on the previous models
            (*Rc::make_mut(&mut self.inner.borrow_mut()).get_mut()) = RepeaterInner::default();
            if let Some(m) = model.get() {
                let peer: Rc<RefCell<dyn ViewAbstraction>> = self.inner.borrow().clone();
                m.attach_peer(ModelPeer { inner: Rc::downgrade(&peer) });
            }
        }
        model.get()
    }

    /// Call this function to make sure that the model is updated.
    /// The init function is the function to create a component
    pub fn ensure_updated(self: Pin<&Self>, init: impl Fn() -> Pin<Rc<C>>) {
        if let Some(model) = self.model() {
            if self.inner.borrow().borrow().is_dirty {
                self.ensure_updated_impl(init, &model, model.row_count())
            }
        } else {
            self.inner.borrow().borrow_mut().components.clear();
        }
    }
    fn ensure_updated_impl(
        self: Pin<&Self>,
        init: impl Fn() -> Pin<Rc<C>>,
        model: &Rc<dyn Model<Data = C::Data>>,
        count: usize,
    ) {
        let inner = self.inner.borrow();
        let mut inner = inner.borrow_mut();
        inner.components.resize_with(count, || (RepeatedComponentState::Dirty, None));
        let offset = inner.offset;
        for (i, c) in inner.components.iter_mut().enumerate() {
            if c.0 == RepeatedComponentState::Dirty {
                if c.1.is_none() {
                    c.1 = Some(init());
                }
                c.1.as_ref().unwrap().update(i + offset, model.row_data(i + offset));
                c.0 = RepeatedComponentState::Clean;
            }
        }
        inner.is_dirty = false;
    }

    /// Same as `Self::ensuer_updated` but for a ListView
    pub fn ensure_updated_listview(
        self: Pin<&Self>,
        init: impl Fn() -> Pin<Rc<C>>,
        viewport_height: Pin<&Property<f32>>,
        viewport_y: Pin<&Property<f32>>,
        listview_height: f32,
    ) {
        let empty_model = || {
            self.inner.borrow().borrow_mut().components.clear();
            viewport_height.set(0.);
            viewport_y.set(0.);
        };

        let model = if let Some(model) = self.model() {
            model
        } else {
            return empty_model();
        };
        let row_count = model.row_count();
        if row_count == 0 {
            return empty_model();
        }

        #[allow(unsafe_code)]
        // Safety: Repeater does not implement drop and never let access model as mutable
        let listview_geometry_tracker =
            unsafe { self.map_unchecked(|s| &s.listview_geometry_tracker) };
        listview_geometry_tracker.evaluate_if_dirty(||{
            // Compute the element height
            let total_height = Cell::new(0.);
            let count = Cell::new(0);

            let mut get_height_visitor = |_: ComponentRefPin, _: isize, item: Pin<ItemRef>| -> VisitChildrenResult {
                count.set(count.get() + 1);
                total_height.set( total_height.get() + item.as_ref().geometry().height());

                VisitChildrenResult::abort(0, 0)
            };
            vtable::new_vref!(
                let mut get_height_visitor: VRefMut<sixtyfps_corelib::item_tree::ItemVisitorVTable> for sixtyfps_corelib::item_tree::ItemVisitor
                     = &mut get_height_visitor
            );

            for c in self.inner.borrow().borrow().components.iter() {
                c.1.as_ref().map(|x| {
                    x.as_ref().compute_layout();
                    x.as_ref().visit_children_item(-1, sixtyfps_corelib::item_tree::TraversalOrder::FrontToBack, get_height_visitor.borrow_mut());
                });
            }

            let element_height = if count.get() > 0 {
                total_height.get() / (count.get() as f32)
            } else {
                // There seems to be currently no items. Just instentiate one item.

                let inner = self.inner.borrow();
                let mut inner = inner.borrow_mut();
                inner.offset = inner.offset.min(row_count - 1);

                self.ensure_updated_impl(&init, &model, 1);
                if let Some(c) = inner.components.get(0) {
                    c.1.as_ref().map(|x| {
                        x.as_ref().compute_layout();
                        x.as_ref().visit_children_item(-1, sixtyfps_corelib::item_tree::TraversalOrder::FrontToBack, get_height_visitor);
                    });
                } else {
                    panic!("Could not determine size of items");
                }
                total_height.get()
            };

            viewport_height.set(element_height * model.row_count() as f32);
            self.set_offset((-viewport_y.get() / element_height).floor() as usize, (listview_height / element_height).ceil() as usize);
        });
        if self.inner.borrow().borrow().is_dirty {
            let count = self.inner.borrow().borrow().components.len();
            self.ensure_updated_impl(init, &model, count);
        }
    }

    fn set_offset(&self, offset: usize, count: usize) {
        let inner = self.inner.borrow();
        let mut inner = inner.borrow_mut();
        let old_offset = inner.offset;
        // Remove the items before the offset, or add items until the old offset
        inner.components.splice(
            0..(offset.saturating_sub(old_offset)),
            core::iter::repeat((RepeatedComponentState::Dirty, None))
                .take(old_offset.saturating_sub(offset)),
        );
        inner.components.resize_with(count, || (RepeatedComponentState::Dirty, None));
        inner.offset = offset;
    }

    /// Call the visitor for each component
    pub fn visit(
        &self,
        order: sixtyfps_corelib::item_tree::TraversalOrder,
        mut visitor: sixtyfps_corelib::item_tree::ItemVisitorRefMut,
    ) -> sixtyfps_corelib::item_tree::VisitChildrenResult {
        // We can't keep self.inner borrowed because the event might modify the model
        let count = self.inner.borrow().borrow().components.len();
        for i in 0..count {
            let c = self.inner.borrow().borrow().components.get(i).and_then(|c| c.1.clone());
            if let Some(c) = c {
                if c.as_ref().visit_children_item(-1, order, visitor.borrow_mut()).has_aborted() {
                    return sixtyfps_corelib::item_tree::VisitChildrenResult::abort(i, 0);
                }
            }
        }
        sixtyfps_corelib::item_tree::VisitChildrenResult::CONTINUE
    }

    /// Forward an input event to a particular item
    pub fn input_event(
        &self,
        idx: usize,
        event: sixtyfps_corelib::input::MouseEvent,
        window: &sixtyfps_corelib::eventloop::ComponentWindow,
        app_component: &ComponentRefPin,
    ) -> sixtyfps_corelib::input::InputEventResult {
        let c = self.inner.borrow().borrow().components[idx].1.clone();
        c.map_or(Default::default(), |c| c.as_ref().input_event(event, window, app_component))
    }

    /// Forward a key event to a particular item
    pub fn key_event(
        &self,
        idx: usize,
        event: &sixtyfps_corelib::input::KeyEvent,
        window: &sixtyfps_corelib::eventloop::ComponentWindow,
    ) -> sixtyfps_corelib::input::KeyEventResult {
        let c = self.inner.borrow().borrow().components[idx].1.clone();
        c.map_or(sixtyfps_corelib::input::KeyEventResult::EventIgnored, |c| {
            c.as_ref().key_event(event, window)
        })
    }

    /// Forward a focus event to a particular item
    pub fn focus_event(
        &self,
        idx: usize,
        event: &sixtyfps_corelib::input::FocusEvent,
        window: &sixtyfps_corelib::eventloop::ComponentWindow,
    ) -> sixtyfps_corelib::input::FocusEventResult {
        let c = self.inner.borrow().borrow().components[idx].1.clone();
        c.map_or(sixtyfps_corelib::input::FocusEventResult::FocusItemNotFound, |c| {
            c.as_ref().focus_event(event, window)
        })
    }

    /// Return the amount of item currently in the component
    pub fn len(&self) -> usize {
        self.inner.borrow().borrow().components.len()
    }

    /// Returns a vector containing all components
    pub fn components_vec(&self) -> Vec<Pin<Rc<C>>> {
        self.inner.borrow().borrow().components.iter().flat_map(|x| x.1.clone()).collect()
    }

    /// Recompute the layout of each child elements
    pub fn compute_layout(&self) {
        for c in self.inner.borrow().borrow().components.iter() {
            c.1.as_ref().map(|x| x.as_ref().compute_layout());
        }
    }

    /// Sets the data directly in the model
    pub fn model_set_row_data(self: Pin<&Self>, row: usize, data: C::Data) {
        if let Some(model) = self.model() {
            model.set_row_data(row, data);
            if let Some(c) = self.inner.borrow().borrow_mut().components.get_mut(row) {
                if c.0 == RepeatedComponentState::Dirty {
                    if let Some(comp) = c.1.as_ref() {
                        comp.update(row, model.row_data(row));
                        c.0 = RepeatedComponentState::Clean;
                    }
                }
            }
        }
    }
}
