/*!
    Property binding engine.

    The current implementation uses lots of heap allocation but that can be optimized later using
    thin dst container, and intrusive linked list
*/

use crate::abi::datastructures::ComponentRef;
use core::cell::*;
use core::ops::DerefMut;
use std::rc::{Rc, Weak};

thread_local!(static CURRENT_BINDING : RefCell<Option<Rc<dyn PropertyNotify>>> = Default::default());

trait Binding<T> {
    fn evaluate(self: Rc<Self>, value: &mut T, context: &EvaluationContext);
    /// When a new value is set on a property that has a binding, this function returns false
    /// if the binding wants to remain active. By default bindings are replaced when
    /// a new value is set on a property.
    fn allow_replace_binding_with_value(self: Rc<Self>, _value: &T) -> bool {
        return true;
    }

    /// When a new binding is set on a property that has a binding, this function returns false
    /// if the binding wants to remain active. By default bindings are replaced when a
    /// new binding is set a on property.
    fn allow_replace_binding_with_binding(self: Rc<Self>, _binding: Rc<dyn Binding<T>>) -> bool {
        return true;
    }

    /// This function is used to notify the binding that one of the dependencies was changed
    /// and therefore this binding may evaluate to a different value, too.
    fn mark_dirty(self: Rc<Self>, _reason: DirtyReason) {}

    fn set_notify_callback(self: Rc<Self>, _callback: Rc<dyn PropertyNotify>) {}
}

#[derive(Default)]
struct PropertyImpl<T> {
    /// Invariant: Must only be called with a pointer to the binding
    binding: Option<Rc<dyn Binding<T>>>,
    dependencies: Vec<Weak<dyn PropertyNotify>>,
    dirty: bool,
    //updating: bool,
}

/// DirtyReason is used to convey to a dependency the reason for the request to
/// mark itself as dirty.
enum DirtyReason {
    /// The dependency shall be considered dirty because a property's value or
    /// subsequent dependency has changed.
    ValueOrDependencyHasChanged,
}

/// PropertyNotify is the interface that allows keeping track of dependencies between
/// property bindings.
trait PropertyNotify {
    /// mark_dirty() is called to notify a property that its binding may need to be re-evaluated
    /// because one of its dependencies may have changed.
    fn mark_dirty(self: Rc<Self>, reason: DirtyReason);
    /// notify() is called to register the currently (thread-local) evaluating binding as a
    /// dependency for this property (self).
    fn register_current_binding_as_dependency(self: Rc<Self>);
}

impl<T> PropertyNotify for RefCell<PropertyImpl<T>> {
    fn mark_dirty(self: Rc<Self>, reason: DirtyReason) {
        let mut v = vec![];
        {
            let mut dep = self.borrow_mut();
            dep.dirty = true;
            if let Some(binding) = &dep.binding {
                binding.clone().mark_dirty(reason);
            }
            std::mem::swap(&mut dep.dependencies, &mut v);
        }
        for d in &v {
            if let Some(d) = d.upgrade() {
                d.mark_dirty(DirtyReason::ValueOrDependencyHasChanged);
            }
        }
    }

    fn register_current_binding_as_dependency(self: Rc<Self>) {
        CURRENT_BINDING.with(|cur_dep| {
            if let Some(m) = &(*cur_dep.borrow()) {
                self.borrow_mut().dependencies.push(Rc::downgrade(m));
            }
        });
    }
}

/// This structure contains what is required for the property engine to evaluate properties
///
/// One must pass it to the getter of the property, or emit of signals, and it can
/// be accessed from the bindings
#[repr(C)]
pub struct EvaluationContext<'a> {
    /// The component which contains the Property or the Signal
    pub component: vtable::VRef<'a, crate::abi::datastructures::ComponentVTable>,

    /// The context of the parent component
    pub parent_context: Option<&'a EvaluationContext<'a>>,
}

impl<'a> EvaluationContext<'a> {
    /// Create a new context related to the root component
    ///
    /// The component need to be a root component, otherwise fetching properties
    /// might panic.
    pub fn for_root_component(component: ComponentRef<'a>) -> Self {
        Self { component, parent_context: None }
    }

    /// Create a context for a child component of a component within the current
    /// context.
    pub fn child_context(&'a self, child: ComponentRef<'a>) -> Self {
        Self { component: child, parent_context: Some(self) }
    }
}

type PropertyHandle<T> = Rc<RefCell<PropertyImpl<T>>>;
/// A Property that allow binding that track changes
///
/// Property van have be assigned value, or bindings.
/// When a binding is assigned, it is lazily evaluated on demand
/// when calling `get()`.
/// When accessing another property from a binding evaluation,
/// a dependency will be registered, such that when the property
/// change, the binding will automatically be updated
#[repr(C)]
#[derive(Default)]
pub struct Property<T: 'static> {
    inner: PropertyHandle<T>,
    /// Only access when holding a lock of the inner refcell.
    /// (so only through Property::borrow and Property::try_borrow_mut)
    value: UnsafeCell<T>,
}

impl<T> Property<T> {
    /// Borrow both `inner` and `value`
    fn try_borrow(&self) -> Result<(Ref<PropertyImpl<T>>, Ref<T>), BorrowError> {
        let lock = self.inner.try_borrow()?;
        // Safety: we use the same locking rules for `inner` and `value`
        Ok(Ref::map_split(lock, |r| unsafe { (r, &*self.value.get()) }))
    }

    /// Borrow both `inner` and `value` as mutable
    fn try_borrow_mut(&self) -> Result<(RefMut<PropertyImpl<T>>, RefMut<T>), BorrowMutError> {
        let lock = self.inner.try_borrow_mut()?;
        // Safety: we use the same locking rules for `inner` and `value`
        Ok(RefMut::map_split(lock, |r| unsafe { (r, &mut *self.value.get()) }))
    }
}

impl<T: Clone + 'static> Property<T> {
    /// Get the value of the property
    ///
    /// This may evaluate the binding if there is a binding and it is dirty
    ///
    /// If the function is called directly or indirectly from a binding evaluation
    /// of another Property, a dependency will be registered.
    ///
    /// The context must be the constext matching the Component which contains this
    /// property
    pub fn get(&self, context: &EvaluationContext) -> T {
        self.update(context);
        self.inner.clone().register_current_binding_as_dependency();
        self.try_borrow().expect("Binding loop detected").1.clone()
    }

    /// Change the value of this property
    ///
    /// If other properties have binding depending of this property, these properties will
    /// be marked as dirty.
    pub fn set(&self, t: T) {
        {
            let maybe_binding = self.inner.borrow().binding.as_ref().map(|binding| binding.clone());
            if let Some(existing_binding) = maybe_binding {
                if !existing_binding.allow_replace_binding_with_value(&t) {
                    return;
                }
            }
            let (mut lock, mut value) = self.try_borrow_mut().expect("Binding loop detected");
            lock.binding = None;
            lock.dirty = false;
            *value = t;
        }
        self.inner.clone().mark_dirty(DirtyReason::ValueOrDependencyHasChanged);
        self.inner.borrow_mut().dirty = false;
    }

    /// Set a binding to this property.
    ///
    /// Bindings are evaluated lazily from calling get, and the return value of the binding
    /// is the new value.
    ///
    /// If other properties have bindings depending of this property, these properties will
    /// be marked as dirty.
    pub fn set_binding(&self, f: impl (Fn(&EvaluationContext) -> T) + 'static) {
        struct BindingFunction<F> {
            function: F,
        }

        impl<T, F: Fn(&mut T, &EvaluationContext)> Binding<T> for BindingFunction<F> {
            fn evaluate(self: Rc<Self>, value_ptr: &mut T, context: &EvaluationContext) {
                (self.function)(value_ptr, context)
            }
        }

        let real_binding = move |ptr: &mut T, context: &EvaluationContext| *ptr = f(context);

        let binding_object = Rc::new(BindingFunction { function: real_binding });

        let maybe_binding = self.inner.borrow().binding.as_ref().map(|binding| binding.clone());
        if let Some(existing_binding) = maybe_binding {
            if !existing_binding.allow_replace_binding_with_binding(binding_object.clone()) {
                return;
            }
        }

        self.set_binding_object(binding_object);
    }

    /// Set a binding object to this property.
    ///
    /// Bindings are evaluated lazily from calling get, and the return value of the binding
    /// is the new value.
    ///
    /// If other properties have bindings depending of this property, these properties will
    /// be marked as dirty.
    fn set_binding_object(&self, binding_object: Rc<dyn Binding<T>>) -> Option<Rc<dyn Binding<T>>> {
        binding_object.clone().set_notify_callback(self.inner.clone());
        let old_binding =
            std::mem::replace(&mut self.inner.borrow_mut().binding, Some(binding_object));
        self.inner.clone().mark_dirty(DirtyReason::ValueOrDependencyHasChanged);
        old_binding
    }

    /// Call the binding if the property is dirty to update the stored value
    fn update(&self, context: &EvaluationContext) {
        if !self.inner.borrow().dirty {
            return;
        }
        let mut old: Option<Rc<dyn PropertyNotify>> = Some(self.inner.clone());
        let (mut lock, mut value) =
            self.try_borrow_mut().expect("Circular dependency in binding evaluation");
        if let Some(binding) = &lock.binding {
            CURRENT_BINDING.with(|cur_dep| {
                let mut m = cur_dep.borrow_mut();
                std::mem::swap(m.deref_mut(), &mut old);
            });
            binding.clone().evaluate(value.deref_mut(), context);
            lock.dirty = false;
            CURRENT_BINDING.with(|cur_dep| {
                let mut m = cur_dep.borrow_mut();
                std::mem::swap(m.deref_mut(), &mut old);
                //somehow ptr_eq does not work as expected despite the pointer are equal
                //debug_assert!(Rc::ptr_eq(&(self.inner.clone() as Rc<dyn PropertyNotify>), &old.unwrap()));
            });
        }
    }
}

#[test]
fn properties_simple_test() {
    #[derive(Default)]
    struct Component {
        width: Property<i32>,
        height: Property<i32>,
        area: Property<i32>,
    }
    let dummy_eval_context = EvaluationContext::for_root_component(unsafe {
        vtable::VRef::from_raw(core::ptr::NonNull::dangling(), core::ptr::NonNull::dangling())
    });
    let compo = Rc::new(Component::default());
    let w = Rc::downgrade(&compo);
    compo.area.set_binding(move |ctx| {
        let compo = w.upgrade().unwrap();
        compo.width.get(ctx) * compo.height.get(ctx)
    });
    compo.width.set(4);
    compo.height.set(8);
    assert_eq!(compo.width.get(&dummy_eval_context), 4);
    assert_eq!(compo.height.get(&dummy_eval_context), 8);
    assert_eq!(compo.area.get(&dummy_eval_context), 4 * 8);

    let w = Rc::downgrade(&compo);
    compo.width.set_binding(move |ctx| {
        let compo = w.upgrade().unwrap();
        compo.height.get(ctx) * 2
    });
    assert_eq!(compo.width.get(&dummy_eval_context), 8 * 2);
    assert_eq!(compo.height.get(&dummy_eval_context), 8);
    assert_eq!(compo.area.get(&dummy_eval_context), 8 * 8 * 2);
}

#[allow(non_camel_case_types)]
type c_void = ();
#[repr(C)]
/// Has the same layout as PropertyHandle
pub struct PropertyHandleOpaque(*const c_void);

/// Initialize the first pointer of the Property. Does not initialize the content
#[no_mangle]
pub unsafe extern "C" fn sixtyfps_property_init(out: *mut PropertyHandleOpaque) {
    assert_eq!(
        core::mem::size_of::<PropertyHandle<()>>(),
        core::mem::size_of::<PropertyHandleOpaque>()
    );
    // This assume that PropertyHandle<()> has the same layout as PropertyHandle<T> ∀T
    core::ptr::write(out as *mut PropertyHandle<()>, PropertyHandle::default());
}

/// To be called before accessing the value
///
/// (same as Property::update and PopertyImpl::notify)
#[no_mangle]
pub unsafe extern "C" fn sixtyfps_property_update(
    out: *const PropertyHandleOpaque,
    context: *const EvaluationContext,
    val: *mut c_void,
) {
    let inner = &*(out as *const PropertyHandle<()>);

    if !inner.borrow().dirty {
        inner.clone().register_current_binding_as_dependency();
        return;
    }
    let mut old: Option<Rc<dyn PropertyNotify>> = Some(inner.clone());
    let mut lock = inner.try_borrow_mut().expect("Circular dependency in binding evaluation");
    if let Some(binding) = &lock.binding {
        CURRENT_BINDING.with(|cur_dep| {
            let mut m = cur_dep.borrow_mut();
            std::mem::swap(m.deref_mut(), &mut old);
        });
        binding.clone().evaluate(&mut *val, &*context);
        lock.dirty = false;
        CURRENT_BINDING.with(|cur_dep| {
            let mut m = cur_dep.borrow_mut();
            std::mem::swap(m.deref_mut(), &mut old);
            //somehow ptr_eq does not work as expected despite the pointer are equal
            //debug_assert!(Rc::ptr_eq(&(inner.clone() as Rc<dyn PropertyNotify>), &old.unwrap()));
        });
    }
    core::mem::drop(lock);
    inner.clone().register_current_binding_as_dependency();
}

/// Mark the fact that the property was changed and that its binding need to be removed, and
/// The dependencies marked dirty
#[no_mangle]
pub unsafe extern "C" fn sixtyfps_property_set_changed(out: *const PropertyHandleOpaque) {
    let inner = &*(out as *const PropertyHandle<()>);
    inner.clone().mark_dirty(DirtyReason::ValueOrDependencyHasChanged);
    inner.borrow_mut().dirty = false;
    inner.borrow_mut().binding = None;
}

/// Set a binding
/// The binding has signature fn(user_data, context, pointer_to_value)
///
/// The current implementation will do usually two memory alocation:
///  1. the allocation from the calling code to allocate user_data
///  2. the box allocation within this binding
/// It might be possible to reduce that by passing something with a
/// vtable, so there is the need for less memory allocation.
#[no_mangle]
pub unsafe extern "C" fn sixtyfps_property_set_binding(
    out: *const PropertyHandleOpaque,
    binding: extern "C" fn(*mut c_void, &EvaluationContext, *mut c_void),
    user_data: *mut c_void,
    drop_user_data: Option<extern "C" fn(*mut c_void)>,
) {
    let inner = &*(out as *const PropertyHandle<()>);

    struct CFunctionBinding {
        binding_function: extern "C" fn(*mut c_void, &EvaluationContext, *mut c_void),
        user_data: *mut c_void,
        drop_user_data: Option<extern "C" fn(*mut c_void)>,
    }

    impl Drop for CFunctionBinding {
        fn drop(&mut self) {
            if let Some(x) = self.drop_user_data {
                x(self.user_data)
            }
        }
    }

    impl Binding<()> for CFunctionBinding {
        fn evaluate(self: Rc<Self>, value_ptr: &mut (), context: &EvaluationContext) {
            (self.binding_function)(self.user_data, context, value_ptr);
        }
    }

    let binding =
        Rc::new(CFunctionBinding { binding_function: binding, user_data, drop_user_data });

    inner.borrow_mut().binding = Some(binding);
    inner.clone().mark_dirty(DirtyReason::ValueOrDependencyHasChanged);
}

/// Destroy handle
#[no_mangle]
pub unsafe extern "C" fn sixtyfps_property_drop(handle: *mut PropertyHandleOpaque) {
    core::ptr::read(handle as *mut PropertyHandle<()>);
}
